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
    pub fn open(path: impl AsRef<Path>) -> Result<Self, JournalError> {
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
    pub fn push<E, F>(
        &mut self,
        input: &[u8],
        apply: &mut F,
    ) -> Result<ProcessingOutcome, ProcessorError<E>>
    where
        F: FnMut(&TransactionBatch) -> Result<(), E>,
    {
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
    pub fn consume_message<E, F>(
        &mut self,
        message: PgOutputMessage,
        apply: &mut F,
    ) -> Result<ProcessingOutcome, ProcessorError<E>>
    where
        F: FnMut(&TransactionBatch) -> Result<(), E>,
    {
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
    pub fn recover<E, F>(&mut self, apply: &mut F) -> Result<LogSequenceNumber, ProcessorError<E>>
    where
        F: FnMut(&TransactionBatch) -> Result<(), E>,
    {
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

impl<E> std::error::Error for ProcessorError<E>
where
    E: std::error::Error + 'static,
{
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::fs::{self, OpenOptions};
    use std::io::Write;
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    struct ApplyFailure;

    impl fmt::Display for ApplyFailure {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("projection apply failed")
        }
    }

    impl std::error::Error for ApplyFailure {}

    fn path(label: &str) -> std::path::PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "veyra-processor-{label}-{}-{nanos}.journal",
            std::process::id()
        ))
    }

    fn begin(final_lsn: u64, xid: u32) -> Vec<u8> {
        let mut bytes = vec![b'B'];
        bytes.extend_from_slice(&final_lsn.to_be_bytes());
        bytes.extend_from_slice(&0_i64.to_be_bytes());
        bytes.extend_from_slice(&xid.to_be_bytes());
        bytes
    }

    fn relation(relation_id: u32) -> Vec<u8> {
        let mut bytes = vec![b'R'];
        bytes.extend_from_slice(&relation_id.to_be_bytes());
        bytes.extend_from_slice(b"public\0inventory\0");
        bytes.push(b'd');
        bytes.extend_from_slice(&1_u16.to_be_bytes());
        bytes.push(1);
        bytes.extend_from_slice(b"available\0");
        bytes.extend_from_slice(&16_u32.to_be_bytes());
        bytes.extend_from_slice(&(-1_i32).to_be_bytes());
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

    fn commit(commit_lsn: u64, end_lsn: u64) -> Vec<u8> {
        let mut bytes = vec![b'C', 0];
        bytes.extend_from_slice(&commit_lsn.to_be_bytes());
        bytes.extend_from_slice(&end_lsn.to_be_bytes());
        bytes.extend_from_slice(&0_i64.to_be_bytes());
        bytes
    }

    fn feed_transaction<E, F>(
        processor: &mut DurableTransactionProcessor,
        apply: &mut F,
    ) -> Result<ProcessingOutcome, ProcessorError<E>>
    where
        F: FnMut(&TransactionBatch) -> Result<(), E>,
    {
        assert!(matches!(
            processor.push(&begin(20, 7), apply)?,
            ProcessingOutcome::Pending
        ));
        assert!(matches!(
            processor.push(&relation(42), apply)?,
            ProcessingOutcome::Pending
        ));
        assert!(matches!(
            processor.push(&insert(42, b"t"), apply)?,
            ProcessingOutcome::Pending
        ));
        processor.push(&commit(20, 21), apply)
    }

    #[test]
    fn acknowledgement_is_returned_only_after_durable_successful_apply(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let journal_path = path("ack-order");
        let mut processor = DurableTransactionProcessor::open(&journal_path)?;
        let mut observed = Vec::new();
        let mut apply = |batch: &TransactionBatch| -> Result<(), ApplyFailure> {
            observed.push(batch.commit_lsn());
            Ok(())
        };

        let outcome = feed_transaction(&mut processor, &mut apply)?;
        assert_eq!(
            outcome,
            ProcessingOutcome::Applied {
                acknowledge_lsn: LogSequenceNumber::new(21),
                replay: ReplayDecision::Apply,
            }
        );
        assert_eq!(observed, vec![LogSequenceNumber::new(20)]);
        assert!(!processor.is_poisoned());
        assert_eq!(
            processor.highest_durable_commit_lsn(),
            LogSequenceNumber::new(20)
        );
        drop(processor);
        assert!(fs::metadata(&journal_path)?.len() > 0);
        let _ = fs::remove_file(journal_path);
        Ok(())
    }

    #[test]
    fn failed_apply_yields_no_ack_and_poisoned_instance_cannot_continue(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let journal_path = path("failed-apply");
        {
            let mut processor = DurableTransactionProcessor::open(&journal_path)?;
            let mut fail = |_batch: &TransactionBatch| -> Result<(), ApplyFailure> {
                Err(ApplyFailure)
            };
            let result = feed_transaction(&mut processor, &mut fail);
            assert!(matches!(result, Err(ProcessorError::Apply(ApplyFailure))));
            assert!(processor.is_poisoned());
            assert_eq!(
                processor.highest_durable_commit_lsn(),
                LogSequenceNumber::new(20)
            );
            assert!(matches!(
                processor.push(&begin(30, 8), &mut fail),
                Err(ProcessorError::Poisoned)
            ));
        }

        let mut restarted = DurableTransactionProcessor::open(&journal_path)?;
        let mut applied = BTreeSet::new();
        let mut apply = |batch: &TransactionBatch| -> Result<(), ApplyFailure> {
            applied.insert(batch.fingerprint());
            Ok(())
        };
        assert_eq!(
            restarted.recover(&mut apply)?,
            LogSequenceNumber::new(21)
        );
        assert_eq!(applied.len(), 1);
        assert!(!restarted.is_poisoned());
        let _ = fs::remove_file(journal_path);
        Ok(())
    }

    #[test]
    fn duplicate_wal_is_reapplied_idempotently_before_ack(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let journal_path = path("duplicate");
        let mut processor = DurableTransactionProcessor::open(&journal_path)?;
        let mut applied = BTreeSet::new();
        let mut apply = |batch: &TransactionBatch| -> Result<(), ApplyFailure> {
            applied.insert(batch.fingerprint());
            Ok(())
        };
        assert!(matches!(
            feed_transaction(&mut processor, &mut apply)?,
            ProcessingOutcome::Applied {
                replay: ReplayDecision::Apply,
                ..
            }
        ));
        assert!(matches!(
            feed_transaction(&mut processor, &mut apply)?,
            ProcessingOutcome::Applied {
                replay: ReplayDecision::Duplicate,
                acknowledge_lsn,
            } if acknowledge_lsn == LogSequenceNumber::new(21)
        ));
        assert_eq!(applied.len(), 1);
        let _ = fs::remove_file(journal_path);
        Ok(())
    }

    #[test]
    fn consume_message_supports_structured_replication_transports(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let journal_path = path("structured");
        let mut processor = DurableTransactionProcessor::open(&journal_path)?;
        let mut applied = 0_u32;
        let mut apply = |_batch: &TransactionBatch| -> Result<(), ApplyFailure> {
            applied = applied.saturating_add(1);
            Ok(())
        };
        assert_eq!(
            processor.consume_message(
                PgOutputMessage::Relation(RelationMetadata {
                    relation_id: 5,
                    namespace: "public".to_owned(),
                    name: "rooms".to_owned(),
                    replica_identity: crate::ReplicaIdentity::Default,
                    columns: Vec::new(),
                }),
                &mut apply,
            )?,
            ProcessingOutcome::Pending
        );
        assert_eq!(
            processor.consume_message(
                PgOutputMessage::Begin {
                    final_lsn: LogSequenceNumber::new(40),
                    commit_timestamp_micros: 0,
                    xid: 10,
                },
                &mut apply,
            )?,
            ProcessingOutcome::Pending
        );
        assert_eq!(
            processor.consume_message(
                PgOutputMessage::Change(crate::RowChange::new(
                    5,
                    crate::ChangeKind::Insert,
                    None,
                    Some(vec![1]),
                )),
                &mut apply,
            )?,
            ProcessingOutcome::Pending
        );
        assert!(matches!(
            processor.consume_message(
                PgOutputMessage::Commit {
                    flags: 0,
                    commit_lsn: LogSequenceNumber::new(40),
                    end_lsn: LogSequenceNumber::new(41),
                    commit_timestamp_micros: 0,
                },
                &mut apply,
            )?,
            ProcessingOutcome::Applied {
                acknowledge_lsn,
                ..
            } if acknowledge_lsn == LogSequenceNumber::new(41)
        ));
        assert_eq!(applied, 1);
        let _ = fs::remove_file(journal_path);
        Ok(())
    }

    #[test]
    fn corrupt_durable_journal_prevents_processor_start(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let journal_path = path("corrupt");
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&journal_path)?;
        let mut corrupt = [0_u8; 32];
        corrupt[0..4].copy_from_slice(b"BAD!");
        file.write_all(&corrupt)?;
        file.sync_data()?;
        drop(file);
        assert!(DurableTransactionProcessor::open(&journal_path).is_err());
        let _ = fs::remove_file(journal_path);
        Ok(())
    }

    #[test]
    fn malformed_pgoutput_poisoning_prevents_later_ack(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let journal_path = path("decode");
        let mut processor = DurableTransactionProcessor::open(&journal_path)?;
        let mut applied = false;
        let mut apply = |_batch: &TransactionBatch| -> Result<(), ApplyFailure> {
            applied = true;
            Ok(())
        };
        let result = processor.push(&[0xff], &mut apply);
        assert!(matches!(result, Err(ProcessorError::Decode(_))));
        assert!(!applied);
        assert!(processor.is_poisoned());
        assert_eq!(
            processor.highest_durable_commit_lsn(),
            LogSequenceNumber::ZERO
        );
        assert!(matches!(
            processor.push(&begin(20, 7), &mut apply),
            Err(ProcessorError::Poisoned)
        ));
        let _ = fs::remove_file(journal_path);
        Ok(())
    }

    #[test]
    fn failed_recovery_poisoning_requires_restart(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let journal_path = path("recover-fail");
        {
            let mut processor = DurableTransactionProcessor::open(&journal_path)?;
            let mut apply = |_batch: &TransactionBatch| -> Result<(), ApplyFailure> { Ok(()) };
            let _ = feed_transaction(&mut processor, &mut apply)?;
        }
        let mut restarted = DurableTransactionProcessor::open(&journal_path)?;
        let mut fail = |_batch: &TransactionBatch| -> Result<(), ApplyFailure> { Err(ApplyFailure) };
        assert!(matches!(
            restarted.recover(&mut fail),
            Err(ProcessorError::Apply(ApplyFailure))
        ));
        assert!(restarted.is_poisoned());
        assert!(matches!(
            restarted.recover(&mut fail),
            Err(ProcessorError::Poisoned)
        ));
        let _ = fs::remove_file(journal_path);
        Ok(())
    }

    #[test]
    fn empty_recovery_returns_zero_lsn() -> Result<(), Box<dyn std::error::Error>> {
        let journal_path = path("empty");
        let mut processor = DurableTransactionProcessor::open(&journal_path)?;
        let mut apply = |_batch: &TransactionBatch| -> Result<(), ApplyFailure> { Ok(()) };
        assert_eq!(processor.recover(&mut apply)?, LogSequenceNumber::ZERO);
        assert!(!processor.is_poisoned());
        let _ = fs::remove_file(journal_path);
        Ok(())
    }

    #[test]
    fn processor_error_messages_are_stable() {
        assert_eq!(
            ProcessorError::<ApplyFailure>::Poisoned.to_string(),
            "processor is poisoned; restart required"
        );
        assert_eq!(
            ProcessorError::Apply(ApplyFailure).to_string(),
            "apply: projection apply failed"
        );
    }
}
