use std::error::Error as _;
use std::fs::{self, OpenOptions};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use pgwire_replication::{Lsn, ReplicationEvent};
use veyra_cdc::{
    AppliedCheckpoint, ChangeKind, CheckpointApplyError, CheckpointError,
    DurableTransactionProcessor, Journal, JournalError, LiveEventOutcome, LiveReplicationError,
    LiveReplicationState, PgOutputError, PgOutputMessage, ProcessorError, RelationMetadata,
    ReplayDecision, ReplicaIdentity, RowChange, StreamError, TransactionBatch,
    TransactionBuildError, TransactionStream, TransactionValidationError,
    process_replication_event, recover_checkpointed,
};
use veyra_types::LogSequenceNumber;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ApplyFailure;

impl std::fmt::Display for ApplyFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("apply failed")
    }
}

impl std::error::Error for ApplyFailure {}

fn path(label: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    std::env::temp_dir().join(format!(
        "veyra-coverage-{label}-{}-{nanos}.bin",
        std::process::id()
    ))
}

fn batch(commit: u64, end: u64, value: u8) -> TransactionBatch {
    TransactionBatch::try_new(
        7,
        LogSequenceNumber::new(commit),
        LogSequenceNumber::new(commit),
        LogSequenceNumber::new(end),
        vec![RowChange::new(
            11,
            ChangeKind::Insert,
            None,
            Some(vec![value]),
        )],
    )
    .unwrap_or_else(|_| unreachable!())
}

fn crc32c(bytes: &[u8]) -> u32 {
    let mut crc = !0_u32;
    for byte in bytes {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            let mask = 0_u32.wrapping_sub(crc & 1);
            crc = (crc >> 1) ^ (0x82f6_3b78 & mask);
        }
    }
    !crc
}

fn read_all(path: &Path) -> Vec<u8> {
    let mut bytes = Vec::new();
    OpenOptions::new()
        .read(true)
        .open(path)
        .and_then(|mut file| file.read_to_end(&mut bytes))
        .unwrap_or_else(|_| unreachable!());
    bytes
}

#[test]
fn checkpoint_boundaries_cover_debug_source_and_corruption() {
    let checkpoint_path = path("checkpoint");
    let first = batch(10, 11, 1);
    {
        let mut checkpoint =
            AppliedCheckpoint::open(&checkpoint_path).unwrap_or_else(|_| unreachable!());
        checkpoint
            .advance(&first)
            .unwrap_or_else(|_| unreachable!());
        assert!(format!("{checkpoint:?}").contains("AppliedCheckpoint"));
        assert_eq!(checkpoint.state().fingerprint(), first.fingerprint());
    }
    for error in [
        CheckpointError::InvalidMagic(4),
        CheckpointError::UnsupportedVersion(9),
        CheckpointError::ChecksumMismatch(8),
        CheckpointError::CorruptRecord,
        CheckpointError::EndBeforeCommit,
        CheckpointError::Regressed,
        CheckpointError::ConflictingCommit(LogSequenceNumber::new(10)),
        CheckpointError::DurableMismatch(LogSequenceNumber::new(10)),
        CheckpointError::MissingFromJournal(LogSequenceNumber::new(10)),
        CheckpointError::LengthOverflow,
    ] {
        assert!(!error.to_string().is_empty());
        assert!(error.source().is_none());
    }
    let io_error = CheckpointError::from(std::io::Error::other("boom"));
    assert!(io_error.source().is_some());
    assert!(io_error.to_string().contains("boom"));

    let original = read_all(&checkpoint_path);
    let bad_magic = path("checkpoint-magic");
    let mut bytes = original.clone();
    bytes[0] ^= 0xff;
    fs::write(&bad_magic, bytes).unwrap_or_else(|_| unreachable!());
    assert!(matches!(
        AppliedCheckpoint::open(&bad_magic),
        Err(CheckpointError::InvalidMagic(0))
    ));
    let bad_version = path("checkpoint-version");
    let mut bytes = original.clone();
    bytes[4..6].copy_from_slice(&99_u16.to_le_bytes());
    fs::write(&bad_version, bytes).unwrap_or_else(|_| unreachable!());
    assert!(matches!(
        AppliedCheckpoint::open(&bad_version),
        Err(CheckpointError::UnsupportedVersion(99))
    ));
    let bad_order = path("checkpoint-order");
    let mut bytes = original;
    bytes[8..16].copy_from_slice(&20_u64.to_le_bytes());
    bytes[16..24].copy_from_slice(&19_u64.to_le_bytes());
    let checksum = crc32c(&bytes[..32]);
    bytes[32..36].copy_from_slice(&checksum.to_le_bytes());
    fs::write(&bad_order, bytes).unwrap_or_else(|_| unreachable!());
    assert!(matches!(
        AppliedCheckpoint::open(&bad_order),
        Err(CheckpointError::EndBeforeCommit)
    ));
    for item in [checkpoint_path, bad_magic, bad_version, bad_order] {
        let _ = fs::remove_file(item);
    }
}

