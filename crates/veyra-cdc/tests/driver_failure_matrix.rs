use std::cell::Cell;
use std::error::Error as _;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use pgwire_replication::{Lsn, ReplicationEvent};
use veyra_cdc::{
    ChangeKind, CheckpointApplyError, CheckpointError, Journal, JournalError,
    LiveReplicationDriver, LiveReplicationError, ProcessorError, ReplayDecision, RowChange,
    TransactionBatch,
};
use veyra_types::LogSequenceNumber;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ApplyFailure;

impl std::fmt::Display for ApplyFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("apply failed")
    }
}

impl std::error::Error for ApplyFailure {}

fn paths(label: &str) -> (PathBuf, PathBuf) {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let base = format!(
        "veyra-driver-failure-{label}-{}-{nanos}",
        std::process::id()
    );
    (
        std::env::temp_dir().join(format!("{base}.journal")),
        std::env::temp_dir().join(format!("{base}.checkpoint")),
    )
}

fn batch() -> TransactionBatch {
    TransactionBatch::try_new(
        7,
        LogSequenceNumber::new(20),
        LogSequenceNumber::new(20),
        LogSequenceNumber::new(21),
        vec![RowChange::new(11, ChangeKind::Insert, None, Some(vec![1]))],
    )
    .unwrap_or_else(|_| unreachable!())
}

fn relation(relation_id: u32) -> Vec<u8> {
    let mut bytes = vec![b'R'];
    bytes.extend_from_slice(&relation_id.to_be_bytes());
    bytes.extend_from_slice(b"public\0inventory\0");
    bytes.push(b'd');
    bytes.extend_from_slice(&0_u16.to_be_bytes());
    bytes
}

fn insert(relation_id: u32) -> Vec<u8> {
    let mut bytes = vec![b'I'];
    bytes.extend_from_slice(&relation_id.to_be_bytes());
    bytes.push(b'N');
    bytes.extend_from_slice(&1_u16.to_be_bytes());
    bytes.push(b't');
    bytes.extend_from_slice(&1_u32.to_be_bytes());
    bytes.push(1);
    bytes
}

fn xlog(data: Vec<u8>) -> ReplicationEvent {
    ReplicationEvent::XLogData {
        wal_start: Lsn::from_u64(20),
        wal_end: Lsn::from_u64(20),
        server_time_micros: 0,
        data: data.into(),
    }
}

#[test]
fn driver_open_preserves_the_exact_durable_failure_layer() {
    let (missing_journal, missing_checkpoint) = paths("missing");
    let missing_root = missing_journal.with_extension("missing-directory");
    let mut apply = |_batch: &TransactionBatch| -> Result<(), ApplyFailure> { Ok(()) };
    let error = LiveReplicationDriver::open(
        &missing_root.join("journal"),
        &missing_root.join("checkpoint"),
        &mut apply,
    )
    .err()
    .unwrap_or_else(|| unreachable!());
    assert!(matches!(
        error,
        LiveReplicationError::Journal(JournalError::Io(_))
    ));

    let (journal_path, _) = paths("checkpoint-open");
    Journal::open(&journal_path).unwrap_or_else(|_| unreachable!());
    let checkpoint_root = missing_checkpoint.with_extension("missing-directory");
    let error = LiveReplicationDriver::open(
        &journal_path,
        &checkpoint_root.join("checkpoint"),
        &mut apply,
    )
    .err()
    .unwrap_or_else(|| unreachable!());
    assert!(matches!(
        error,
        LiveReplicationError::Checkpoint(CheckpointError::Io(_))
    ));

    let (replay_journal, replay_checkpoint) = paths("replay-apply");
    let mut journal = Journal::open(&replay_journal).unwrap_or_else(|_| unreachable!());
    assert_eq!(
        journal.append(&batch()).unwrap_or_else(|_| unreachable!()),
        ReplayDecision::Apply
    );
    drop(journal);
    let mut fail = |_batch: &TransactionBatch| -> Result<(), ApplyFailure> { Err(ApplyFailure) };
    let error = LiveReplicationDriver::open(&replay_journal, &replay_checkpoint, &mut fail)
        .err()
        .unwrap_or_else(|| unreachable!());
    assert!(matches!(
        error,
        LiveReplicationError::Processor(ProcessorError::Apply(CheckpointApplyError::Apply(
            ApplyFailure
        )))
    ));

    let _ = std::fs::remove_file(journal_path);
    let _ = std::fs::remove_file(replay_journal);
    let _ = std::fs::remove_file(replay_checkpoint);
}

