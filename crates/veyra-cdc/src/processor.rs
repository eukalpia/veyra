use core::fmt;
use std::path::Path;

use veyra_types::LogSequenceNumber;

use crate::{
    Journal, JournalError, PgOutputDecoder, PgOutputError, PgOutputMessage, ReplayDecision,
    StreamError, TransactionBatch, TransactionStream,
};

/// Result of consuming one logical replication protocol message.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProcessingOutcome {
    /// The message did not finish a `PostgreSQL` transaction, so nothing may be acknowledged yet.
    Pending,
    /// A complete transaction is durable and has been successfully applied to the projection.
    /// The returned LSN is the only value the network adapter may acknowledge upstream.
    Applied {
        acknowledge_lsn: LogSequenceNumber,
        replay: ReplayDecision,
    },
}

/// Crash-safe transaction processing boundary.
///
/// Ordering is deliberately fixed:
///
/// 1. decode/assemble the complete transaction;
/// 2. append and `sync_data` the durable journal;
/// 3. apply the transaction to the derived projection;
/// 4. return the acknowledgement LSN.
///
/// A caller must never acknowledge `PostgreSQL` before `Applied` is returned. Any decode, stream,
/// journal, or apply failure permanently poisons this processor instance; restart/recovery is then
/// required before more WAL may be accepted.
pub struct DurableTransactionProcessor {
    stream: TransactionStream,
    journal: Journal,
    poisoned: bool,
}

impl fmt::Debug for DurableTransactionProcessor {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DurableTransactionProcessor")
            .field("stream", &self.stream)
            .field("journal", &self.journal)
            .field("poisoned", &self.poisoned)
            .finish()
    }
}

impl DurableTransactionProcessor {
    pub fn open(path: &Path) -> Result<Self, JournalError> {
        Ok(Self {
            stream: TransactionStream::new(),
            journal: Journal::open(path)?,
            poisoned: false,
        })
    }

    /// Processes one raw `pgoutput` protocol message.
    ///
    /// `apply` must itself be idempotent because `PostgreSQL` may redeliver a transaction after a
    /// crash and a transaction durably written before a failed apply is deliberately retried.
    pub fn push<E>(
        &mut self,
        input: &[u8],
        apply: &mut dyn FnMut(&TransactionBatch) -> Result<(), E>,
    ) -> Result<ProcessingOutcome, ProcessorError<E>> {
        self.ensure_usable()?;
        let message = match PgOutputDecoder::decode(input) {
            Ok(message) => message,
            Err(error) => {
                self.poisoned = true;
                return Err(ProcessorError::Decode(error));
            }
        };
        self.consume_message(message, apply)
    }

    /// Consumes one already-decoded `pgoutput` message.
    ///
    /// This is the integration boundary for replication transports that expose transaction
    /// boundaries separately from raw `XLogData` payloads.
    pub fn consume_message<E>(
        &mut self,
        message: PgOutputMessage,
        apply: &mut dyn FnMut(&TransactionBatch) -> Result<(), E>,
    ) -> Result<ProcessingOutcome, ProcessorError<E>> {
        self.ensure_usable()?;
        let batch = match self.stream.consume(message) {
            Ok(Some(batch)) => batch,
            Ok(None) => return Ok(ProcessingOutcome::Pending),
            Err(error) => {
                self.poisoned = true;
                return Err(ProcessorError::Stream(error));
            }
        };
        let replay = match self.journal.append(&batch) {
            Ok(replay) => replay,
            Err(error) => {
                self.poisoned = true;
                return Err(ProcessorError::Journal(error));
            }
        };
        if let Err(error) = apply(&batch) {
            self.poisoned = true;
            return Err(ProcessorError::Apply(error));
        }
        Ok(ProcessingOutcome::Applied {
            acknowledge_lsn: batch.end_lsn(),
            replay,
        })
    }

    /// Replays every durable transaction after process restart before live WAL is acknowledged.
    ///
    /// The returned LSN is zero for an empty journal. If any projection apply fails, no resume LSN
    /// is returned and the processor is poisoned so the caller remains fail-closed.
    pub fn recover<E>(
        &mut self,
        apply: &mut dyn FnMut(&TransactionBatch) -> Result<(), E>,
    ) -> Result<LogSequenceNumber, ProcessorError<E>> {
        self.ensure_usable()?;
        let records = match self.journal.replay() {
            Ok(records) => records,
            Err(error) => {
                self.poisoned = true;
                return Err(ProcessorError::Journal(error));
            }
        };
        let mut applied = LogSequenceNumber::ZERO;
        for batch in &records {
            if let Err(error) = apply(batch) {
                self.poisoned = true;
                return Err(ProcessorError::Apply(error));
            }
            applied = batch.end_lsn();
        }
        Ok(applied)
    }

    #[must_use]
    pub fn highest_durable_commit_lsn(&self) -> LogSequenceNumber {
        self.journal.highest_commit_lsn()
    }

    #[must_use]
    pub const fn is_poisoned(&self) -> bool {
        self.poisoned
    }

    fn ensure_usable<E>(&self) -> Result<(), ProcessorError<E>> {
        if self.poisoned {
            Err(ProcessorError::Poisoned)
        } else {
            Ok(())
        }
    }
}

#[derive(Debug)]
pub enum ProcessorError<E> {
    Decode(PgOutputError),
    Stream(StreamError),
    Journal(JournalError),
    Apply(E),
    Poisoned,
}

impl<E: fmt::Display> fmt::Display for ProcessorError<E> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Decode(error) => write!(formatter, "decode: {error}"),
            Self::Stream(error) => write!(formatter, "stream: {error}"),
            Self::Journal(error) => write!(formatter, "journal: {error}"),
            Self::Apply(error) => write!(formatter, "apply: {error}"),
            Self::Poisoned => formatter.write_str("processor is poisoned; restart required"),
        }
    }
}

impl<E> std::error::Error for ProcessorError<E> where E: std::error::Error + 'static {}

#[cfg(test)]
#[path = "processor_tests.rs"]
mod tests;
