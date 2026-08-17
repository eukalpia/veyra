use std::cell::Cell;
use std::collections::BTreeSet;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::time::{SystemTime, UNIX_EPOCH};

use super::*;
use crate::RelationMetadata;

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

fn feed_transaction<E>(
    processor: &mut DurableTransactionProcessor,
    apply: &mut dyn FnMut(&TransactionBatch) -> Result<(), E>,
) -> Result<ProcessingOutcome, ProcessorError<E>> {
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
fn acknowledgement_is_returned_only_after_durable_successful_apply()
-> Result<(), Box<dyn std::error::Error>> {
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
fn failed_apply_yields_no_ack_and_poisoned_instance_cannot_continue()
-> Result<(), Box<dyn std::error::Error>> {
    let journal_path = path("failed-apply");
    {
        let mut processor = DurableTransactionProcessor::open(&journal_path)?;
        let mut fail =
            |_batch: &TransactionBatch| -> Result<(), ApplyFailure> { Err(ApplyFailure) };
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
    assert_eq!(restarted.recover(&mut apply)?, LogSequenceNumber::new(21));
    assert_eq!(applied.len(), 1);
    assert!(!restarted.is_poisoned());
    let _ = fs::remove_file(journal_path);
    Ok(())
}

#[test]
fn duplicate_wal_is_reapplied_idempotently_before_ack() -> Result<(), Box<dyn std::error::Error>> {
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
fn consume_message_supports_structured_replication_transports()
-> Result<(), Box<dyn std::error::Error>> {
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
fn stream_failure_poisoning_prevents_later_messages() -> Result<(), Box<dyn std::error::Error>> {
    let journal_path = path("stream-failure");
    let mut processor = DurableTransactionProcessor::open(&journal_path)?;
    let mut apply = |_batch: &TransactionBatch| -> Result<(), ApplyFailure> { Ok(()) };
    let commit_without_begin = PgOutputMessage::Commit {
        flags: 0,
        commit_lsn: LogSequenceNumber::new(10),
        end_lsn: LogSequenceNumber::new(11),
        commit_timestamp_micros: 0,
    };
    assert!(matches!(
        processor.consume_message(commit_without_begin, &mut apply),
        Err(ProcessorError::Stream(_))
    ));
    assert!(processor.is_poisoned());
    assert!(matches!(
        processor.consume_message(
            PgOutputMessage::Relation(RelationMetadata {
                relation_id: 1,
                namespace: "public".to_owned(),
                name: "rooms".to_owned(),
                replica_identity: crate::ReplicaIdentity::Default,
                columns: Vec::new(),
            }),
            &mut apply,
        ),
        Err(ProcessorError::Poisoned)
    ));
    let _ = fs::remove_file(journal_path);
    Ok(())
}

#[cfg(target_os = "linux")]
#[test]
fn journal_write_failure_poisoning_prevents_acknowledgement()
-> Result<(), Box<dyn std::error::Error>> {
    let mut processor = DurableTransactionProcessor::open(std::path::Path::new("/dev/full"))?;
    let mut apply = |_batch: &TransactionBatch| -> Result<(), ApplyFailure> { Ok(()) };
    assert!(matches!(
        feed_transaction(&mut processor, &mut apply),
        Err(ProcessorError::Journal(JournalError::Io(_)))
    ));
    assert!(processor.is_poisoned());
    Ok(())
}

#[test]
fn corrupt_durable_journal_prevents_processor_start() -> Result<(), Box<dyn std::error::Error>> {
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
fn malformed_pgoutput_poisoning_prevents_later_ack() -> Result<(), Box<dyn std::error::Error>> {
    let journal_path = path("decode");
    let mut processor = DurableTransactionProcessor::open(&journal_path)?;
    let applied = Cell::new(false);
    let mut apply = |_batch: &TransactionBatch| -> Result<(), ApplyFailure> {
        applied.set(true);
        Ok(())
    };
    let result = processor.push(&[0xff], &mut apply);
    assert!(matches!(result, Err(ProcessorError::Decode(_))));
    assert!(!applied.get());
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
fn failed_recovery_poisoning_requires_restart() -> Result<(), Box<dyn std::error::Error>> {
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
