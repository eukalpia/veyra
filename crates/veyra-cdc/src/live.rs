use core::cell::Cell;
use core::fmt;
use std::path::Path;

use pgwire_replication::{
    Lsn, PgWireError, ReplicationClient, ReplicationConfig, ReplicationEvent,
};
use veyra_types::LogSequenceNumber;

use crate::{
    AppliedCheckpoint, CheckpointError, DurableTransactionProcessor, JournalError, PgOutputMessage,
    ProcessingOutcome, ProcessorError, TransactionBatch,
};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CdcProgress {
    pub received_lsn: LogSequenceNumber,
    pub durable_lsn: LogSequenceNumber,
    pub applied_lsn: LogSequenceNumber,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LiveEventOutcome {
    Continue,
    Acknowledge(LogSequenceNumber),
    Stop(LogSequenceNumber),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LiveReplicationState {
    progress: CdcProgress,
    transaction_open: bool,
}

impl LiveReplicationState {
    #[must_use]
    pub const fn recovered(resume_lsn: LogSequenceNumber) -> Self {
        Self {
            progress: CdcProgress {
                received_lsn: resume_lsn,
                durable_lsn: resume_lsn,
                applied_lsn: resume_lsn,
            },
            transaction_open: false,
        }
    }

    #[must_use]
    pub const fn progress(self) -> CdcProgress {
        self.progress
    }

    #[must_use]
    pub const fn transaction_open(self) -> bool {
        self.transaction_open
    }

    fn observe_received(&mut self, lsn: Lsn) {
        let lsn = local_lsn(lsn);
        if lsn > self.progress.received_lsn {
            self.progress.received_lsn = lsn;
        }
    }

    fn acknowledge(&mut self, lsn: LogSequenceNumber) -> LiveEventOutcome {
        if lsn > self.progress.received_lsn {
            self.progress.received_lsn = lsn;
        }
        if lsn > self.progress.durable_lsn {
            self.progress.durable_lsn = lsn;
        }
        if lsn > self.progress.applied_lsn {
            self.progress.applied_lsn = lsn;
        }
        LiveEventOutcome::Acknowledge(lsn)
    }
}

impl Default for LiveReplicationState {
    fn default() -> Self {
        Self::recovered(LogSequenceNumber::ZERO)
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct LiveRunSummary {
    pub events_seen: u64,
    pub acknowledgements: u64,
    pub progress: CdcProgress,
}

#[derive(Debug)]
pub enum CheckpointApplyError<E> {
    Apply(E),
    Checkpoint(CheckpointError),
}

impl<E> fmt::Display for CheckpointApplyError<E>
where
    E: fmt::Display,
{
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Apply(error) => write!(formatter, "projection apply: {error}"),
            Self::Checkpoint(error) => write!(formatter, "applied checkpoint: {error}"),
        }
    }
}

impl<E> std::error::Error for CheckpointApplyError<E>
where
    E: std::error::Error + 'static,
{
}

pub fn recover_checkpointed<E, F>(
    processor: &mut DurableTransactionProcessor,
    checkpoint: &mut AppliedCheckpoint,
    apply: &mut F,
) -> Result<LogSequenceNumber, LiveReplicationError<E>>
where
    F: FnMut(&TransactionBatch) -> Result<(), E>,
{
    let target = checkpoint.state();
    let found_target = Cell::new(target.commit_lsn() == LogSequenceNumber::ZERO);
    let mut checkpointing_apply = |batch: &TransactionBatch| {
        if batch.commit_lsn() == target.commit_lsn() {
            found_target.set(true);
        } else if batch.commit_lsn() > target.commit_lsn() && !found_target.get() {
            return Err(CheckpointApplyError::Checkpoint(
                CheckpointError::MissingFromJournal(target.commit_lsn()),
            ));
        }
        apply_checkpointed(checkpoint, batch, apply)
    };

    processor
        .recover(&mut checkpointing_apply)
        .map_err(LiveReplicationError::Processor)?;
    if !found_target.get() {
        return Err(LiveReplicationError::Checkpoint(
            CheckpointError::MissingFromJournal(target.commit_lsn()),
        ));
    }
    Ok(checkpoint.state().end_lsn())
}

pub fn process_replication_event<E, F>(
    processor: &mut DurableTransactionProcessor,
    checkpoint: &mut AppliedCheckpoint,
    state: &mut LiveReplicationState,
    event: ReplicationEvent,
    apply: &mut F,
) -> Result<LiveEventOutcome, LiveReplicationError<E>>
where
    F: FnMut(&TransactionBatch) -> Result<(), E>,
{
    match event {
        ReplicationEvent::KeepAlive { wal_end, .. } => {
            state.observe_received(wal_end);
            Ok(LiveEventOutcome::Continue)
        }
        ReplicationEvent::Begin {
            final_lsn,
            xid,
            commit_time_micros,
        } => {
            state.observe_received(final_lsn);
            let mut checkpointing_apply = |batch: &TransactionBatch| {
                apply_checkpointed(checkpoint, batch, apply)
            };
            let outcome = processor
                .consume_message(
                    PgOutputMessage::Begin {
                        final_lsn: local_lsn(final_lsn),
                        commit_timestamp_micros: commit_time_micros,
                        xid,
                    },
                    &mut checkpointing_apply,
                )
                .map_err(LiveReplicationError::Processor)?;
            if outcome != ProcessingOutcome::Pending {
                return Err(LiveReplicationError::UnexpectedBoundaryOutcome);
            }
            state.transaction_open = true;
            Ok(LiveEventOutcome::Continue)
        }
        ReplicationEvent::XLogData {
            wal_start,
            wal_end,
            data,
            ..
        } => {
            state.observe_received(wal_start);
            state.observe_received(wal_end);
            let mut checkpointing_apply = |batch: &TransactionBatch| {
                apply_checkpointed(checkpoint, batch, apply)
            };
            match processor
                .push(&data, &mut checkpointing_apply)
                .map_err(LiveReplicationError::Processor)?
            {
                ProcessingOutcome::Pending => Ok(LiveEventOutcome::Continue),
                ProcessingOutcome::Applied {
                    acknowledge_lsn, ..
                } => {
                    state.transaction_open = false;
                    Ok(state.acknowledge(acknowledge_lsn))
                }
            }
        }
        ReplicationEvent::Commit {
            lsn,
            end_lsn,
            commit_time_micros,
        } => {
            state.observe_received(end_lsn);
            let mut checkpointing_apply = |batch: &TransactionBatch| {
                apply_checkpointed(checkpoint, batch, apply)
            };
            let outcome = processor
                .consume_message(
                    PgOutputMessage::Commit {
                        flags: 0,
                        commit_lsn: local_lsn(lsn),
                        end_lsn: local_lsn(end_lsn),
                        commit_timestamp_micros: commit_time_micros,
                    },
                    &mut checkpointing_apply,
                )
                .map_err(LiveReplicationError::Processor)?;
            match outcome {
                ProcessingOutcome::Applied {
                    acknowledge_lsn, ..
                } => {
                    state.transaction_open = false;
                    Ok(state.acknowledge(acknowledge_lsn))
                }
                ProcessingOutcome::Pending => Err(LiveReplicationError::UnexpectedBoundaryOutcome),
            }
        }
        ReplicationEvent::Message { prefix, .. } => {
            Err(LiveReplicationError::UnsupportedLogicalMessage(prefix))
        }
        ReplicationEvent::StoppedAt { reached } => {
            state.observe_received(reached);
            if state.transaction_open {
                return Err(LiveReplicationError::StoppedMidTransaction(local_lsn(
                    reached,
                )));
            }
            Ok(LiveEventOutcome::Stop(local_lsn(reached)))
        }
    }
}

pub async fn run_pgwire<E, F>(
    config: ReplicationConfig,
    journal_path: impl AsRef<Path>,
    checkpoint_path: impl AsRef<Path>,
    apply: &mut F,
) -> Result<LiveRunSummary, LiveReplicationError<E>>
where
    F: FnMut(&TransactionBatch) -> Result<(), E>,
{
    let mut processor = DurableTransactionProcessor::open(journal_path)
        .map_err(LiveReplicationError::Journal)?;
    let mut checkpoint =
        AppliedCheckpoint::open(checkpoint_path).map_err(LiveReplicationError::Checkpoint)?;
    let resume_lsn = recover_checkpointed(&mut processor, &mut checkpoint, apply)?;
    let mut state = LiveReplicationState::recovered(resume_lsn);
    let config = config.with_start_lsn(Lsn::from_u64(resume_lsn.get()));
    let mut client = ReplicationClient::connect(config)
        .await
        .map_err(LiveReplicationError::Transport)?;
    let mut events_seen = 0_u64;
    let mut acknowledgements = 0_u64;

    while let Some(event) = client
        .recv()
        .await
        .map_err(LiveReplicationError::Transport)?
    {
        events_seen = events_seen.saturating_add(1);
        match process_replication_event(
            &mut processor,
            &mut checkpoint,
            &mut state,
            event,
            apply,
        )? {
            LiveEventOutcome::Continue => {}
            LiveEventOutcome::Acknowledge(lsn) => {
                client.update_applied_lsn(Lsn::from_u64(lsn.get()));
                acknowledgements = acknowledgements.saturating_add(1);
            }
            LiveEventOutcome::Stop(_) => break,
        }
    }

    Ok(LiveRunSummary {
        events_seen,
        acknowledgements,
        progress: state.progress(),
    })
}

fn apply_checkpointed<E, F>(
    checkpoint: &mut AppliedCheckpoint,
    batch: &TransactionBatch,
    apply: &mut F,
) -> Result<(), CheckpointApplyError<E>>
where
    F: FnMut(&TransactionBatch) -> Result<(), E>,
{
    let state = checkpoint.state();
    if batch.commit_lsn() < state.commit_lsn() {
        return Ok(());
    }
    if batch.commit_lsn() == state.commit_lsn() {
        if batch.end_lsn() != state.end_lsn() || batch.fingerprint() != state.fingerprint() {
            return Err(CheckpointApplyError::Checkpoint(
                CheckpointError::DurableMismatch(batch.commit_lsn()),
            ));
        }
        return Ok(());
    }

    apply(batch).map_err(CheckpointApplyError::Apply)?;
    checkpoint
        .advance(batch)
        .map_err(CheckpointApplyError::Checkpoint)?;
    Ok(())
}

fn local_lsn(lsn: Lsn) -> LogSequenceNumber {
    LogSequenceNumber::new(lsn.as_u64())
}

#[derive(Debug)]
pub enum LiveReplicationError<E> {
    Journal(JournalError),
    Checkpoint(CheckpointError),
    Processor(ProcessorError<CheckpointApplyError<E>>),
    Transport(PgWireError),
    UnsupportedLogicalMessage(String),
    StoppedMidTransaction(LogSequenceNumber),
    UnexpectedBoundaryOutcome,
}

impl<E> fmt::Display for LiveReplicationError<E>
where
    E: fmt::Display,
{
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Journal(error) => write!(formatter, "journal: {error}"),
            Self::Checkpoint(error) => write!(formatter, "checkpoint: {error}"),
            Self::Processor(error) => write!(formatter, "processor: {error}"),
            Self::Transport(error) => write!(formatter, "transport: {error}"),
            Self::UnsupportedLogicalMessage(prefix) => {
                write!(formatter, "unsupported logical replication message {prefix:?}")
            }
            Self::StoppedMidTransaction(lsn) => {
                write!(formatter, "replication stopped mid-transaction at {lsn:?}")
            }
            Self::UnexpectedBoundaryOutcome => {
                formatter.write_str("unexpected transaction-boundary processing outcome")
            }
        }
    }
}