#[test]
fn rejected_events_are_counted_but_never_acknowledged() {
    let (journal_path, checkpoint_path) = paths("rejected-events");
    let mut apply = |_batch: &TransactionBatch| -> Result<(), ApplyFailure> { Ok(()) };
    let mut driver = LiveReplicationDriver::open(&journal_path, &checkpoint_path, &mut apply)
        .unwrap_or_else(|_| unreachable!());

    let error = driver
        .process(
            ReplicationEvent::Message {
                transactional: false,
                lsn: Lsn::from_u64(1),
                prefix: "unsupported".to_owned(),
                content: Vec::<u8>::new().into(),
            },
            &mut apply,
        )
        .err()
        .unwrap_or_else(|| unreachable!());
    assert!(matches!(
        error,
        LiveReplicationError::UnsupportedLogicalMessage(prefix) if prefix == "unsupported"
    ));
    assert_eq!(driver.summary().events_seen, 1);
    assert_eq!(driver.summary().acknowledgements, 0);
    assert_eq!(
        driver.summary().progress.applied_lsn,
        LogSequenceNumber::ZERO
    );

    driver
        .process(
            ReplicationEvent::Begin {
                final_lsn: Lsn::from_u64(20),
                xid: 7,
                commit_time_micros: 0,
            },
            &mut apply,
        )
        .unwrap_or_else(|_| unreachable!());
    let error = driver
        .process(
            ReplicationEvent::StoppedAt {
                reached: Lsn::from_u64(20),
            },
            &mut apply,
        )
        .err()
        .unwrap_or_else(|| unreachable!());
    assert!(matches!(
        error,
        LiveReplicationError::StoppedMidTransaction(lsn) if lsn == LogSequenceNumber::new(20)
    ));
    assert_eq!(driver.summary().events_seen, 3);
    assert_eq!(driver.summary().acknowledgements, 0);

    let _ = std::fs::remove_file(journal_path);
    let _ = std::fs::remove_file(checkpoint_path);
}

#[test]
fn failed_commit_apply_keeps_driver_progress_unacknowledged() {
    let (journal_path, checkpoint_path) = paths("commit-apply");
    let applied = Cell::new(0_u32);
    let mut apply = |_batch: &TransactionBatch| -> Result<(), ApplyFailure> {
        applied.set(applied.get().saturating_add(1));
        Ok(())
    };
    let mut driver = LiveReplicationDriver::open(&journal_path, &checkpoint_path, &mut apply)
        .unwrap_or_else(|_| unreachable!());

    driver
        .process(
            ReplicationEvent::Begin {
                final_lsn: Lsn::from_u64(20),
                xid: 7,
                commit_time_micros: 0,
            },
            &mut apply,
        )
        .unwrap_or_else(|_| unreachable!());
    driver
        .process(xlog(relation(11)), &mut apply)
        .unwrap_or_else(|_| unreachable!());
    driver
        .process(xlog(insert(11)), &mut apply)
        .unwrap_or_else(|_| unreachable!());

    let mut fail = |_batch: &TransactionBatch| -> Result<(), ApplyFailure> { Err(ApplyFailure) };
    let error = driver
        .process(
            ReplicationEvent::Commit {
                lsn: Lsn::from_u64(20),
                end_lsn: Lsn::from_u64(21),
                commit_time_micros: 0,
            },
            &mut fail,
        )
        .err()
        .unwrap_or_else(|| unreachable!());
    assert!(matches!(
        error,
        LiveReplicationError::Processor(ProcessorError::Apply(CheckpointApplyError::Apply(
            ApplyFailure
        )))
    ));
    assert_eq!(applied.get(), 0);
    assert_eq!(driver.summary().events_seen, 4);
    assert_eq!(driver.summary().acknowledgements, 0);
    assert_eq!(
        driver.summary().progress.applied_lsn,
        LogSequenceNumber::ZERO
    );
    assert_eq!(
        driver.summary().progress.durable_lsn,
        LogSequenceNumber::ZERO
    );

    let _ = std::fs::remove_file(journal_path);
    let _ = std::fs::remove_file(checkpoint_path);
}

#[test]
fn typed_driver_errors_keep_their_sources_when_available() {
    let journal = LiveReplicationError::<ApplyFailure>::Journal(JournalError::from(
        std::io::Error::other("journal"),
    ));
    let checkpoint = LiveReplicationError::<ApplyFailure>::Checkpoint(CheckpointError::from(
        std::io::Error::other("checkpoint"),
    ));
    assert!(journal.source().is_none());
    assert!(checkpoint.source().is_none());
    assert!(journal.to_string().contains("journal"));
    assert!(checkpoint.to_string().contains("checkpoint"));
}
