use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use veyra_cdc::{
    BootstrapError, ChangeKind, Journal, LiveReplicationDriver, LiveReplicationError, RowChange,
    TransactionBatch,
};
use veyra_types::LogSequenceNumber;

fn lsn(value: u64) -> LogSequenceNumber {
    LogSequenceNumber::new(value)
}

fn path(label: &str, suffix: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    std::env::temp_dir().join(format!(
        "veyra-bootstrap-baseline-{label}-{}-{nanos}.{suffix}",
        std::process::id()
    ))
}

fn batch(commit: u64, end: u64, value: u8) -> TransactionBatch {
    TransactionBatch::try_new(
        7,
        lsn(commit),
        lsn(commit),
        lsn(end),
        vec![RowChange::new(
            42,
            ChangeKind::Insert,
            None,
            Some(vec![value]),
        )],
    )
    .unwrap_or_else(|_| unreachable!())
}

#[test]
fn empty_durable_state_resumes_exactly_from_snapshot_baseline() -> Result<(), Box<dyn std::error::Error>> {
    let journal = path("empty", "journal");
    let checkpoint = path("empty", "checkpoint");
    let mut apply = |_batch: &TransactionBatch| -> Result<(), std::convert::Infallible> { Ok(()) };

    let driver = LiveReplicationDriver::open_from_baseline(
        &journal,
        &checkpoint,
        lsn(100),
        &mut apply,
    )?;

    assert_eq!(driver.resume_lsn(), lsn(100));
    let _ = std::fs::remove_file(journal);
    let _ = std::fs::remove_file(checkpoint);
    Ok(())
}

#[test]
fn durable_uncheckpointed_wal_is_recovered_on_top_of_snapshot_baseline() -> Result<(), Box<dyn std::error::Error>> {
    let journal_path = path("replay", "journal");
    let checkpoint_path = path("replay", "checkpoint");
    let durable = batch(120, 121, 9);
    Journal::open(&journal_path)?.append(&durable)?;

    let mut applied = Vec::new();
    let mut apply = |transaction: &TransactionBatch| -> Result<(), std::convert::Infallible> {
        applied.push(transaction.clone());
        Ok(())
    };
    let driver = LiveReplicationDriver::open_from_baseline(
        &journal_path,
        &checkpoint_path,
        lsn(100),
        &mut apply,
    )?;

    assert_eq!(driver.resume_lsn(), lsn(121));
    assert_eq!(applied, vec![durable]);
    let _ = std::fs::remove_file(journal_path);
    let _ = std::fs::remove_file(checkpoint_path);
    Ok(())
}

#[test]
fn durable_state_behind_snapshot_baseline_is_rejected() -> Result<(), Box<dyn std::error::Error>> {
    let journal_path = path("stale", "journal");
    let checkpoint_path = path("stale", "checkpoint");
    Journal::open(&journal_path)?.append(&batch(80, 81, 1))?;
    let mut apply = |_transaction: &TransactionBatch| -> Result<(), std::convert::Infallible> { Ok(()) };

    let result = LiveReplicationDriver::open_from_baseline(
        &journal_path,
        &checkpoint_path,
        lsn(100),
        &mut apply,
    );

    assert!(matches!(
        result,
        Err(LiveReplicationError::Bootstrap(
            BootstrapError::RecoveredBeforeBaseline {
                baseline,
                recovered,
            }
        )) if baseline == lsn(100) && recovered == lsn(81)
    ));
    let _ = std::fs::remove_file(journal_path);
    let _ = std::fs::remove_file(checkpoint_path);
    Ok(())
}