#[test]
fn journal_boundaries_cover_debug_sources_and_corruption() {
    let journal_path = path("journal");
    let first = batch(20, 21, 2);
    {
        let mut journal = Journal::open(&journal_path).unwrap_or_else(|_| unreachable!());
        assert_eq!(journal.highest_commit_lsn(), LogSequenceNumber::ZERO);
        assert!(matches!(journal.append(&first), Ok(ReplayDecision::Apply)));
        assert_eq!(journal.highest_commit_lsn(), LogSequenceNumber::new(20));
        assert!(matches!(
            journal.append(&first),
            Ok(ReplayDecision::Duplicate)
        ));
        assert!(format!("{journal:?}").contains("highest_commit_lsn"));
    }
    let bytes = read_all(&journal_path);
    for error in [
        JournalError::InvalidMagic(1),
        JournalError::UnsupportedVersion(2),
        JournalError::CorruptHeader,
        JournalError::RecordTooLarge(3),
        JournalError::TupleTooLarge(4),
        JournalError::TooManyChanges(5),
        JournalError::IncompleteTail(6),
        JournalError::ChecksumMismatch(7),
        JournalError::HeaderPayloadMismatch(8),
        JournalError::UnexpectedEof,
        JournalError::LengthOverflow,
        JournalError::InvalidChangeKind(9),
        JournalError::InvalidOptionTag(10),
        JournalError::TrailingBytes(11),
        JournalError::ConflictingReplay(LogSequenceNumber::new(12)),
        JournalError::DuplicateDurableRecord(LogSequenceNumber::new(13)),
    ] {
        assert!(!error.to_string().is_empty());
        assert!(error.source().is_none());
    }
    let invalid_tx = JournalError::InvalidTransaction(TransactionValidationError::EndBeforeCommit);
    assert!(invalid_tx.source().is_some());
    let io_error = JournalError::from(std::io::Error::other("io"));
    assert!(io_error.source().is_some());

    let duplicate = path("journal-duplicate");
    let mut doubled = bytes.clone();
    doubled.extend_from_slice(&bytes);
    fs::write(&duplicate, doubled).unwrap_or_else(|_| unreachable!());
    assert!(matches!(
        Journal::open(&duplicate),
        Err(JournalError::DuplicateDurableRecord(_))
    ));
    let invalid_magic = path("journal-magic");
    let mut corrupted = bytes.clone();
    corrupted[0] ^= 0xff;
    fs::write(&invalid_magic, corrupted).unwrap_or_else(|_| unreachable!());
    assert!(matches!(
        Journal::open(&invalid_magic),
        Err(JournalError::InvalidMagic(0))
    ));
    let unsupported = path("journal-version");
    let mut corrupted = bytes.clone();
    corrupted[4..6].copy_from_slice(&99_u16.to_le_bytes());
    fs::write(&unsupported, corrupted).unwrap_or_else(|_| unreachable!());
    assert!(matches!(
        Journal::open(&unsupported),
        Err(JournalError::UnsupportedVersion(99))
    ));
    let mismatch = path("journal-mismatch");
    let mut corrupted = bytes.clone();
    corrupted[16..24].copy_from_slice(&999_u64.to_le_bytes());
    fs::write(&mismatch, corrupted).unwrap_or_else(|_| unreachable!());
    assert!(matches!(
        Journal::open(&mismatch),
        Err(JournalError::HeaderPayloadMismatch(0))
    ));
    let checksum = path("journal-checksum");
    let mut corrupted = bytes;
    corrupted[32] ^= 0xff;
    fs::write(&checksum, corrupted).unwrap_or_else(|_| unreachable!());
    assert!(matches!(
        Journal::open(&checksum),
        Err(JournalError::ChecksumMismatch(0))
    ));
    for item in [
        journal_path,
        duplicate,
        invalid_magic,
        unsupported,
        mismatch,
        checksum,
    ] {
        let _ = fs::remove_file(item);
    }
}

