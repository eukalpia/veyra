use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use veyra_cdc::{ChangeKind, Journal, RowChange, TransactionBatch};
use veyra_types::LogSequenceNumber;

const HEADER_LEN: usize = 32;

fn path(label: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    std::env::temp_dir().join(format!(
        "veyra-journal-truncation-{label}-{}-{nanos}.bin",
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

fn batch() -> TransactionBatch {
    TransactionBatch::try_new(
        77,
        LogSequenceNumber::new(100),
        LogSequenceNumber::new(100),
        LogSequenceNumber::new(101),
        vec![
            RowChange::new(1, ChangeKind::Insert, None, Some(vec![1, 2, 3])),
            RowChange::new(
                2,
                ChangeKind::Update,
                Some(vec![4, 5]),
                Some(vec![6, 7, 8]),
            ),
            RowChange::new(3, ChangeKind::Delete, Some(vec![9]), None),
            RowChange::new(4, ChangeKind::Truncate, None, None),
        ],
    )
    .unwrap_or_else(|_| unreachable!())
}

fn valid_record() -> Vec<u8> {
    let source = path("source");
    let mut journal = Journal::open(&source).unwrap_or_else(|_| unreachable!());
    journal
        .append(&batch())
        .unwrap_or_else(|_| unreachable!());
    drop(journal);
    let record = fs::read(&source).unwrap_or_else(|_| unreachable!());
    let _ = fs::remove_file(source);
    record
}

#[test]
fn recovery_truncates_every_incomplete_record_prefix() {
    let record = valid_record();
    let target = path("recover-prefix");

    for end in 0..record.len() {
        fs::write(&target, &record[..end]).unwrap_or_else(|_| unreachable!());
        let journal = Journal::open(&target).unwrap_or_else(|_| unreachable!());
        drop(journal);
        assert_eq!(
            fs::metadata(&target)
                .unwrap_or_else(|_| unreachable!())
                .len(),
            0,
            "prefix {end}/{} was not truncated",
            record.len()
        );
    }

    let _ = fs::remove_file(target);
}

#[test]
fn every_truncated_transaction_payload_fails_closed_after_integrity_validation() {
    let record = valid_record();
    let payload_len = usize::try_from(u64::from_le_bytes(
        record[8..16]
            .try_into()
            .unwrap_or_else(|_| unreachable!()),
    ))
    .unwrap_or_else(|_| unreachable!());
    let payload = &record[HEADER_LEN..HEADER_LEN + payload_len];
    let target = path("payload-prefix");

    for end in 0..payload.len() {
        let prefix = &payload[..end];
        let mut variant = record[..HEADER_LEN].to_vec();
        variant[8..16].copy_from_slice(
            &u64::try_from(prefix.len())
                .unwrap_or(u64::MAX)
                .to_le_bytes(),
        );
        variant.extend_from_slice(prefix);
        variant.extend_from_slice(&crc32c(prefix).to_le_bytes());
        fs::write(&target, variant).unwrap_or_else(|_| unreachable!());
        assert!(
            Journal::open(&target).is_err(),
            "truncated payload {end}/{} unexpectedly opened",
            payload.len()
        );
    }

    let _ = fs::remove_file(target);
}
