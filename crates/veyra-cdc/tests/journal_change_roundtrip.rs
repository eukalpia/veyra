use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use veyra_cdc::{ChangeKind, Journal, JournalError, ReplayDecision, RowChange, TransactionBatch};
use veyra_types::LogSequenceNumber;

fn path(label: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    std::env::temp_dir().join(format!(
        "veyra-journal-roundtrip-{label}-{}-{nanos}.bin",
        std::process::id()
    ))
}

fn batch(commit: u64, value: u8) -> TransactionBatch {
    TransactionBatch::try_new(
        17,
        LogSequenceNumber::new(commit),
        LogSequenceNumber::new(commit),
        LogSequenceNumber::new(commit + 1),
        vec![
            RowChange::new(1, ChangeKind::Insert, None, Some(vec![value])),
            RowChange::new(
                2,
                ChangeKind::Update,
                Some(vec![value]),
                Some(vec![value.saturating_add(1)]),
            ),
            RowChange::new(3, ChangeKind::Delete, Some(vec![value]), None),
            RowChange::new(4, ChangeKind::Truncate, None, None),
        ],
    )
    .unwrap_or_else(|_| unreachable!())
}

#[test]
fn every_change_kind_and_optional_tuple_shape_round_trips() -> Result<(), JournalError> {
    let journal_path = path("all-kinds");
    let first = batch(100, 7);
    let second = batch(200, 9);

    {
        let mut journal = Journal::open(&journal_path)?;
        assert_eq!(journal.highest_commit_lsn(), LogSequenceNumber::ZERO);
        assert_eq!(journal.append(&first)?, ReplayDecision::Apply);
        assert_eq!(journal.append(&second)?, ReplayDecision::Apply);
        assert_eq!(journal.highest_commit_lsn(), LogSequenceNumber::new(200));
        assert_eq!(journal.replay()?, vec![first.clone(), second.clone()]);
    }

    let mut reopened = Journal::open(&journal_path)?;
    assert_eq!(reopened.highest_commit_lsn(), LogSequenceNumber::new(200));
    assert_eq!(reopened.replay()?, vec![first, second]);
    let _ = fs::remove_file(journal_path);
    Ok(())
}

#[test]
fn conflicting_same_lsn_is_rejected_without_changing_durable_state() -> Result<(), JournalError> {
    let journal_path = path("conflict");
    let first = batch(100, 7);
    let conflicting = batch(100, 8);
    let mut journal = Journal::open(&journal_path)?;
    assert_eq!(journal.append(&first)?, ReplayDecision::Apply);
    assert!(matches!(
        journal.append(&conflicting),
        Err(JournalError::ConflictingReplay(lsn)) if lsn == LogSequenceNumber::new(100)
    ));
    assert_eq!(journal.highest_commit_lsn(), LogSequenceNumber::new(100));
    assert_eq!(journal.replay()?, vec![first]);
    let _ = fs::remove_file(journal_path);
    Ok(())
}
