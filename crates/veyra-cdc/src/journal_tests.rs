use super::*;
use std::time::{SystemTime, UNIX_EPOCH};

fn temp_path(label: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    std::env::temp_dir().join(format!(
        "veyra-{label}-{}-{nanos}.journal",
        std::process::id()
    ))
}

fn batch(commit: u64, value: u8) -> TransactionBatch {
    TransactionBatch::try_new(
        1,
        LogSequenceNumber::new(commit),
        LogSequenceNumber::new(commit),
        LogSequenceNumber::new(commit + 1),
        vec![RowChange::new(
            7,
            ChangeKind::Update,
            Some(vec![value]),
            Some(vec![value.saturating_add(1)]),
        )],
    )
    .unwrap_or_else(|_| unreachable!())
}

#[test]
fn journal_round_trips_and_reopens() -> Result<(), JournalError> {
    let path = temp_path("roundtrip");
    {
        let mut journal = Journal::open(&path)?;
        assert_eq!(journal.append(&batch(10, 1))?, ReplayDecision::Apply);
        assert_eq!(journal.append(&batch(10, 1))?, ReplayDecision::Duplicate);
        assert_eq!(journal.highest_commit_lsn(), LogSequenceNumber::new(10));
    }
    let mut reopened = Journal::open(&path)?;
    assert_eq!(reopened.replay()?, vec![batch(10, 1)]);
    let _ = std::fs::remove_file(path);
    Ok(())
}

#[test]
fn replay_guard_handles_duplicate_and_conflict() {
    let mut guard = ReplayGuard::new();
    let first = batch(5, 1);
    assert_eq!(guard.observe(&first), Ok(ReplayDecision::Apply));
    assert_eq!(guard.observe(&first), Ok(ReplayDecision::Duplicate));
    assert_eq!(
        guard.observe(&batch(5, 9)),
        Err(JournalError::ConflictingReplay(LogSequenceNumber::new(5)))
    );
    assert_eq!(guard.highest_commit_lsn().get(), 5);
}

#[test]
fn recovery_truncates_incomplete_tail() -> Result<(), JournalError> {
    let path = temp_path("tail");
    {
        let mut journal = Journal::open(&path)?;
        journal.append(&batch(10, 1))?;
    }
    let valid_len = std::fs::metadata(&path)?.len();
    {
        let mut file = OpenOptions::new().append(true).open(&path)?;
        file.write_all(b"partial")?;
        file.sync_data()?;
    }
    let mut recovered = Journal::open(&path)?;
    assert_eq!(recovered.replay()?, vec![batch(10, 1)]);
    assert_eq!(std::fs::metadata(&path)?.len(), valid_len);
    let _ = std::fs::remove_file(path);
    Ok(())
}

#[test]
fn checksum_corruption_is_rejected() -> Result<(), Box<dyn std::error::Error>> {
    let path = temp_path("crc");
    {
        let mut journal = Journal::open(&path)?;
        journal.append(&batch(10, 1))?;
    }
    let mut bytes = std::fs::read(&path)?;
    if let Some(byte) = bytes.get_mut(HEADER_LEN) {
        *byte ^= 0x55;
    }
    std::fs::write(&path, bytes)?;
    assert!(matches!(
        Journal::open(&path),
        Err(JournalError::ChecksumMismatch(0))
    ));
    let _ = std::fs::remove_file(path);
    Ok(())
}

#[test]
fn duplicate_durable_record_is_rejected_on_recovery() -> Result<(), Box<dyn std::error::Error>> {
    let path = temp_path("duplicate-durable");
    {
        let mut journal = Journal::open(&path)?;
        journal.append(&batch(10, 1))?;
    }
    let record = std::fs::read(&path)?;
    let mut file = OpenOptions::new().append(true).open(&path)?;
    file.write_all(&record)?;
    file.sync_data()?;
    drop(file);
    assert!(matches!(
        Journal::open(&path),
        Err(JournalError::DuplicateDurableRecord(lsn))
            if lsn == LogSequenceNumber::new(10)
    ));
    let _ = std::fs::remove_file(path);
    Ok(())
}

#[test]
fn crc_matches_standard_vector() {
    assert_eq!(crc32c(b"123456789"), 0xe306_9283);
}

#[test]
fn codec_rejects_invalid_kind_and_trailing_bytes() {
    let mut encoded = encode_batch(&batch(10, 1)).unwrap_or_else(|_| unreachable!());
    let kind_offset = 4 + 8 + 8 + 8 + 4 + 4;
    if let Some(kind) = encoded.get_mut(kind_offset) {
        *kind = 99;
    }
    assert_eq!(decode_batch(&encoded), Err(JournalError::InvalidChangeKind(99)));

    let mut valid = encode_batch(&batch(10, 1)).unwrap_or_else(|_| unreachable!());
    valid.push(0);
    assert_eq!(decode_batch(&valid), Err(JournalError::TrailingBytes(1)));
}
