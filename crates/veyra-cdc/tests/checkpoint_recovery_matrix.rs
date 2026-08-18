use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use veyra_cdc::{
    AppliedCheckpoint, ChangeKind, CheckpointAdvance, CheckpointError, RowChange, TransactionBatch,
};
use veyra_types::LogSequenceNumber;

const RECORD_LEN: usize = 36;
const BODY_LEN: usize = 32;
static PATH_SEQUENCE: AtomicU64 = AtomicU64::new(0);

fn path(label: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let sequence = PATH_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "veyra-checkpoint-matrix-{label}-{}-{nanos}-{sequence}.bin",
        std::process::id()
    ))
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

fn batch(commit: u64, end: u64, value: u8) -> TransactionBatch {
    TransactionBatch::try_new(
        9,
        LogSequenceNumber::new(commit),
        LogSequenceNumber::new(commit),
        LogSequenceNumber::new(end),
        vec![RowChange::new(
            3,
            ChangeKind::Update,
            Some(vec![value]),
            Some(vec![value.saturating_add(1)]),
        )],
    )
    .unwrap_or_else(|_| unreachable!())
}

fn records() -> Vec<u8> {
    let source = path("source");
    let mut checkpoint = AppliedCheckpoint::open(&source).unwrap_or_else(|_| unreachable!());
    checkpoint
        .advance(&batch(10, 11, 1))
        .unwrap_or_else(|_| unreachable!());
    checkpoint
        .advance(&batch(20, 21, 2))
        .unwrap_or_else(|_| unreachable!());
    drop(checkpoint);
    let bytes = fs::read(&source).unwrap_or_else(|_| unreachable!());
    let _ = fs::remove_file(source);
    bytes
}

fn rewrite_crc(record: &mut [u8]) {
    let checksum = crc32c(&record[..BODY_LEN]);
    record[BODY_LEN..RECORD_LEN].copy_from_slice(&checksum.to_le_bytes());
}

#[test]
fn duplicate_checkpoint_records_are_idempotent_on_recovery() {
    let bytes = records();
    let target = path("duplicate");
    let mut duplicated = Vec::new();
    duplicated.extend_from_slice(&bytes[..RECORD_LEN]);
    duplicated.extend_from_slice(&bytes[..RECORD_LEN]);
    duplicated.extend_from_slice(&bytes[RECORD_LEN..]);
    fs::write(&target, duplicated).unwrap_or_else(|_| unreachable!());

    let checkpoint = AppliedCheckpoint::open(&target).unwrap_or_else(|_| unreachable!());
    assert_eq!(checkpoint.state().commit_lsn(), LogSequenceNumber::new(20));
    assert_eq!(checkpoint.state().end_lsn(), LogSequenceNumber::new(21));
    let _ = fs::remove_file(target);
}

#[test]
fn regressed_and_conflicting_records_fail_closed_during_recovery() {
    let bytes = records();

    let regressed = path("regressed");
    let mut variant = bytes.clone();
    let second = &mut variant[RECORD_LEN..RECORD_LEN * 2];
    second[8..16].copy_from_slice(&5_u64.to_le_bytes());
    second[16..24].copy_from_slice(&6_u64.to_le_bytes());
    rewrite_crc(second);
    fs::write(&regressed, variant).unwrap_or_else(|_| unreachable!());
    assert!(matches!(
        AppliedCheckpoint::open(&regressed),
        Err(CheckpointError::Regressed)
    ));

    let conflicting = path("conflicting");
    let mut variant = bytes;
    let second = &mut variant[RECORD_LEN..RECORD_LEN * 2];
    second[8..16].copy_from_slice(&10_u64.to_le_bytes());
    second[16..24].copy_from_slice(&12_u64.to_le_bytes());
    rewrite_crc(second);
    fs::write(&conflicting, variant).unwrap_or_else(|_| unreachable!());
    assert!(matches!(
        AppliedCheckpoint::open(&conflicting),
        Err(CheckpointError::ConflictingCommit(lsn)) if lsn == LogSequenceNumber::new(10)
    ));

    let _ = fs::remove_file(regressed);
    let _ = fs::remove_file(conflicting);
}

#[test]
fn every_incomplete_checkpoint_prefix_is_truncated() {
    let bytes = records();
    let target = path("prefix");
    for end in 0..RECORD_LEN {
        fs::write(&target, &bytes[..end]).unwrap_or_else(|_| unreachable!());
        let checkpoint = AppliedCheckpoint::open(&target).unwrap_or_else(|_| unreachable!());
        assert_eq!(checkpoint.state().commit_lsn(), LogSequenceNumber::ZERO);
        drop(checkpoint);
        assert_eq!(
            fs::metadata(&target)
                .unwrap_or_else(|_| unreachable!())
                .len(),
            0
        );
    }
    let _ = fs::remove_file(target);
}

#[test]
fn duplicate_advance_is_reported_without_new_durable_record() {
    let target = path("advance-duplicate");
    let first = batch(10, 11, 1);
    let mut checkpoint = AppliedCheckpoint::open(&target).unwrap_or_else(|_| unreachable!());
    assert_eq!(
        checkpoint
            .advance(&first)
            .unwrap_or_else(|_| unreachable!()),
        CheckpointAdvance::Advanced
    );
    let len = fs::metadata(&target)
        .unwrap_or_else(|_| unreachable!())
        .len();
    assert_eq!(
        checkpoint
            .advance(&first)
            .unwrap_or_else(|_| unreachable!()),
        CheckpointAdvance::Duplicate
    );
    assert_eq!(
        fs::metadata(&target)
            .unwrap_or_else(|_| unreachable!())
            .len(),
        len
    );
    let _ = fs::remove_file(target);
}
