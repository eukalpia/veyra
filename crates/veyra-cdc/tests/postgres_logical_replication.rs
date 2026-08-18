use std::env;
use std::io;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use pgwire_replication::{Lsn, ReplicationConfig, TlsConfig};
use veyra_cdc::{AppliedCheckpoint, ChangeKind, Journal, run_pgwire};

fn unique_path(label: &str, suffix: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    env::temp_dir().join(format!(
        "veyra-postgres-{label}-{}-{nanos}.{suffix}",
        std::process::id()
    ))
}

#[tokio::test]
async fn live_postgres_transaction_is_durable_applied_checkpointed_and_acknowledged()
-> Result<(), Box<dyn std::error::Error>> {
    let Ok(stop_lsn_text) = env::var("VEYRA_POSTGRES_STOP_LSN") else {
        return Ok(());
    };
    let port = env::var("VEYRA_POSTGRES_PORT")
        .ok()
        .and_then(|value| value.parse::<u16>().ok())
        .unwrap_or(55_432);
    let stop_lsn = Lsn::parse(&stop_lsn_text)?;
    let journal_path = unique_path("live", "journal");
    let checkpoint_path = unique_path("live", "checkpoint");

    let config = ReplicationConfig::new(
        "127.0.0.1",
        "postgres",
        "postgres",
        "postgres",
        "veyra_slot",
        "veyra_pub",
    )
    .with_port(port)
    .with_tls(TlsConfig::disabled())
    .with_stop_lsn(stop_lsn)
    .with_status_interval(Duration::from_millis(250))
    .with_wakeup_interval(Duration::from_secs(2));

    let mut applied = Vec::new();
    let mut apply = |batch: &veyra_cdc::TransactionBatch| -> Result<(), io::Error> {
        applied.push(batch.clone());
        Ok(())
    };

    let summary = tokio::time::timeout(
        Duration::from_mins(1),
        run_pgwire(config, &journal_path, &checkpoint_path, &mut apply),
    )
    .await??;

    assert!(summary.events_seen > 0);
    assert_eq!(summary.acknowledgements, 1);
    assert_eq!(applied.len(), 1);
    assert_eq!(applied[0].changes().len(), 1);
    assert_eq!(applied[0].changes()[0].kind, ChangeKind::Insert);
    assert_eq!(summary.progress.durable_lsn, summary.progress.applied_lsn);
    assert!(summary.progress.received_lsn >= summary.progress.applied_lsn);

    let checkpoint = AppliedCheckpoint::open(&checkpoint_path)?;
    assert_eq!(checkpoint.state().end_lsn(), summary.progress.applied_lsn);
    assert_eq!(checkpoint.state().fingerprint(), applied[0].fingerprint());

    let mut journal = Journal::open(&journal_path)?;
    let durable = journal.replay()?;
    assert_eq!(durable, applied);

    let _ = std::fs::remove_file(journal_path);
    let _ = std::fs::remove_file(checkpoint_path);
    Ok(())
}
