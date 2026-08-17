use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use veyra_cdc::{ChangeKind, Journal, JournalError, ReplayDecision, RowChange, TransactionBatch};
use veyra_types::LogSequenceNumber;

#[cfg(target_os = "linux")]
const MAX_RECORD_BYTES: usize = 64 * 1024 * 1024;
#[cfg(target_os = "linux")]
const MAX_CHANGES: u32 = 1_000_000;

fn path(label: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    std::env::temp_dir().join(format!(
        "veyra-journal-region-{label}-{}-{nanos}.bin",
        std::process::id()
    ))
}

fn batch(changes: Vec<RowChange>) -> TransactionBatch {
    TransactionBatch::try_new(
        7,
        LogSequenceNumber::new(20),
        LogSequenceNumber::new(20),
        LogSequenceNumber::new(21),
        changes,
    )
    .unwrap_or_else(|_| unreachable!())
}

fn small_batch() -> TransactionBatch {
    batch(vec![RowChange::new(
        11,
        ChangeKind::Insert,
        None,
        Some(vec![9]),
    )])
}

fn remove(path: &Path) {
    let _ = fs::remove_file(path);
}

#[cfg(target_os = "linux")]
#[test]
fn encode_bounds_fail_before_any_durable_acknowledgement() {
    let file_path = path("encode-bounds");
    let mut journal = Journal::open(&file_path).unwrap_or_else(|_| unreachable!());

    let too_many = (0_u32..=MAX_CHANGES)
        .map(|relation_id| RowChange::new(relation_id, ChangeKind::Insert, None, None))
        .collect::<Vec<_>>();
    assert!(matches!(
        journal.append(&batch(too_many)),
        Err(JournalError::TooManyChanges(count)) if count == 1_000_001
    ));
    assert_eq!(journal.highest_commit_lsn(), LogSequenceNumber::ZERO);

    let oversized_old = vec![0_u8; MAX_RECORD_BYTES + 1];
    assert!(matches!(
        journal.append(&batch(vec![RowChange::new(
            1,
            ChangeKind::Update,
            Some(oversized_old),
            None,
        )])),
        Err(JournalError::TupleTooLarge(size)) if size == MAX_RECORD_BYTES + 1
    ));
    assert_eq!(journal.highest_commit_lsn(), LogSequenceNumber::ZERO);

    let oversized_new = vec![0_u8; MAX_RECORD_BYTES + 1];
    assert!(matches!(
        journal.append(&batch(vec![RowChange::new(
            1,
            ChangeKind::Update,
            None,
            Some(oversized_new),
        )])),
        Err(JournalError::TupleTooLarge(size)) if size == MAX_RECORD_BYTES + 1
    ));
    assert_eq!(journal.highest_commit_lsn(), LogSequenceNumber::ZERO);

    let half = MAX_RECORD_BYTES / 2 + 1;
    let aggregate = batch(vec![RowChange::new(
        1,
        ChangeKind::Update,
        Some(vec![0_u8; half]),
        Some(vec![0_u8; half]),
    )]);
    assert!(matches!(
        journal.append(&aggregate),
        Err(JournalError::RecordTooLarge(size)) if size > MAX_RECORD_BYTES
    ));
    assert_eq!(journal.highest_commit_lsn(), LogSequenceNumber::ZERO);

    remove(&file_path);
}

#[cfg(target_os = "linux")]
#[test]
fn sync_failure_and_missing_replay_path_never_advance_durable_state() {
    let mut null = Journal::open("/dev/null").unwrap_or_else(|_| unreachable!());
    assert!(matches!(
        null.append(&small_batch()),
        Err(JournalError::Io(_))
    ));
    assert_eq!(null.highest_commit_lsn(), LogSequenceNumber::ZERO);

    let file_path = path("missing-replay");
    let mut journal = Journal::open(&file_path).unwrap_or_else(|_| unreachable!());
    assert_eq!(
        journal
            .append(&small_batch())
            .unwrap_or_else(|_| unreachable!()),
        ReplayDecision::Apply
    );
    remove(&file_path);
    assert!(matches!(journal.replay(), Err(JournalError::Io(_))));
}

#[test]
fn duplicated_durable_record_is_rejected_during_recovery() {
    let file_path = path("duplicate-durable");
    {
        let mut journal = Journal::open(&file_path).unwrap_or_else(|_| unreachable!());
        assert_eq!(
            journal
                .append(&small_batch())
                .unwrap_or_else(|_| unreachable!()),
            ReplayDecision::Apply
        );
    }
    let bytes = fs::read(&file_path).unwrap_or_else(|_| unreachable!());
    OpenOptions::new()
        .append(true)
        .open(&file_path)
        .and_then(|mut file| file.write_all(&bytes))
        .unwrap_or_else(|_| unreachable!());

    assert!(matches!(
        Journal::open(&file_path),
        Err(JournalError::DuplicateDurableRecord(lsn)) if lsn.get() == 20
    ));
    remove(&file_path);
}
