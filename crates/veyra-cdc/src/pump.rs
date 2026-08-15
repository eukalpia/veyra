use core::fmt;

use veyra_types::LogSequenceNumber;

use crate::assembler::{AssemblerAction, AssemblerError, CdcAssembler};
use crate::durable::{AppendOutcome, DurableCdcLog, DurableLogError};
use crate::model::TransactionBatch;
use crate::postgres::{PostgresCdcError, PostgresReplicationStream};
use crate::progress::{CdcProgressError, CdcProgressTracker};

/// Result of one durability-fenced CDC pump iteration.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CdcPumpEvent {
    /// A complete transaction was fsynced before feedback could advance.
    Transaction {
        batch: TransactionBatch,
        append: AppendOutcome,
    },
    /// Bounded replay reached its configured stop point.
    StoppedAt(LogSequenceNumber),
    /// `PostgreSQL` ended replication cleanly.
    EndOfStream,
}

/// Correctness-first single-writer CDC coordinator.
///
/// The order on commit is intentionally fixed:
/// assemble -> append -> fsync -> mark durable -> `PostgreSQL` feedback.
/// No query projection is modified by this type.
pub struct CdcPump {
    stream: PostgresReplicationStream,
    assembler: CdcAssembler,
    log: DurableCdcLog,
    progress: CdcProgressTracker,
}

impl CdcPump {
    /// Builds the coordinator from already-open components.
    #[must_use]
    pub fn new(
        stream: PostgresReplicationStream,
        assembler: CdcAssembler,
        log: DurableCdcLog,
        progress: CdcProgressTracker,
    ) -> Self {
        Self {
            stream,
            assembler,
            log,
            progress,
        }
    }

    /// Returns the current replication progress.
    #[must_use]
    pub const fn progress(&self) -> CdcProgressTracker {
        self.progress
    }

    /// Returns the last locally durable LSN.
    #[must_use]
    pub const fn durable_lsn(&self) -> LogSequenceNumber {
        self.log.last_durable_lsn()
    }

    /// Receives until a complete durable transaction, stop marker or clean EOF.
    pub async fn next_durable(&mut self) -> Result<CdcPumpEvent, CdcPumpError> {
        loop {
            let Some(event) = self.stream.recv().await? else {
                return Ok(CdcPumpEvent::EndOfStream);
            };

            match self.assembler.push(event)? {
                AssemblerAction::None => {}
                AssemblerAction::WalObserved(lsn) => {
                    self.progress.observe_received(lsn)?;
                }
                AssemblerAction::StoppedAt(lsn) => {
                    self.progress.observe_received(lsn)?;
                    return Ok(CdcPumpEvent::StoppedAt(lsn));
                }
                AssemblerAction::Transaction(batch) => {
                    self.progress.observe_received(batch.end_lsn())?;
                    let append = self.log.append(&batch)?;
                    self.progress.mark_durable(batch.end_lsn())?;

                    // Feedback uses the maximum locally durable checkpoint, not the
                    // replayed batch's older LSN.
                    self.stream
                        .acknowledge_durable(self.progress.snapshot().durable())?;
                    return Ok(CdcPumpEvent::Transaction { batch, append });
                }
            }
        }
    }

    /// Gracefully stops the replication worker.
    pub async fn shutdown(&mut self) -> Result<(), CdcPumpError> {
        self.stream.shutdown().await?;
        Ok(())
    }
}

/// CDC coordinator failure.
#[derive(Debug)]
pub enum CdcPumpError {
    Transport(PostgresCdcError),
    Assembly(AssemblerError),
    Durable(DurableLogError),
    Progress(CdcProgressError),
}

impl From<PostgresCdcError> for CdcPumpError {
    fn from(value: PostgresCdcError) -> Self {
        Self::Transport(value)
    }
}

impl From<AssemblerError> for CdcPumpError {
    fn from(value: AssemblerError) -> Self {
        Self::Assembly(value)
    }
}

impl From<DurableLogError> for CdcPumpError {
    fn from(value: DurableLogError) -> Self {
        Self::Durable(value)
    }
}

impl From<CdcProgressError> for CdcPumpError {
    fn from(value: CdcProgressError) -> Self {
        Self::Progress(value)
    }
}

impl fmt::Display for CdcPumpError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Transport(error) => write!(formatter, "CDC transport failed: {error}"),
            Self::Assembly(error) => write!(formatter, "CDC transaction assembly failed: {error}"),
            Self::Durable(error) => write!(formatter, "CDC durability failed: {error}"),
            Self::Progress(error) => write!(formatter, "CDC progress failed: {error}"),
        }
    }
}

impl std::error::Error for CdcPumpError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Transport(error) => Some(error),
            Self::Assembly(error) => Some(error),
            Self::Durable(error) => Some(error),
            Self::Progress(error) => Some(error),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::BatchValidationError;

    #[test]
    fn error_wrappers_preserve_sources() {
        let assembly = CdcPumpError::Assembly(AssemblerError::CommitWithoutBegin);
        assert_eq!(
            assembly.to_string(),
            "CDC transaction assembly failed: PostgreSQL COMMIT without BEGIN"
        );
        assert!(std::error::Error::source(&assembly).is_some());

        let progress = CdcPumpError::Progress(CdcProgressError::AppliedAheadOfDurable {
            attempted: LogSequenceNumber::new(2),
            durable: LogSequenceNumber::new(1),
        });
        assert!(progress.to_string().starts_with("CDC progress failed:"));
        assert!(std::error::Error::source(&progress).is_some());

        let durable = CdcPumpError::Durable(DurableLogError::Codec(
            crate::BatchCodecError::Validation(BatchValidationError::EndBeforeCommit),
        ));
        assert!(durable.to_string().starts_with("CDC durability failed:"));
        assert!(std::error::Error::source(&durable).is_some());
    }
}