impl<E> std::error::Error for LiveReplicationError<E>
where
    E: std::error::Error + 'static,
{
}

#[cfg(test)]
mod tests {
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
        bytes.extend_from_slice(
            &u32::try_from(value.len())
                .unwrap_or_default()
                .to_be_bytes(),
        );
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
    fn ack_requires_journal_apply_and_checkpoint_durability() -> Result<(), Box<dyn std::error::Error>> {
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
        assert_eq!(feed_transaction(&mut processor, &mut checkpoint, &mut state, &mut apply)?, LiveEventOutcome::Acknowledge(LogSequenceNumber::new(21)));
        assert_eq!(applied.get(), 1);
        assert_eq!(checkpoint.state().end_lsn().get(), 21);
        assert_eq!(state.progress().durable_lsn.get(), 21);
        assert_eq!(state.progress().applied_lsn.get(), 21);
        cleanup(&journal_path, &checkpoint_path);
        Ok(())
    }

    #[test]
    fn duplicate_redelivery_does_not_reapply_checkpointed_transaction() -> Result<(), Box<dyn std::error::Error>> {
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
        assert_eq!(recover_checkpointed(&mut processor, &mut checkpoint, &mut apply)?.get(), 21);
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
    fn mid_transaction_stop_and_logical_message_fail_closed() -> Result<(), Box<dyn std::error::Error>> {
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
                ReplicationEvent::StoppedAt { reached: Lsn::from_u64(30) },
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
                ReplicationEvent::StoppedAt { reached: Lsn::from_u64(8) },
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
}
