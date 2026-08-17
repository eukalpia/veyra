use std::cell::Cell;
use std::error::Error;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use pgwire_replication::{Lsn, ReplicationEvent};
use veyra_cdc::{
    AppliedCheckpoint, ChangeKind, CheckpointError, DurableTransactionProcessor, Journal,
    JournalError, LiveEventOutcome, LiveReplicationState, PgOutputError, ProcessorError,
    ReplayDecision, RowChange, StreamError, TransactionBatch, TransactionBuildError,
    TransactionValidationError, process_replication_event, recover_checkpointed,
};
use veyra_types::LogSequenceNumber;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ApplyFailure;

impl core::fmt::Display for ApplyFailure {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.write_str("apply failed")
    }
}
impl Error for ApplyFailure {}

fn path(label: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    std::env::temp_dir().join(format!(
        "veyra-region-{label}-{}-{nanos}",
        std::process::id()
    ))
}

fn batch(commit: u64, end: u64, value: u8) -> TransactionBatch {
    TransactionBatch::try_new(
        1,
        LogSequenceNumber::new(commit),
        LogSequenceNumber::new(commit),
        LogSequenceNumber::new(end),
        vec![RowChange::new(
            7,
            ChangeKind::Insert,
            None,
            Some(vec![value]),
        )],
    )
    .unwrap_or_else(|_| unreachable!())
}

fn remove(path: &Path) {
    let _ = std::fs::remove_file(path);
}

#[test]
fn recovery_distinguishes_older_exact_future_and_missing_checkpoint_positions()
-> Result<(), Box<dyn Error>> {
    let journal_path = path("journal-order");
    let checkpoint_path = path("checkpoint-order");
    {
        let mut journal = Journal::open(&journal_path)?;
        assert_eq!(journal.append(&batch(10, 11, 1))?, ReplayDecision::Apply);
        assert_eq!(journal.append(&batch(20, 21, 2))?, ReplayDecision::Apply);
    }
    let mut checkpoint = AppliedCheckpoint::open(&checkpoint_path)?;
    let synthetic = batch(15, 16, 9);
    checkpoint.advance(&synthetic)?;
    let mut processor = DurableTransactionProcessor::open(&journal_path)?;
    let applied = Cell::new(0_u32);
    let mut apply = |_batch: &TransactionBatch| -> Result<(), ApplyFailure> {
        applied.set(applied.get() + 1);
        Ok(())
    };
    assert!(matches!(
        recover_checkpointed(&mut processor, &mut checkpoint, &mut apply),
        Err(veyra_cdc::LiveReplicationError::Processor(ProcessorError::Apply(
            veyra_cdc::CheckpointApplyError::Checkpoint(
                CheckpointError::MissingFromJournal(lsn)
            )
        ))) if lsn.get() == 15
    ));
    assert_eq!(
        applied.get(),
        0,
        "nothing after an unproven checkpoint may apply"
    );
    remove(&journal_path);
    remove(&checkpoint_path);

    let journal_path = path("journal-after");
    let checkpoint_path = path("checkpoint-after");
    {
        let mut journal = Journal::open(&journal_path)?;
        journal.append(&batch(10, 11, 1))?;
        journal.append(&batch(20, 21, 2))?;
    }
    let mut checkpoint = AppliedCheckpoint::open(&checkpoint_path)?;
    checkpoint.advance(&batch(30, 31, 3))?;
    let mut processor = DurableTransactionProcessor::open(&journal_path)?;
    let mut apply = |_batch: &TransactionBatch| -> Result<(), ApplyFailure> { Ok(()) };
    assert!(matches!(
        recover_checkpointed(&mut processor, &mut checkpoint, &mut apply),
        Err(veyra_cdc::LiveReplicationError::Checkpoint(
            CheckpointError::MissingFromJournal(lsn)
        )) if lsn.get() == 30
    ));
    remove(&journal_path);
    remove(&checkpoint_path);
    Ok(())
}

#[test]
fn checkpoint_fingerprint_mismatch_is_never_treated_as_duplicate() -> Result<(), Box<dyn Error>> {
    let journal_path = path("journal-mismatch");
    let checkpoint_path = path("checkpoint-mismatch");
    {
        let mut journal = Journal::open(&journal_path)?;
        journal.append(&batch(10, 11, 1))?;
    }
    let mut checkpoint = AppliedCheckpoint::open(&checkpoint_path)?;
    checkpoint.advance(&batch(10, 11, 2))?;
    let mut processor = DurableTransactionProcessor::open(&journal_path)?;
    let mut apply = |_batch: &TransactionBatch| -> Result<(), ApplyFailure> { Ok(()) };
    assert!(matches!(
        recover_checkpointed(&mut processor, &mut checkpoint, &mut apply),
        Err(veyra_cdc::LiveReplicationError::Processor(ProcessorError::Apply(
            veyra_cdc::CheckpointApplyError::Checkpoint(
                CheckpointError::DurableMismatch(lsn)
            )
        ))) if lsn.get() == 10
    ));
    remove(&journal_path);
    remove(&checkpoint_path);
    Ok(())
}

