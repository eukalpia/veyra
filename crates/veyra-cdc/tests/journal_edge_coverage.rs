use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use veyra_cdc::{ChangeKind, Journal, JournalError, ReplayDecision, RowChange, TransactionBatch};
use veyra_types::LogSequenceNumber;

const HEADER_LEN: usize = 32;
const MAX_RECORD_BYTES: usize = 64 * 1024 * 1024;

fn path(label: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    std::env::temp_dir().join(format!(
        "veyra-journal-edge-{label}-{}-{nanos}.bin",
        std::process::id()
    ))
}

fn batch() -> TransactionBatch {
    TransactionBatch::try_new(
        7,
        LogSequenceNumber::new(20),
        LogSequenceNumber::new(20),
        LogSequenceNumber::new(21),
        vec![RowChange::new(11, ChangeKind::Insert, None, Some(vec![9]))],
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

fn valid_bytes(label: &str) -> (PathBuf, Vec<u8>) {
    let file_path = path(label);
    let mut journal = Journal::open(&file_path).unwrap_or_else(|_| unreachable!());
    assert!(matches!(
        journal.append(&batch()),
        Ok(ReplayDecision::Apply)
    ));
    drop(journal);
    let bytes = fs::read(&file_path).unwrap_or_else(|_| unreachable!());
    (file_path, bytes)
}

fn payload_len(bytes: &[u8]) -> usize {
    usize::try_from(u64::from_le_bytes(
        bytes[8..16].try_into().unwrap_or_else(|_| unreachable!()),
    ))
    .unwrap_or_else(|_| unreachable!())
}

fn rewrite_crc(bytes: &mut [u8]) {
    let len = payload_len(bytes);
    let crc_offset = HEADER_LEN + len;
    let checksum = crc32c(&bytes[HEADER_LEN..crc_offset]);
    bytes[crc_offset..crc_offset + 4].copy_from_slice(&checksum.to_le_bytes());
}

fn write_variant(path: &Path, bytes: &[u8]) {
    fs::write(path, bytes).unwrap_or_else(|_| unreachable!());
}

#[test]
fn strict_replay_rejects_an_incomplete_tail() {
    let file_path = path("tail");
    let mut journal = Journal::open(&file_path).unwrap_or_else(|_| unreachable!());
    journal.append(&batch()).unwrap_or_else(|_| unreachable!());
    let valid_len = fs::metadata(&file_path)
        .unwrap_or_else(|_| unreachable!())
        .len();
    OpenOptions::new()
        .append(true)
        .open(&file_path)
        .and_then(|mut file| file.write_all(&[1, 2, 3]))
        .unwrap_or_else(|_| unreachable!());

    assert!(matches!(
        journal.replay(),
        Err(JournalError::IncompleteTail(offset)) if offset == valid_len
    ));
    let _ = fs::remove_file(file_path);
}

#[test]
fn declared_record_and_change_cardinality_are_bounded_before_allocation() {
    let (source, bytes) = valid_bytes("bounds-source");

    let oversized = path("record-too-large");
    let mut variant = bytes.clone();
    variant[8..16].copy_from_slice(
        &u64::try_from(MAX_RECORD_BYTES + 1)
            .unwrap_or(u64::MAX)
            .to_le_bytes(),
    );
    write_variant(&oversized, &variant);
    assert!(matches!(
        Journal::open(&oversized),
        Err(JournalError::RecordTooLarge(size)) if size == MAX_RECORD_BYTES + 1
    ));

    let too_many = path("too-many-changes");
    let mut variant = bytes.clone();
    variant[60..64].copy_from_slice(&1_000_001_u32.to_le_bytes());
    rewrite_crc(&mut variant);
    write_variant(&too_many, &variant);
    assert!(matches!(
        Journal::open(&too_many),
        Err(JournalError::TooManyChanges(1_000_001))
    ));

    let tuple_too_large = path("tuple-too-large");
    let mut variant = bytes;
    variant[71..75].copy_from_slice(
        &u32::try_from(MAX_RECORD_BYTES + 1)
            .unwrap_or(u32::MAX)
            .to_le_bytes(),
    );
    rewrite_crc(&mut variant);
    write_variant(&tuple_too_large, &variant);
    assert!(matches!(
        Journal::open(&tuple_too_large),
        Err(JournalError::TupleTooLarge(size)) if size == MAX_RECORD_BYTES + 1
    ));

    for item in [source, oversized, too_many, tuple_too_large] {
        let _ = fs::remove_file(item);
    }
}

#[test]
fn malformed_payload_tags_eof_and_trailing_bytes_fail_closed() {
    let (source, bytes) = valid_bytes("codec-source");

    let bad_kind = path("kind");
    let mut variant = bytes.clone();
    variant[68] = 9;
    rewrite_crc(&mut variant);
    write_variant(&bad_kind, &variant);
    assert!(matches!(
        Journal::open(&bad_kind),
        Err(JournalError::InvalidChangeKind(9))
    ));

    let bad_option = path("option");
    let mut variant = bytes.clone();
    variant[69] = 2;
    rewrite_crc(&mut variant);
    write_variant(&bad_option, &variant);
    assert!(matches!(
        Journal::open(&bad_option),
        Err(JournalError::InvalidOptionTag(2))
    ));

    let unexpected_eof = path("eof");
    let mut variant = bytes.clone();
    variant[60..64].copy_from_slice(&2_u32.to_le_bytes());
    rewrite_crc(&mut variant);
    write_variant(&unexpected_eof, &variant);
    assert!(matches!(
        Journal::open(&unexpected_eof),
        Err(JournalError::UnexpectedEof)
    ));

    let trailing = path("trailing");
    let len = payload_len(&bytes);
    let mut variant = Vec::with_capacity(bytes.len() + 1);
    variant.extend_from_slice(&bytes[..HEADER_LEN + len]);
    variant.push(0);
    variant.extend_from_slice(&[0; 4]);
    variant[8..16].copy_from_slice(&u64::try_from(len + 1).unwrap_or(u64::MAX).to_le_bytes());
    rewrite_crc(&mut variant);
    write_variant(&trailing, &variant);
    assert!(matches!(
        Journal::open(&trailing),
        Err(JournalError::TrailingBytes(1))
    ));

    for item in [source, bad_kind, bad_option, unexpected_eof, trailing] {
        let _ = fs::remove_file(item);
    }
}

#[test]
fn transaction_semantics_are_validated_after_decode() {
    let (source, mut bytes) = valid_bytes("transaction-source");
    bytes[52..60].copy_from_slice(&19_u64.to_le_bytes());
    rewrite_crc(&mut bytes);
    let invalid = path("transaction-invalid");
    write_variant(&invalid, &bytes);

    assert!(matches!(
        Journal::open(&invalid),
        Err(JournalError::InvalidTransaction(_))
    ));
    let _ = fs::remove_file(source);
    let _ = fs::remove_file(invalid);
}

#[cfg(target_os = "linux")]
#[test]
fn failed_durable_write_is_retried_instead_of_misclassified_as_duplicate() {
    let mut journal = Journal::open("/dev/full").unwrap_or_else(|_| unreachable!());

    assert!(matches!(journal.append(&batch()), Err(JournalError::Io(_))));
    assert!(matches!(journal.append(&batch()), Err(JournalError::Io(_))));
}