#[test]
fn processor_live_and_stream_public_boundaries_are_exercised() {
    let journal_path = path("processor");
    let checkpoint_path = path("checkpoint-live");
    let mut processor =
        DurableTransactionProcessor::open(&journal_path).unwrap_or_else(|_| unreachable!());
    assert_eq!(
        processor.highest_durable_commit_lsn(),
        LogSequenceNumber::ZERO
    );
    assert!(format!("{processor:?}").contains("DurableTransactionProcessor"));
    for error in [
        ProcessorError::<ApplyFailure>::Decode(PgOutputError::UnsupportedMessage(0xff)),
        ProcessorError::Stream(StreamError::UnknownRelation(7)),
        ProcessorError::Apply(ApplyFailure),
        ProcessorError::Poisoned,
    ] {
        assert!(!error.to_string().is_empty());
    }
    for error in [
        StreamError::UnknownRelation(1),
        StreamError::Transaction(TransactionBuildError::CommitWithoutBegin),
    ] {
        assert!(!error.to_string().is_empty());
    }
    let converted: StreamError = TransactionBuildError::CommitWithoutBegin.into();
    assert_eq!(
        converted,
        StreamError::Transaction(TransactionBuildError::CommitWithoutBegin)
    );
    assert!(
        !TransactionValidationError::EndBeforeCommit
            .to_string()
            .is_empty()
    );

    let mut stream = TransactionStream::new();
    assert!(!stream.transaction_open());
    assert!(stream.relation(77).is_none());
    stream
        .consume(PgOutputMessage::Relation(RelationMetadata {
            relation_id: 77,
            namespace: "public".to_owned(),
            name: "inventory".to_owned(),
            replica_identity: ReplicaIdentity::Default,
            columns: Vec::new(),
        }))
        .unwrap_or_else(|_| unreachable!());
    assert_eq!(
        stream.relation(77).map(|relation| relation.name.as_str()),
        Some("inventory")
    );
    stream
        .consume(PgOutputMessage::Begin {
            final_lsn: LogSequenceNumber::new(9),
            commit_timestamp_micros: 0,
            xid: 1,
        })
        .unwrap_or_else(|_| unreachable!());
    assert!(stream.transaction_open());

    let mut checkpoint =
        AppliedCheckpoint::open(&checkpoint_path).unwrap_or_else(|_| unreachable!());
    let mut state = LiveReplicationState::default();
    assert!(!state.transaction_open());
    let mut apply = |_batch: &TransactionBatch| -> Result<(), ApplyFailure> { Ok(()) };
    assert!(matches!(
        process_replication_event(
            &mut processor,
            &mut checkpoint,
            &mut state,
            ReplicationEvent::KeepAlive {
                wal_end: Lsn::from_u64(5),
                reply_requested: false,
                server_time_micros: 0,
            },
            &mut apply,
        ),
        Ok(LiveEventOutcome::Continue)
    ));
    assert_eq!(state.progress().received_lsn.get(), 5);
    assert!(matches!(
        recover_checkpointed(&mut processor, &mut checkpoint, &mut apply),
        Ok(LogSequenceNumber::ZERO)
    ));

    let errors = [
        LiveReplicationError::<ApplyFailure>::Checkpoint(CheckpointError::LengthOverflow),
        LiveReplicationError::Processor(ProcessorError::Apply(CheckpointApplyError::Apply(
            ApplyFailure,
        ))),
        LiveReplicationError::UnsupportedLogicalMessage("x".to_owned()),
        LiveReplicationError::StoppedMidTransaction(LogSequenceNumber::new(1)),
        LiveReplicationError::UnexpectedBoundaryOutcome,
    ];
    for error in errors {
        assert!(!error.to_string().is_empty());
    }
    assert!(matches!(
        PgOutputMessage::Change(RowChange::new(1, ChangeKind::Delete, Some(vec![1]), None)),
        PgOutputMessage::Change(_)
    ));
    let _ = fs::remove_file(journal_path);
    let _ = fs::remove_file(checkpoint_path);
}

#[test]
fn missing_parent_directories_return_typed_io_errors() {
    let missing_root = path("missing-parent");
    let journal_path = missing_root.join("journal.bin");
    let checkpoint_path = missing_root.join("checkpoint.bin");

    let journal_error = Journal::open(&journal_path)
        .err()
        .unwrap_or_else(|| unreachable!());
    assert!(matches!(journal_error, JournalError::Io(_)));
    assert!(journal_error.source().is_some());

    let checkpoint_error = AppliedCheckpoint::open(&checkpoint_path)
        .err()
        .unwrap_or_else(|| unreachable!());
    assert!(matches!(checkpoint_error, CheckpointError::Io(_)));
    assert!(checkpoint_error.source().is_some());
}
