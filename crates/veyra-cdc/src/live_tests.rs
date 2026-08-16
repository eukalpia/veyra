use std::cell::Cell;
use std::time::{SystemTime, UNIX_EPOCH};

use super::*;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ApplyFailure;

impl fmt::Display for ApplyFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("apply failed")
    }
}
impl std::error::Error for ApplyFailure {}

fn paths(label: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let base = format!("veyra-live-{label}-{}-{nanos}", std::process::id());
    (
        std::env::temp_dir().join(format!("{base}.journal")),
        std::env::temp_dir().join(format!("{base}.checkpoint")),
    )
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

fn feed_transaction<F>(
    processor: &mut DurableTransactionProcessor,
    checkpoint: &mut AppliedCheckpoint,
    state: &mut LiveReplicationState,
    apply: &mut F,
) -> Result<LiveEventOutcome, LiveReplicationError<ApplyFailure>>
where
    F: FnMut(&TransactionBatch) -> Result<(), ApplyFailure>,
{
    process_replication_event(
        processor,
        checkpoint,
        state,
        ReplicationEvent::Begin {
            final_lsn: Lsn::from_u64(20),
            xid: 7,
            commit_time_micros: 0,
        },
        apply,
    )?;
    process_replication_event(
        processor,
        checkpoint,
        state,
        ReplicationEvent::XLogData {
            wal_start: Lsn::from_u64(19),
            wal_end: Lsn::ZERO,
            server_time_micros: 0,
            data: relation(42).into(),
        },
        apply,
    )?;
    process_replication_event(
        processor,
        checkpoint,
        state,
        ReplicationEvent::XLogData {
            wal_start: Lsn::from_u64(20),
            wal_end: Lsn::ZERO,
            server_time_micros: 0,
            data: insert(42, b"t").into(),
        },
        apply,
    )?;
    process_replication_event(
        processor,
        checkpoint,
        state,
        ReplicationEvent::Commit {
            lsn: Lsn::from_u64(20),
            end_lsn: Lsn::from_u64(21),
            commit_time_micros: 0,
        },
        apply,
    )
}

fn cleanup(journal: &Path, checkpoint: &Path) {
    let _ = std::fs::remove_file(journal);
    let _ = std::fs::remove_file(checkpoint);
}

#[test]
fn ack_requires_journal_apply_and_checkpoint_durability() -> Result<(), Box<dyn std::error::Error>>
{
    let (journal_path, checkpoint_path) = paths("ack");
    let mut processor = DurableTransactionProcessor::open(&journal_path)?;
    let mut checkpoint = AppliedCheckpoint::open(&checkpoint_path)?;
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
            ReplicationEvent::KeepAlive {
                wal_end: Lsn::from_u64(9),
                reply_requested: true,
                server_time_micros: 0,
            },
            &mut apply,
        )?,
        LiveEventOutcome::Continue
    );
    assert_eq!(
        feed_transaction(&mut processor, &mut checkpoint, &mut state, &mut apply)?,
        LiveEventOutcome::Acknowledge(LogSequenceNumber::new(21))
    );
    assert_eq!(applied.get(), 1);
    assert_eq!(checkpoint.state().end_lsn().get(), 21);
    assert_eq!(state.progress().durable_lsn.get(), 21);
    assert_eq!(state.progress().applied_lsn.get(), 21);
    cleanup(&journal_path, &checkpoint_path);
    Ok(())
}

#[test]
fn duplicate_redelivery_does_not_reapply_checkpointed_transaction()
-> Result<(), Box<dyn std::error::Error>> {
    let (journal_path, checkpoint_path) = paths("duplicate");
    let mut processor = DurableTransactionProcessor::open(&journal_path)?;
    let mut checkpoint = AppliedCheckpoint::open(&checkpoint_path)?;
    let mut state = LiveReplicationState::default();
    let applied = Cell::new(0_u32);
    let mut apply = |_batch: &TransactionBatch| -> Result<(), ApplyFailure> {
        applied.set(applied.get().saturating_add(1));
        Ok(())
    };
    let _ = feed_transaction(&mut processor, &mut checkpoint, &mut state, &mut apply)?;
    let _ = feed_transaction(&mut processor, &mut checkpoint, &mut state, &mut apply)?;
    assert_eq!(applied.get(), 1);
    assert_eq!(checkpoint.state().commit_lsn().get(), 20);
    cleanup(&journal_path, &checkpoint_path);
    Ok(())
}

