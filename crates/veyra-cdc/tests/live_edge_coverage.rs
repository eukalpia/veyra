use std::cell::Cell;
use std::error::Error as _;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use pgwire_replication::{Lsn, ReplicationEvent};
use veyra_cdc::{
    AppliedCheckpoint, ChangeKind, CheckpointApplyError, CheckpointError,
    DurableTransactionProcessor, Journal, JournalError, LiveEventOutcome, LiveReplicationError,
    LiveReplicationState, ProcessorError, ReplayDecision, RowChange, TransactionBatch,
    process_replication_event, recover_checkpointed,
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

fn path(label: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    std::env::temp_dir().join(format!(
        "veyra-live-edge-{label}-{}-{nanos}.bin",
        std::process::id()
    ))
}

fn batch(commit: u64, end: u64, value: u8) -> TransactionBatch {
    TransactionBatch::try_new(
        7,
        LogSequenceNumber::new(commit),
        LogSequenceNumber::new(commit),
        LogSequenceNumber::new(end),
        vec![RowChange::new(
            11,
            ChangeKind::Insert,
            None,
            Some(vec![value]),
        )],
    )
    .unwrap_or_else(|_| unreachable!())
}

fn write_journal(file_path: &Path, batches: &[TransactionBatch]) {
    let mut journal = Journal::open(file_path).unwrap_or_else(|_| unreachable!());
    for batch in batches {
        assert!(matches!(journal.append(batch), Ok(ReplayDecision::Apply)));
    }
}

fn write_checkpoint(file_path: &Path, batch: &TransactionBatch) {
    let mut checkpoint = AppliedCheckpoint::open(file_path).unwrap_or_else(|_| unreachable!());
    checkpoint.advance(batch).unwrap_or_else(|_| unreachable!());
}

fn relation(relation_id: u32) -> Vec<u8> {
    let mut bytes = vec![b'R'];
    bytes.extend_from_slice(&relation_id.to_be_bytes());
    bytes.extend_from_slice(b"public\0inventory\0");
    bytes.push(b'd');
    bytes.extend_from_slice(&0_u16.to_be_bytes());
    bytes
}

fn insert(relation_id: u32, value: &[u8]) -> Vec<u8> {
    let mut bytes = vec![b'I'];
    bytes.extend_from_slice(&relation_id.to_be_bytes());
    bytes.push(b'N');
    bytes.extend_from_slice(&1_u16.to_be_bytes());
    bytes.push(b't');
    bytes.extend_from_slice(&u32::try_from(value.len()).unwrap_or_default().to_be_bytes());
    bytes.extend_from_slice(value);
    bytes
}

fn commit(commit_lsn: u64, end_lsn: u64) -> Vec<u8> {
    let mut bytes = vec![b'C', 0];
    bytes.extend_from_slice(&commit_lsn.to_be_bytes());
    bytes.extend_from_slice(&end_lsn.to_be_bytes());
    bytes.extend_from_slice(&0_i64.to_be_bytes());
    bytes
}

fn begin_event(final_lsn: u64) -> ReplicationEvent {
    ReplicationEvent::Begin {
        final_lsn: Lsn::from_u64(final_lsn),
        xid: 7,
        commit_time_micros: 0,
    }
}

fn xlog(wal_start: u64, wal_end: u64, data: Vec<u8>) -> ReplicationEvent {
    ReplicationEvent::XLogData {
        wal_start: Lsn::from_u64(wal_start),
        wal_end: Lsn::from_u64(wal_end),
        server_time_micros: 0,
        data: data.into(),
    }
}

fn commit_event(commit_lsn: u64, end_lsn: u64) -> ReplicationEvent {
    ReplicationEvent::Commit {
        lsn: Lsn::from_u64(commit_lsn),
        end_lsn: Lsn::from_u64(end_lsn),
        commit_time_micros: 0,
    }
}

#[test]
fn checkpoint_recovery_skips_older_records_and_matches_the_target() {
    let journal_path = path("recover-journal");
    let checkpoint_path = path("recover-checkpoint");
    let first = batch(10, 11, 1);
    let second = batch(20, 21, 2);
    write_journal(&journal_path, &[first, second.clone()]);
    write_checkpoint(&checkpoint_path, &second);

    let mut processor =
        DurableTransactionProcessor::open(&journal_path).unwrap_or_else(|_| unreachable!());
    let mut checkpoint =
        AppliedCheckpoint::open(&checkpoint_path).unwrap_or_else(|_| unreachable!());
    let applied = Cell::new(0_u32);
    let mut apply = |_batch: &TransactionBatch| -> Result<(), ApplyFailure> {
        applied.set(applied.get().saturating_add(1));
        Ok(())
    };

    assert_eq!(
        recover_checkpointed(&mut processor, &mut checkpoint, &mut apply),
        Ok(LogSequenceNumber::new(21))
    );
    assert_eq!(applied.get(), 0);

    let _ = fs::remove_file(journal_path);
    let _ = fs::remove_file(checkpoint_path);
}

#[test]
fn checkpoint_recovery_rejects_missing_and_conflicting_durable_records() {
    let first = batch(10, 11, 1);
    let second = batch(20, 21, 2);

    let journal_path = path("missing-journal");
    let checkpoint_path = path("missing-checkpoint");
    write_journal(&journal_path, &[first.clone(), second.clone()]);
    write_checkpoint(&checkpoint_path, &batch(15, 16, 3));
    let mut processor =
        DurableTransactionProcessor::open(&journal_path).unwrap_or_else(|_| unreachable!());
    let mut checkpoint =
        AppliedCheckpoint::open(&checkpoint_path).unwrap_or_else(|_| unreachable!());
    let mut apply = |_batch: &TransactionBatch| -> Result<(), ApplyFailure> { Ok(()) };
    assert!(matches!(
        recover_checkpointed(&mut processor, &mut checkpoint, &mut apply),
        Err(LiveReplicationError::Processor(ProcessorError::Apply(
            CheckpointApplyError::Checkpoint(CheckpointError::MissingFromJournal(lsn))
        ))) if lsn == LogSequenceNumber::new(15)
    ));

    let empty_journal = path("empty-journal");
    let missing_checkpoint = path("empty-missing-checkpoint");
    write_journal(&empty_journal, &[]);
    write_checkpoint(&missing_checkpoint, &batch(15, 16, 4));
    let mut processor =
        DurableTransactionProcessor::open(&empty_journal).unwrap_or_else(|_| unreachable!());
    let mut checkpoint =
        AppliedCheckpoint::open(&missing_checkpoint).unwrap_or_else(|_| unreachable!());
    assert!(matches!(
        recover_checkpointed(&mut processor, &mut checkpoint, &mut apply),
        Err(LiveReplicationError::Checkpoint(
            CheckpointError::MissingFromJournal(lsn)
        )) if lsn == LogSequenceNumber::new(15)
    ));

    let mismatch_journal = path("mismatch-journal");
    let mismatch_checkpoint = path("mismatch-checkpoint");
    write_journal(&mismatch_journal, &[first, second]);
    write_checkpoint(&mismatch_checkpoint, &batch(20, 21, 99));
    let mut processor =
        DurableTransactionProcessor::open(&mismatch_journal).unwrap_or_else(|_| unreachable!());
    let mut checkpoint =
        AppliedCheckpoint::open(&mismatch_checkpoint).unwrap_or_else(|_| unreachable!());
    assert!(matches!(
        recover_checkpointed(&mut processor, &mut checkpoint, &mut apply),
        Err(LiveReplicationError::Processor(ProcessorError::Apply(
            CheckpointApplyError::Checkpoint(CheckpointError::DurableMismatch(lsn))
        ))) if lsn == LogSequenceNumber::new(20)
    ));

    for item in [
        journal_path,
        checkpoint_path,
        empty_journal,
        missing_checkpoint,
        mismatch_journal,
        mismatch_checkpoint,
    ] {
        let _ = fs::remove_file(item);
    }
}

#[test]
fn raw_xlog_commit_acknowledges_only_after_apply_and_updates_all_progress() {
    let journal_path = path("xlog-journal");
    let checkpoint_path = path("xlog-checkpoint");
    let mut processor =
        DurableTransactionProcessor::open(&journal_path).unwrap_or_else(|_| unreachable!());
    let mut checkpoint =
        AppliedCheckpoint::open(&checkpoint_path).unwrap_or_else(|_| unreachable!());
    let mut state = LiveReplicationState::default();
    let applied = Cell::new(0_u32);
    let mut apply = |_batch: &TransactionBatch| -> Result<(), ApplyFailure> {
        applied.set(applied.get().saturating_add(1));
        Ok(())
    };

    assert_eq!(
        process_replication_event(
            &mut processor,
            &mut checkpoint,
            &mut state,
            begin_event(20),
            &mut apply,
        ),
        Ok(LiveEventOutcome::Continue)
    );
    for data in [relation(11), insert(11, b"x")] {
        assert_eq!(
            process_replication_event(
                &mut processor,
                &mut checkpoint,
                &mut state,
                xlog(20, 0, data),
                &mut apply,
            ),
            Ok(LiveEventOutcome::Continue)
        );
    }
    assert_eq!(
        process_replication_event(
            &mut processor,
            &mut checkpoint,
            &mut state,
            xlog(20, 0, commit(20, 21)),
            &mut apply,
        ),
        Ok(LiveEventOutcome::Acknowledge(LogSequenceNumber::new(21)))
    );
    assert_eq!(applied.get(), 1);
    assert!(!state.transaction_open());
    assert_eq!(state.progress().received_lsn, LogSequenceNumber::new(21));
    assert_eq!(state.progress().durable_lsn, LogSequenceNumber::new(21));
    assert_eq!(state.progress().applied_lsn, LogSequenceNumber::new(21));

    assert_eq!(
        process_replication_event(
            &mut processor,
            &mut checkpoint,
            &mut state,
            ReplicationEvent::KeepAlive {
                wal_end: Lsn::from_u64(5),
                reply_requested: false,
                server_time_micros: 0,
            },
            &mut apply,
        ),
        Ok(LiveEventOutcome::Continue)
    );
    assert_eq!(state.progress().received_lsn, LogSequenceNumber::new(21));

    let _ = fs::remove_file(journal_path);
    let _ = fs::remove_file(checkpoint_path);
}

#[test]
fn commit_apply_failure_produces_no_ack_and_poisoned_processor() {
    let journal_path = path("apply-journal");
    let checkpoint_path = path("apply-checkpoint");
    let mut processor =
        DurableTransactionProcessor::open(&journal_path).unwrap_or_else(|_| unreachable!());
    let mut checkpoint =
        AppliedCheckpoint::open(&checkpoint_path).unwrap_or_else(|_| unreachable!());
    let mut state = LiveReplicationState::default();
    let mut prepare = |_batch: &TransactionBatch| -> Result<(), ApplyFailure> { Ok(()) };

    process_replication_event(
        &mut processor,
        &mut checkpoint,
        &mut state,
        begin_event(20),
        &mut prepare,
    )
    .unwrap_or_else(|_| unreachable!());
    for data in [relation(11), insert(11, b"x")] {
        process_replication_event(
            &mut processor,
            &mut checkpoint,
            &mut state,
            xlog(20, 20, data),
            &mut prepare,
        )
        .unwrap_or_else(|_| unreachable!());
    }

    let mut fail = |_batch: &TransactionBatch| -> Result<(), ApplyFailure> { Err(ApplyFailure) };
    assert!(matches!(
        process_replication_event(
            &mut processor,
            &mut checkpoint,
            &mut state,
            commit_event(20, 21),
            &mut fail,
        ),
        Err(LiveReplicationError::Processor(ProcessorError::Apply(
            CheckpointApplyError::Apply(ApplyFailure)
        )))
    ));
    assert!(processor.is_poisoned());
    assert_eq!(checkpoint.state().commit_lsn(), LogSequenceNumber::ZERO);
    assert_eq!(state.progress().applied_lsn, LogSequenceNumber::ZERO);

    let _ = fs::remove_file(journal_path);
    let _ = fs::remove_file(checkpoint_path);
}

#[test]
fn processor_recovery_poisoning_preserves_strict_journal_tail_semantics() {
    let journal_path = path("processor-tail");
    let mut processor =
        DurableTransactionProcessor::open(&journal_path).unwrap_or_else(|_| unreachable!());
    OpenOptions::new()
        .append(true)
        .open(&journal_path)
        .and_then(|mut file| file.write_all(&[1]))
        .unwrap_or_else(|_| unreachable!());
    let mut apply = |_batch: &TransactionBatch| -> Result<(), ApplyFailure> { Ok(()) };

    assert!(matches!(
        processor.recover(&mut apply),
        Err(ProcessorError::Journal(JournalError::IncompleteTail(0)))
    ));
    assert!(processor.is_poisoned());
    assert!(matches!(
        processor.recover(&mut apply),
        Err(ProcessorError::Poisoned)
    ));
    let _ = fs::remove_file(journal_path);
}

#[test]
fn checkpoint_apply_and_live_errors_expose_stable_context() {
    let apply = CheckpointApplyError::Apply(ApplyFailure);
    let checkpoint = CheckpointApplyError::<ApplyFailure>::Checkpoint(
        CheckpointError::MissingFromJournal(LogSequenceNumber::new(7)),
    );
    assert_eq!(apply.to_string(), "projection apply: apply failed");
    assert!(checkpoint.to_string().contains("applied checkpoint"));
    assert!(apply.source().is_none());

    let errors = [
        LiveReplicationError::<ApplyFailure>::Journal(JournalError::UnexpectedEof),
        LiveReplicationError::Checkpoint(CheckpointError::CorruptRecord),
        LiveReplicationError::Processor(ProcessorError::Journal(JournalError::UnexpectedEof)),
        LiveReplicationError::UnsupportedLogicalMessage("prefix".to_owned()),
        LiveReplicationError::StoppedMidTransaction(LogSequenceNumber::new(8)),
        LiveReplicationError::UnexpectedBoundaryOutcome,
    ];
    for error in errors {
        assert!(!error.to_string().is_empty());
        assert!(error.source().is_none());
    }
}
