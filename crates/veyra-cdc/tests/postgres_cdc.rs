use std::path::Path;
use std::time::Duration;

use pgwire_replication::{ReplicationConfig, TlsConfig};
use tempfile::tempdir;
use tokio::time::timeout;
use tokio_postgres::{Client, NoTls};
use veyra_cdc::{
    BatchLimits, CdcAssembler, CdcProgressTracker, CdcPump, CdcPumpEvent, DurableCdcLog,
    PostgresReplicationStream, SnapshotBoundary, TransactionBatch, TransactionItem,
    begin_consistent_snapshot,
};
use veyra_types::LogSequenceNumber;

const SLOT: &str = "veyra_ci_slot";
const PUBLICATION: &str = "veyra_ci_publication";

type TestError = Box<dyn std::error::Error>;

async fn connect(url: &str) -> Result<(Client, tokio::task::JoinHandle<()>), TestError> {
    let (client, connection) = tokio_postgres::connect(url, NoTls).await?;
    let task = tokio::spawn(async move {
        let _ = connection.await;
    });
    Ok((client, task))
}

fn replication_config() -> ReplicationConfig {
    ReplicationConfig::new(
        "127.0.0.1",
        "postgres",
        "postgres",
        "veyra",
        SLOT,
        PUBLICATION,
    )
    .with_tls(TlsConfig::disabled())
    .with_status_interval(Duration::from_millis(50))
    .with_wakeup_interval(Duration::from_millis(10))
}

async fn next_transaction(pump: &mut CdcPump) -> Result<TransactionBatch, TestError> {
    let event = timeout(Duration::from_secs(10), pump.next_durable()).await??;
    match event {
        CdcPumpEvent::Transaction { batch, .. } => Ok(batch),
        CdcPumpEvent::StoppedAt(_) | CdcPumpEvent::EndOfStream => {
            Err("replication ended before a committed transaction".into())
        }
    }
}

fn batch_contains_marker(batch: &TransactionBatch, marker: &[u8]) -> bool {
    batch.items().iter().any(|item| match item {
        TransactionItem::Wal(chunk) => chunk
            .data()
            .windows(marker.len())
            .any(|window| window == marker),
        TransactionItem::Message(message) => message
            .content()
            .windows(marker.len())
            .any(|window| window == marker),
    })
}

async fn prepare_fixture(control: &Client) -> Result<(), TestError> {
    control
        .batch_execute(
            "DROP PUBLICATION IF EXISTS veyra_ci_publication; \
             DROP TABLE IF EXISTS veyra_cdc_fixture; \
             CREATE TABLE veyra_cdc_fixture (id BIGSERIAL PRIMARY KEY, marker TEXT NOT NULL); \
             CREATE PUBLICATION veyra_ci_publication FOR TABLE veyra_cdc_fixture;",
        )
        .await?;
    control
        .execute(
            "SELECT pg_drop_replication_slot(slot_name) \
             FROM pg_replication_slots WHERE slot_name = $1",
            &[&SLOT],
        )
        .await?;
    control
        .query_one(
            "SELECT slot_name, lsn \
             FROM pg_create_logical_replication_slot($1, 'pgoutput')",
            &[&SLOT],
        )
        .await?;
    Ok(())
}

async fn establish_snapshot_overlap(
    control: &mut Client,
    writer: &Client,
) -> Result<SnapshotBoundary, TestError> {
    let snapshot = begin_consistent_snapshot(control, SLOT).await?;
    let count: i64 = snapshot
        .transaction()
        .query_one("SELECT count(*) FROM veyra_cdc_fixture", &[])
        .await?
        .get(0);
    assert_eq!(count, 0);

    writer
        .execute(
            "INSERT INTO veyra_cdc_fixture(marker) VALUES ('during-snapshot')",
            &[],
        )
        .await?;
    let snapshot_count: i64 = snapshot
        .transaction()
        .query_one("SELECT count(*) FROM veyra_cdc_fixture", &[])
        .await?
        .get(0);
    assert_eq!(snapshot_count, 0);

    let boundary = snapshot.finish().await?;
    assert!(boundary.snapshot_lsn() >= boundary.replay_from_lsn());
    assert!(!boundary.snapshot_id().is_empty());
    Ok(boundary)
}