#[test]
fn restart_recovery_skips_already_checkpointed_apply() -> Result<(), Box<dyn std::error::Error>> {
    let (journal_path, checkpoint_path) = paths("recover");
    {
        let mut processor = DurableTransactionProcessor::open(&journal_path)?;
        let mut checkpoint = AppliedCheckpoint::open(&checkpoint_path)?;
        let mut state = LiveReplicationState::default();
        let mut apply = |_batch: &TransactionBatch| -> Result<(), ApplyFailure> { Ok(()) };
        let _ = feed_transaction(&mut processor, &mut checkpoint, &mut state, &mut apply)?;
    }
    let mut processor = DurableTransactionProcessor::open(&journal_path)?;
    let mut checkpoint = AppliedCheckpoint::open(&checkpoint_path)?;
    let applied = Cell::new(0_u32);
    let mut apply = |_batch: &TransactionBatch| -> Result<(), ApplyFailure> {
        applied.set(applied.get().saturating_add(1));
        Ok(())
    };
    assert_eq!(
        recover_checkpointed(&mut processor, &mut checkpoint, &mut apply)?.get(),
        21
    );
    assert_eq!(applied.get(), 0);
    cleanup(&journal_path, &checkpoint_path);
    Ok(())
}

#[test]
fn malformed_raw_payload_yields_no_ack_or_checkpoint() -> Result<(), Box<dyn std::error::Error>> {
    let (journal_path, checkpoint_path) = paths("decode");
    let mut processor = DurableTransactionProcessor::open(&journal_path)?;
    let mut checkpoint = AppliedCheckpoint::open(&checkpoint_path)?;
    let mut state = LiveReplicationState::default();
    let applied = Cell::new(false);
    let mut apply = |_batch: &TransactionBatch| -> Result<(), ApplyFailure> {
        applied.set(true);
        Ok(())
    };
    assert!(matches!(
        process_replication_event(
            &mut processor,
            &mut checkpoint,
            &mut state,
            ReplicationEvent::XLogData {
                wal_start: Lsn::from_u64(1),
                wal_end: Lsn::from_u64(2),
                server_time_micros: 0,
                data: vec![0xff].into(),
            },
            &mut apply,
        ),
        Err(LiveReplicationError::Processor(ProcessorError::Decode(_)))
    ));
    assert!(!applied.get());
    assert_eq!(checkpoint.state(), crate::AppliedState::default());
    cleanup(&journal_path, &checkpoint_path);
    Ok(())
}

#[test]
fn mid_transaction_stop_and_logical_message_fail_closed() -> Result<(), Box<dyn std::error::Error>>
{
    let (journal_path, checkpoint_path) = paths("closed");
    let mut processor = DurableTransactionProcessor::open(&journal_path)?;
    let mut checkpoint = AppliedCheckpoint::open(&checkpoint_path)?;
    let mut state = LiveReplicationState::default();
    let mut apply = |_batch: &TransactionBatch| -> Result<(), ApplyFailure> { Ok(()) };
    process_replication_event(
        &mut processor,
        &mut checkpoint,
        &mut state,
        ReplicationEvent::Begin {
            final_lsn: Lsn::from_u64(30),
            xid: 8,
            commit_time_micros: 0,
        },
        &mut apply,
    )?;
    assert!(matches!(
        process_replication_event(
            &mut processor,
            &mut checkpoint,
            &mut state,
            ReplicationEvent::StoppedAt {
                reached: Lsn::from_u64(30)
            },
            &mut apply,
        ),
        Err(LiveReplicationError::StoppedMidTransaction(lsn)) if lsn.get() == 30
    ));
    assert!(matches!(
        process_replication_event(
            &mut processor,
            &mut checkpoint,
            &mut LiveReplicationState::default(),
            ReplicationEvent::Message {
                transactional: false,
                lsn: Lsn::from_u64(31),
                prefix: "unknown".to_owned(),
                content: Vec::<u8>::new().into(),
            },
            &mut apply,
        ),
        Err(LiveReplicationError::UnsupportedLogicalMessage(prefix)) if prefix == "unknown"
    ));
    cleanup(&journal_path, &checkpoint_path);
    Ok(())
}

#[test]
fn clean_stop_preserves_applied_progress() -> Result<(), Box<dyn std::error::Error>> {
    let (journal_path, checkpoint_path) = paths("stop");
    let mut processor = DurableTransactionProcessor::open(&journal_path)?;
    let mut checkpoint = AppliedCheckpoint::open(&checkpoint_path)?;
    let mut state = LiveReplicationState::recovered(LogSequenceNumber::new(5));
    let mut apply = |_batch: &TransactionBatch| -> Result<(), ApplyFailure> { Ok(()) };
    assert_eq!(
        process_replication_event(
            &mut processor,
            &mut checkpoint,
            &mut state,
            ReplicationEvent::StoppedAt {
                reached: Lsn::from_u64(8)
            },
            &mut apply,
        )?,
        LiveEventOutcome::Stop(LogSequenceNumber::new(8))
    );
    assert_eq!(state.progress().received_lsn.get(), 8);
    assert_eq!(state.progress().applied_lsn.get(), 5);
    cleanup(&journal_path, &checkpoint_path);
    Ok(())
}

#[test]
fn error_display_is_stable() {
    assert_eq!(
        LiveReplicationError::<ApplyFailure>::UnexpectedBoundaryOutcome.to_string(),
        "unexpected transaction-boundary processing outcome"
    );
}
