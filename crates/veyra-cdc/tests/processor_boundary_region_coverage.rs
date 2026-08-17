use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use pgwire_replication::{Lsn, ReplicationEvent};
use veyra_cdc::{
    AppliedCheckpoint, CheckpointApplyError, CheckpointError, DurableTransactionProcessor,
    LiveEventOutcome, LiveReplicationError, LiveReplicationState, ProcessorError,
    TransactionBatch, TransactionBuildError, process_replication_event,
};

fn path(label: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    std::env::temp_dir().join(format!(
        "veyra-processor-boundary-{label}-{}-{nanos}.bin",
        std::process::id()
    ))
}

fn begin(lsn: u64, xid: u32) -> ReplicationEvent {
    ReplicationEvent::Begin {
        final_lsn: Lsn::from_u64(lsn),
        xid,
        commit_time_micros: 0,
    }
}

fn commit(lsn: u64, end_lsn: u64) -> ReplicationEvent {
    ReplicationEvent::Commit {
        lsn: Lsn::from_u64(lsn),
        end_lsn: Lsn::from_u64(end_lsn),
        commit_time_micros: 0,
    }
}

#[test]
fn nested_begin_and_commit_without_begin_poison_the_processor() {
    let journal_path = path("nested");
    let checkpoint_path = path("nested-checkpoint");
    let mut processor =
        DurableTransactionProcessor::open(&journal_path).unwrap_or_else(|_| unreachable!());
    let mut checkpoint =
        AppliedCheckpoint::open(&checkpoint_path).unwrap_or_else(|_| unreachable!());
    let mut state = LiveReplicationState::default();
    let mut apply = |_batch: &TransactionBatch| -> Result<(), std::convert::Infallible> { Ok(()) };

    assert_eq!(
        process_replication_event(
            &mut processor,
            &mut checkpoint,
            &mut state,
            begin(10, 1),
            &mut apply,
        ),
        Ok(LiveEventOutcome::Continue)
    );
    assert!(matches!(
        process_replication_event(
            &mut processor,
            &mut checkpoint,
            &mut state,
            begin(11, 2),
            &mut apply,
        ),
        Err(LiveReplicationError::Processor(ProcessorError::Stream(
            veyra_cdc::StreamError::Transaction(TransactionBuildError::NestedTransaction)
        )))
    ));
    assert!(processor.is_poisoned());

    let journal_path_2 = path("commit-without-begin");
    let checkpoint_path_2 = path("commit-without-begin-checkpoint");
    let mut processor =
        DurableTransactionProcessor::open(&journal_path_2).unwrap_or_else(|_| unreachable!());
    let mut checkpoint =
        AppliedCheckpoint::open(&checkpoint_path_2).unwrap_or_else(|_| unreachable!());
    let mut state = LiveReplicationState::default();
    assert!(matches!(
        process_replication_event(
            &mut processor,
            &mut checkpoint,
            &mut state,
            commit(20, 21),
            &mut apply,
        ),
        Err(LiveReplicationError::Processor(ProcessorError::Stream(
            veyra_cdc::StreamError::Transaction(TransactionBuildError::CommitWithoutBegin)
        )))
    ));
    assert!(processor.is_poisoned());

    for file in [journal_path, checkpoint_path, journal_path_2, checkpoint_path_2] {
        let _ = std::fs::remove_file(file);
    }
}

#[cfg(target_os = "linux")]
#[test]
fn checkpoint_durability_failure_surfaces_through_live_apply_chain() {
    let journal_path = path("checkpoint-failure");
    let mut processor =
        DurableTransactionProcessor::open(&journal_path).unwrap_or_else(|_| unreachable!());
    let mut checkpoint = AppliedCheckpoint::open("/dev/full").unwrap_or_else(|_| unreachable!());
    let mut state = LiveReplicationState::default();
    let mut apply = |_batch: &TransactionBatch| -> Result<(), std::convert::Infallible> { Ok(()) };

    assert_eq!(
        process_replication_event(
            &mut processor,
            &mut checkpoint,
            &mut state,
            begin(30, 3),
            &mut apply,
        ),
        Ok(LiveEventOutcome::Continue)
    );
    assert!(matches!(
        process_replication_event(
            &mut processor,
            &mut checkpoint,
            &mut state,
            commit(30, 31),
            &mut apply,
        ),
        Err(LiveReplicationError::Processor(ProcessorError::Apply(
            CheckpointApplyError::Checkpoint(CheckpointError::Io(_))
        )))
    ));
    assert!(processor.is_poisoned());
    assert_eq!(state.progress().applied_lsn.get(), 0);

    let _ = std::fs::remove_file(journal_path);
}
