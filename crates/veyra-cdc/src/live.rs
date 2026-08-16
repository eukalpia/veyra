use core::fmt;
use std::path::Path;

use pgwire_replication::{
    Lsn, PgWireError, ReplicationClient, ReplicationConfig, ReplicationEvent,
};
use veyra_types::LogSequenceNumber;

use crate::{
    DurableTransactionProcessor, JournalError, PgOutputMessage, ProcessingOutcome, ProcessorError,
    TransactionBatch,
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

pub fn process_replication_event<E, F>(
    processor: &mut DurableTransactionProcessor,
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
            let outcome = processor
                .consume_message(
                    PgOutputMessage::Begin {
                        final_lsn: local_lsn(final_lsn),
                        commit_timestamp_micros: commit_time_micros,
                        xid,
                    },
                    apply,
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
            match processor
                .push(&data, apply)
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
            let outcome = processor
                .consume_message(
                    PgOutputMessage::Commit {
                        flags: 0,
                        commit_lsn: local_lsn(lsn),
                        end_lsn: local_lsn(end_lsn),
                        commit_timestamp_micros: commit_time_micros,
                    },
                    apply,
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
    apply: &mut F,
) -> Result<LiveRunSummary, LiveReplicationError<E>>
where
    F: FnMut(&TransactionBatch) -> Result<(), E>,
{
    let mut processor =
        DurableTransactionProcessor::open(journal_path).map_err(LiveReplicationError::Journal)?;
    let resume_lsn = processor
        .recover(apply)
        .map_err(LiveReplicationError::Processor)?;
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
        match process_replication_event(&mut processor, &mut state, event, apply)? {
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

fn local_lsn(lsn: Lsn) -> LogSequenceNumber {
    LogSequenceNumber::new(lsn.as_u64())
}

#[derive(Debug)]
pub enum LiveReplicationError<E> {
    Journal(JournalError),
    Processor(ProcessorError<E>),
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

    fn path(label: &str) -> std::path::PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "veyra-live-{label}-{}-{nanos}.journal",
            std::process::id()
        ))
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

    #[test]
    fn structured_pgwire_events_ack_only_after_durable_apply()
    -> Result<(), Box<dyn std::error::Error>> {
        let journal = path("ack");
        let mut processor = DurableTransactionProcessor::open(&journal)?;
        let mut state = LiveReplicationState::default();
        let applied = Cell::new(0_u32);
        let mut apply = |_batch: &TransactionBatch| -> Result<(), ApplyFailure> {
            applied.set(applied.get().saturating_add(1));
            Ok(())
        };

        assert_eq!(
            process_replication_event(
                &mut processor,
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
        assert_eq!(state.progress().received_lsn.get(), 9);
        assert_eq!(state.progress().applied_lsn, LogSequenceNumber::ZERO);

        assert_eq!(
            process_replication_event(
                &mut processor,
                &mut state,
                ReplicationEvent::Begin {
                    final_lsn: Lsn::from_u64(20),
                    xid: 7,
                    commit_time_micros: 0,
                },
                &mut apply,
            )?,
            LiveEventOutcome::Continue
        );
        assert!(state.transaction_open());
        assert_eq!(
            process_replication_event(
                &mut processor,
                &mut state,
                ReplicationEvent::XLogData {
                    wal_start: Lsn::from_u64(19),
                    wal_end: Lsn::ZERO,
                    server_time_micros: 0,
                    data: relation(42).into(),
                },
                &mut apply,
            )?,
            LiveEventOutcome::Continue
        );
        assert_eq!(
            process_replication_event(
                &mut processor,
                &mut state,
                ReplicationEvent::XLogData {
                    wal_start: Lsn::from_u64(20),
                    wal_end: Lsn::ZERO,
                    server_time_micros: 0,
                    data: insert(42, b"t").into(),
                },
                &mut apply,
            )?,
            LiveEventOutcome::Continue
        );
        assert_eq!(
            process_replication_event(
                &mut processor,
                &mut state,
                ReplicationEvent::Commit {
                    lsn: Lsn::from_u64(20),
                    end_lsn: Lsn::from_u64(21),
                    commit_time_micros: 0,
                },
                &mut apply,
            )?,
            LiveEventOutcome::Acknowledge(LogSequenceNumber::new(21))
        );
        assert_eq!(applied.get(), 1);
        assert!(!state.transaction_open());
        assert_eq!(state.progress().received_lsn.get(), 21);
        assert_eq!(state.progress().durable_lsn.get(), 21);
        assert_eq!(state.progress().applied_lsn.get(), 21);
        let _ = std::fs::remove_file(journal);
        Ok(())
    }

    #[test]
    fn adapter_fails_closed_for_mid_transaction_stop_and_logical_message()
    -> Result<(), Box<dyn std::error::Error>> {
        let journal = path("closed");
        let mut processor = DurableTransactionProcessor::open(&journal)?;
        let mut state = LiveReplicationState::default();
        let mut apply = |_batch: &TransactionBatch| -> Result<(), ApplyFailure> { Ok(()) };
        process_replication_event(
            &mut processor,
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
                &mut state,
                ReplicationEvent::StoppedAt {
                    reached: Lsn::from_u64(30),
                },
                &mut apply,
            ),
            Err(LiveReplicationError::StoppedMidTransaction(lsn)) if lsn.get() == 30
        ));
        assert!(matches!(
            process_replication_event(
                &mut processor,
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
        let _ = std::fs::remove_file(journal);
        Ok(())
    }

    #[test]
    fn malformed_raw_payload_yields_no_ack_and_poisoned_processor()
    -> Result<(), Box<dyn std::error::Error>> {
        let journal = path("decode");
        let mut processor = DurableTransactionProcessor::open(&journal)?;
        let mut state = LiveReplicationState::default();
        let applied = Cell::new(false);
        let mut apply = |_batch: &TransactionBatch| -> Result<(), ApplyFailure> {
            applied.set(true);
            Ok(())
        };
        assert!(matches!(
            process_replication_event(
                &mut processor,
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
        assert!(processor.is_poisoned());
        assert_eq!(state.progress().applied_lsn, LogSequenceNumber::ZERO);
        let _ = std::fs::remove_file(journal);
        Ok(())
    }

    #[test]
    fn clean_stop_is_explicit() -> Result<(), Box<dyn std::error::Error>> {
        let journal = path("stop");
        let mut processor = DurableTransactionProcessor::open(&journal)?;
        let mut state = LiveReplicationState::recovered(LogSequenceNumber::new(5));
        let mut apply = |_batch: &TransactionBatch| -> Result<(), ApplyFailure> { Ok(()) };
        assert_eq!(
            process_replication_event(
                &mut processor,
                &mut state,
                ReplicationEvent::StoppedAt {
                    reached: Lsn::from_u64(8),
                },
                &mut apply,
            )?,
            LiveEventOutcome::Stop(LogSequenceNumber::new(8))
        );
        assert_eq!(state.progress().received_lsn.get(), 8);
        assert_eq!(state.progress().applied_lsn.get(), 5);
        let _ = std::fs::remove_file(journal);
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
