use std::time::Duration;

use pgwire_replication::{ReplicationConfig, TlsConfig};
use tempfile::tempdir;
use tokio::time::timeout;
use tokio_postgres::NoTls;
use veyra_cdc::{
    BatchLimits, CdcAssembler, CdcProgressTracker, CdcPump, CdcPumpEvent, DurableCdcLog,
    PostgresReplicationStream, TransactionItem, begin_consistent_snapshot,
};
use veyra_types::LogSequenceNumber;

const SLOT: &str = "veyra_ci_slot";
const PUBLICATION: &str = "veyra_ci_publication";

async fn connect(url: &str) -> Result<(tokio_postgres::Client, tokio::task::JoinHandle<()>), Box<dyn std::error::Error>> {
    let (client, connection) = tokio_postgres::connect(url, NoTls).await?;
    let task = tokio::spawn(async move {
        let _ = connection.await;
    });
    Ok((client, task))
}

fn replication_config() -> ReplicationConfig {
    ReplicationConfig::new("127.0.0.1", "postgres", "postgres", "veyra", SLOT, PUBLICATION)
        .with_tls(TlsConfig::disabled())
        .with_status_interval(Duration::from_millis(50))
        .with_wakeup_interval(Duration::from_millis(10))
}

async fn next_transaction(pump: &mut CdcPump) -> Result<veyra_cdc::TransactionBatch, Box<dyn std::error::Error>> {
    loop {
        let event = timeout(Duration::from_secs(10), pump.next_durable()).await??;
        match event {
            CdcPumpEvent::Transaction { batch, .. } => return Ok(batch),
            CdcPumpEvent::StoppedAt(_) | CdcPumpEvent::EndOfStream => {
                return Err("replication ended before a committed transaction".into());
            }
        }
    }
}

fn batch_contains_marker(batch: &veyra_cdc::TransactionBatch, marker: &[u8]) -> bool {
    batch.items().iter().any(|item| match item {
        TransactionItem::Wal(chunk) => chunk.data().windows(marker.len()).any(|window| window == marker),
        TransactionItem::Message(message) => message.content().windows(marker.len()).any(|window| window == marker),
    })
}

#[tokio::test]
async fn snapshot_catchup_restart_and_duplicate_delivery_are_gap_free() -> Result<(), Box<dyn std::error::Error>> {
    let Ok(url) = std::env::var("VEYRA_TEST_POSTGRES_URL") else {
        eprintln!("VEYRA_TEST_POSTGRES_URL not configured; skipping live PostgreSQL CDC test");
        return Ok(());
    };

    let (mut control, control_task) = connect(&url).await?;
    let (writer, writer_task) = connect(&url).await?;

    control.batch_execute(
        "DROP PUBLICATION IF EXISTS veyra_ci_publication; \
         DROP TABLE IF EXISTS veyra_cdc_fixture; \
         CREATE TABLE veyra_cdc_fixture (id BIGSERIAL PRIMARY KEY, marker TEXT NOT NULL); \
         CREATE PUBLICATION veyra_ci_publication FOR TABLE veyra_cdc_fixture;",
    ).await?;
    control.execute(
        "SELECT pg_drop_replication_slot(slot_name) FROM pg_replication_slots WHERE slot_name = $1",
        &[&SLOT],
    ).await?;
    control.query_one(
        "SELECT slot_name, lsn FROM pg_create_logical_replication_slot($1, 'pgoutput')",
        &[&SLOT],
    ).await?;

    let snapshot = begin_consistent_snapshot(&mut control, SLOT).await?;
    let count: i64 = snapshot.transaction().query_one("SELECT count(*) FROM veyra_cdc_fixture", &[]).await?.get(0);
    assert_eq!(count, 0);

    writer.execute("INSERT INTO veyra_cdc_fixture(marker) VALUES ('during-snapshot')", &[]).await?;
    let snapshot_count: i64 = snapshot.transaction().query_one("SELECT count(*) FROM veyra_cdc_fixture", &[]).await?.get(0);
    assert_eq!(snapshot_count, 0);
    let boundary = snapshot.finish().await?;
    assert!(boundary.snapshot_lsn() >= boundary.replay_from_lsn());
    assert!(!boundary.snapshot_id().is_empty());

    let directory = tempdir()?;
    let log_path = directory.path().join("cdc.log");
    let (log, outcome) = DurableCdcLog::open(&log_path, BatchLimits::default())?;
    assert_eq!(outcome.last_durable_lsn, LogSequenceNumber::ZERO);
    let stream = PostgresReplicationStream::connect(replication_config(), boundary.replay_from_lsn()).await?;
    let mut pump = CdcPump::new(
        stream,
        CdcAssembler::new(BatchLimits::default()),
        log,
        CdcProgressTracker::ZERO,
    );
    let first = next_transaction(&mut pump).await?;
    assert!(batch_contains_marker(&first, b"during-snapshot"));
    let first_lsn = first.end_lsn();
    assert_eq!(pump.durable_lsn(), first_lsn);
    assert_eq!(pump.progress().snapshot().durable(), first_lsn);
    pump.shutdown().await?;

    writer.batch_execute(
        "BEGIN; \
         INSERT INTO veyra_cdc_fixture(marker) VALUES ('after-restart-a'); \
         INSERT INTO veyra_cdc_fixture(marker) VALUES ('after-restart-b'); \
         COMMIT;",
    ).await?;

    let (log, outcome) = DurableCdcLog::open(&log_path, BatchLimits::default())?;
    assert_eq!(outcome.last_durable_lsn, first_lsn);
    let stream = PostgresReplicationStream::connect(replication_config(), first_lsn).await?;
    let progress = CdcProgressTracker::recover(first_lsn, LogSequenceNumber::ZERO, LogSequenceNumber::ZERO)?;
    let mut pump = CdcPump::new(
        stream,
        CdcAssembler::new(BatchLimits::default()),
        log,
        progress,
    );

    let second = loop {
        let batch = next_transaction(&mut pump).await?;
        if batch.end_lsn() > first_lsn {
            break batch;
        }
    };
    assert!(batch_contains_marker(&second, b"after-restart-a"));
    assert!(batch_contains_marker(&second, b"after-restart-b"));
    let second_lsn = second.end_lsn();
    assert!(second_lsn > first_lsn);
    pump.shutdown().await?;

    let replayed = DurableCdcLog::replay_from(&log_path, BatchLimits::default(), LogSequenceNumber::ZERO)?;
    assert_eq!(replayed.len(), 2);
    assert_eq!(replayed[0].end_lsn(), first_lsn);
    assert_eq!(replayed[1].end_lsn(), second_lsn);
    let row_count: i64 = writer.query_one("SELECT count(*) FROM veyra_cdc_fixture", &[]).await?.get(0);
    assert_eq!(row_count, 3);

    control_task.abort();
    writer_task.abort();
    Ok(())
}