async fn start_pump(
    log_path: &Path,
    start_lsn: LogSequenceNumber,
    progress: CdcProgressTracker,
) -> Result<CdcPump, TestError> {
    let (log, outcome) = DurableCdcLog::open(log_path, BatchLimits::default())?;
    assert!(outcome.last_durable_lsn <= start_lsn || start_lsn == LogSequenceNumber::ZERO);
    let stream = PostgresReplicationStream::connect(replication_config(), start_lsn).await?;
    Ok(CdcPump::new(
        stream,
        CdcAssembler::new(BatchLimits::default()),
        log,
        progress,
    ))
}

async fn consume_initial_snapshot_transaction(
    pump: &mut CdcPump,
) -> Result<LogSequenceNumber, TestError> {
    let first = next_transaction(pump).await?;
    assert!(batch_contains_marker(&first, b"during-snapshot"));
    let first_lsn = first.end_lsn();
    assert_eq!(pump.durable_lsn(), first_lsn);
    assert_eq!(pump.progress().snapshot().durable(), first_lsn);
    pump.shutdown().await?;
    Ok(first_lsn)
}

async fn consume_fresh_after_restart(
    pump: &mut CdcPump,
    first_lsn: LogSequenceNumber,
) -> Result<LogSequenceNumber, TestError> {
    loop {
        let batch = next_transaction(pump).await?;
        if batch.end_lsn() <= first_lsn {
            continue;
        }
        assert!(batch_contains_marker(&batch, b"after-restart-a"));
        assert!(batch_contains_marker(&batch, b"after-restart-b"));
        let second_lsn = batch.end_lsn();
        pump.shutdown().await?;
        return Ok(second_lsn);
    }
}

async fn verify_durable_replay(
    log_path: &Path,
    writer: &Client,
    first_lsn: LogSequenceNumber,
    second_lsn: LogSequenceNumber,
) -> Result<(), TestError> {
    let replayed =
        DurableCdcLog::replay_from(log_path, BatchLimits::default(), LogSequenceNumber::ZERO)?;
    assert_eq!(replayed.len(), 2);
    assert_eq!(replayed[0].end_lsn(), first_lsn);
    assert_eq!(replayed[1].end_lsn(), second_lsn);

    let row_count: i64 = writer
        .query_one("SELECT count(*) FROM veyra_cdc_fixture", &[])
        .await?
        .get(0);
    assert_eq!(row_count, 3);
    Ok(())
}

#[tokio::test]
async fn snapshot_catchup_restart_and_duplicate_delivery_are_gap_free() -> Result<(), TestError> {
    let Ok(url) = std::env::var("VEYRA_TEST_POSTGRES_URL") else {
        eprintln!("VEYRA_TEST_POSTGRES_URL not configured; skipping live PostgreSQL CDC test");
        return Ok(());
    };

    let (mut control, control_task) = connect(&url).await?;
    let (writer, writer_task) = connect(&url).await?;
    prepare_fixture(&control).await?;

    let boundary = establish_snapshot_overlap(&mut control, &writer).await?;
    let directory = tempdir()?;
    let log_path = directory.path().join("cdc.log");

    let mut first_pump = start_pump(
        &log_path,
        boundary.replay_from_lsn(),
        CdcProgressTracker::ZERO,
    )
    .await?;
    let first_lsn = consume_initial_snapshot_transaction(&mut first_pump).await?;

    writer
        .batch_execute(
            "BEGIN; \
             INSERT INTO veyra_cdc_fixture(marker) VALUES ('after-restart-a'); \
             INSERT INTO veyra_cdc_fixture(marker) VALUES ('after-restart-b'); \
             COMMIT;",
        )
        .await?;

    let progress =
        CdcProgressTracker::recover(first_lsn, LogSequenceNumber::ZERO, LogSequenceNumber::ZERO)?;
    let mut second_pump = start_pump(&log_path, first_lsn, progress).await?;
    let second_lsn = consume_fresh_after_restart(&mut second_pump, first_lsn).await?;
    assert!(second_lsn > first_lsn);

    verify_durable_replay(&log_path, &writer, first_lsn, second_lsn).await?;
    control_task.abort();
    writer_task.abort();
    Ok(())
}
