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

impl<E> std::error::Error for CheckpointApplyError<E> where E: std::error::Error + 'static {}

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
            let mut checkpointing_apply =
                |batch: &TransactionBatch| apply_checkpointed(checkpoint, batch, apply);
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
            let mut checkpointing_apply =
                |batch: &TransactionBatch| apply_checkpointed(checkpoint, batch, apply);
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
            let mut checkpointing_apply =
                |batch: &TransactionBatch| apply_checkpointed(checkpoint, batch, apply);
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

/// Durable, deterministic live replication state machine independent of network transport.
pub struct LiveReplicationDriver {
    processor: DurableTransactionProcessor,
    checkpoint: AppliedCheckpoint,
    state: LiveReplicationState,
    events_seen: u64,
    acknowledgements: u64,
}

impl LiveReplicationDriver {
    /// Opens durable state and replays it up to the applied checkpoint before accepting events.
    pub fn open<E, F>(
        journal_path: impl AsRef<Path>,
        checkpoint_path: impl AsRef<Path>,
        apply: &mut F,
    ) -> Result<Self, LiveReplicationError<E>>
    where
        F: FnMut(&TransactionBatch) -> Result<(), E>,
    {
        let mut processor = DurableTransactionProcessor::open(journal_path)
            .map_err(LiveReplicationError::Journal)?;
        let mut checkpoint =
            AppliedCheckpoint::open(checkpoint_path).map_err(LiveReplicationError::Checkpoint)?;
        let resume_lsn = recover_checkpointed(&mut processor, &mut checkpoint, apply)?;
        Ok(Self {
            processor,
            checkpoint,
            state: LiveReplicationState::recovered(resume_lsn),
            events_seen: 0,
            acknowledgements: 0,
        })
    }

    /// Returns the exact LSN from which an external replication transport must resume.
    #[must_use]
    pub const fn resume_lsn(&self) -> LogSequenceNumber {
        self.state.progress.applied_lsn
    }

    /// Processes one transport event and records deterministic run counters.
    pub fn process<E, F>(
        &mut self,
        event: ReplicationEvent,
        apply: &mut F,
    ) -> Result<LiveEventOutcome, LiveReplicationError<E>>
    where
        F: FnMut(&TransactionBatch) -> Result<(), E>,
    {
        self.events_seen = self.events_seen.saturating_add(1);
        let outcome = process_replication_event(
            &mut self.processor,
            &mut self.checkpoint,
            &mut self.state,
            event,
            apply,
        )?;
        if matches!(outcome, LiveEventOutcome::Acknowledge(_)) {
            self.acknowledgements = self.acknowledgements.saturating_add(1);
        }
        Ok(outcome)
    }

    /// Returns the current deterministic run summary without consuming the driver.
    #[must_use]
    pub const fn summary(&self) -> LiveRunSummary {
        LiveRunSummary {
            events_seen: self.events_seen,
            acknowledgements: self.acknowledgements,
            progress: self.state.progress,
        }
    }
}

/// Connects the deterministic driver to the external `PostgreSQL` replication transport.
///
/// This thin adapter is covered by the `PostgreSQL` integration suite rather than the hermetic
/// production-core coverage job.
// coverage: external-postgres-transport
pub async fn run_pgwire<E, F>(
    config: ReplicationConfig,
    journal_path: impl AsRef<Path>,
    checkpoint_path: impl AsRef<Path>,
    apply: &mut F,
) -> Result<LiveRunSummary, LiveReplicationError<E>>
where
    F: FnMut(&TransactionBatch) -> Result<(), E>,
{
    let mut driver = LiveReplicationDriver::open(journal_path, checkpoint_path, apply)?;
    let config = config.with_start_lsn(Lsn::from_u64(driver.resume_lsn().get()));
    let mut client = ReplicationClient::connect(config)
        .await
        .map_err(LiveReplicationError::Transport)?;

    while let Some(event) = client
        .recv()
        .await
        .map_err(LiveReplicationError::Transport)?
    {
        match driver.process(event, apply)? {
            LiveEventOutcome::Continue => {}
            LiveEventOutcome::Acknowledge(lsn) => {
                client.update_applied_lsn(Lsn::from_u64(lsn.get()));
            }
            LiveEventOutcome::Stop(_) => break,
        }
    }

    Ok(driver.summary())
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
                write!(
                    formatter,
                    "unsupported logical replication message {prefix:?}"
                )
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

impl<E> std::error::Error for LiveReplicationError<E> where E: std::error::Error + 'static {}

#[cfg(test)]
#[path = "live_tests.rs"]
mod tests;
