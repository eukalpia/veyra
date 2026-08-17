#![cfg(target_os = "linux")]

use veyra_cdc::{AppliedCheckpoint, ChangeKind, CheckpointError, RowChange, TransactionBatch};
use veyra_types::LogSequenceNumber;

fn batch() -> TransactionBatch {
    TransactionBatch::try_new(
        1,
        LogSequenceNumber::new(10),
        LogSequenceNumber::new(10),
        LogSequenceNumber::new(11),
        vec![RowChange::new(
            7,
            ChangeKind::Insert,
            None,
            Some(vec![1]),
        )],
    )
    .unwrap_or_else(|_| unreachable!())
}

#[test]
fn checkpoint_write_and_sync_failures_are_typed_and_never_publish_state() {
    let record = batch();

    let mut full = AppliedCheckpoint::open("/dev/full").unwrap_or_else(|_| unreachable!());
    assert!(matches!(
        full.advance(&record),
        Err(CheckpointError::Io(_))
    ));
    assert_eq!(full.state().commit_lsn(), LogSequenceNumber::ZERO);

    let mut null = AppliedCheckpoint::open("/dev/null").unwrap_or_else(|_| unreachable!());
    assert!(matches!(
        null.advance(&record),
        Err(CheckpointError::Io(_))
    ));
    assert_eq!(null.state().commit_lsn(), LogSequenceNumber::ZERO);
}