#[test]
fn an_exact_checkpoint_skips_old_records_then_applies_only_newer_transactions()
-> Result<(), Box<dyn Error>> {
    let journal_path = path("journal-exact");
    let checkpoint_path = path("checkpoint-exact");
    {
        let mut journal = Journal::open(&journal_path)?;
        journal.append(&batch(10, 11, 1))?;
        journal.append(&batch(20, 21, 2))?;
        journal.append(&batch(30, 31, 3))?;
    }
    let mut checkpoint = AppliedCheckpoint::open(&checkpoint_path)?;
    checkpoint.advance(&batch(20, 21, 2))?;
    let mut processor = DurableTransactionProcessor::open(&journal_path)?;
    let applied = Cell::new(Vec::<u64>::new());
    let mut apply = |batch: &TransactionBatch| -> Result<(), ApplyFailure> {
        applied.set({
            let mut values = applied.take();
            values.push(batch.commit_lsn().get());
            values
        });
        Ok(())
    };
    assert_eq!(
        recover_checkpointed(&mut processor, &mut checkpoint, &mut apply)?.get(),
        31
    );
    assert_eq!(applied.take(), vec![30]);
    assert_eq!(checkpoint.state().commit_lsn().get(), 30);
    remove(&journal_path);
    remove(&checkpoint_path);
    Ok(())
}

#[test]
fn keepalive_regression_does_not_move_received_progress_backwards() -> Result<(), Box<dyn Error>> {
    let journal_path = path("journal-progress");
    let checkpoint_path = path("checkpoint-progress");
    let mut processor = DurableTransactionProcessor::open(&journal_path)?;
    let mut checkpoint = AppliedCheckpoint::open(&checkpoint_path)?;
    let mut state = LiveReplicationState::recovered(LogSequenceNumber::new(50));
    let mut apply = |_batch: &TransactionBatch| -> Result<(), ApplyFailure> { Ok(()) };
    assert!(!state.transaction_open());
    assert_eq!(
        process_replication_event(
            &mut processor,
            &mut checkpoint,
            &mut state,
            ReplicationEvent::KeepAlive {
                wal_end: Lsn::from_u64(40),
                reply_requested: false,
                server_time_micros: 0,
            },
            &mut apply,
        )?,
        LiveEventOutcome::Continue
    );
    assert_eq!(state.progress().received_lsn.get(), 50);
    assert_eq!(state.progress().durable_lsn.get(), 50);
    assert_eq!(state.progress().applied_lsn.get(), 50);
    remove(&journal_path);
    remove(&checkpoint_path);
    Ok(())
}

#[test]
fn public_error_wrappers_preserve_sources_and_all_non_io_surfaces() {
    let journal_io: JournalError =
        io::Error::new(io::ErrorKind::PermissionDenied, "journal denied").into();
    assert!(journal_io.source().is_some());
    assert!(journal_io.to_string().contains("journal I/O error"));
    let journal_tx = JournalError::InvalidTransaction(TransactionValidationError::EndBeforeCommit);
    assert!(journal_tx.source().is_some());
    for error in [
        JournalError::InvalidMagic(1),
        JournalError::UnsupportedVersion(2),
        JournalError::CorruptHeader,
        JournalError::RecordTooLarge(1),
        JournalError::TupleTooLarge(1),
        JournalError::TooManyChanges(1),
        JournalError::IncompleteTail(1),
        JournalError::ChecksumMismatch(1),
        JournalError::HeaderPayloadMismatch(1),
        JournalError::UnexpectedEof,
        JournalError::InvalidChangeKind(9),
        JournalError::InvalidOptionTag(9),
        JournalError::TrailingBytes(1),
        JournalError::ConflictingReplay(LogSequenceNumber::new(1)),
        JournalError::DuplicateDurableRecord(LogSequenceNumber::new(1)),
    ] {
        assert!(!error.to_string().is_empty());
        assert!(error.source().is_none());
    }

    let checkpoint_io: CheckpointError =
        io::Error::new(io::ErrorKind::PermissionDenied, "checkpoint denied").into();
    assert!(checkpoint_io.source().is_some());
    assert!(checkpoint_io.to_string().contains("checkpoint I/O"));
    for error in [
        CheckpointError::InvalidMagic(1),
        CheckpointError::UnsupportedVersion(2),
        CheckpointError::ChecksumMismatch(1),
        CheckpointError::CorruptRecord,
        CheckpointError::EndBeforeCommit,
        CheckpointError::Regressed,
        CheckpointError::ConflictingCommit(LogSequenceNumber::new(1)),
        CheckpointError::DurableMismatch(LogSequenceNumber::new(1)),
        CheckpointError::MissingFromJournal(LogSequenceNumber::new(1)),
        CheckpointError::LengthOverflow,
    ] {
        assert!(!error.to_string().is_empty());
        assert!(error.source().is_none());
    }

    let processor_errors = [
        ProcessorError::<ApplyFailure>::Decode(PgOutputError::UnsupportedMessage(255)),
        ProcessorError::<ApplyFailure>::Stream(StreamError::UnknownRelation(1)),
        ProcessorError::<ApplyFailure>::Journal(JournalError::CorruptHeader),
        ProcessorError::<ApplyFailure>::Apply(ApplyFailure),
        ProcessorError::<ApplyFailure>::Poisoned,
    ];
    for error in processor_errors {
        assert!(!error.to_string().is_empty());
    }
    assert!(
        !StreamError::Transaction(TransactionBuildError::CommitWithoutBegin)
            .to_string()
            .is_empty()
    );
}
