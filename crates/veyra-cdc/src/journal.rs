use core::fmt;
use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use veyra_types::LogSequenceNumber;

use crate::{ChangeKind, RowChange, TransactionBatch, TransactionValidationError};

const MAGIC: [u8; 4] = *b"VYCD";
const VERSION: u16 = 1;
const HEADER_LEN: usize = 32;
const MAX_RECORD_BYTES: usize = 64 * 1024 * 1024;
const MAX_CHANGES: usize = 1_000_000;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReplayDecision {
    Apply,
    Duplicate,
}

/// Detects duplicate WAL redelivery while failing closed if the same commit LSN carries different
/// transaction contents.
#[derive(Clone, Debug, Default)]
pub struct ReplayGuard {
    fingerprints: BTreeMap<LogSequenceNumber, u64>,
}

impl ReplayGuard {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn observe(&mut self, batch: &TransactionBatch) -> Result<ReplayDecision, JournalError> {
        let lsn = batch.commit_lsn();
        let fingerprint = batch.fingerprint();
        match self.fingerprints.get(&lsn) {
            Some(existing) if *existing == fingerprint => Ok(ReplayDecision::Duplicate),
            Some(_) => Err(JournalError::ConflictingReplay(lsn)),
            None => {
                self.fingerprints.insert(lsn, fingerprint);
                Ok(ReplayDecision::Apply)
            }
        }
    }

    #[must_use]
    pub fn highest_commit_lsn(&self) -> LogSequenceNumber {
        self.fingerprints
            .last_key_value()
            .map_or(LogSequenceNumber::ZERO, |(lsn, _)| *lsn)
    }
}

/// Append-only durable transaction journal.
pub struct Journal {
    path: PathBuf,
    file: File,
    guard: ReplayGuard,
}

impl fmt::Debug for Journal {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Journal")
            .field("path", &self.path)
            .field("highest_commit_lsn", &self.guard.highest_commit_lsn())
            .finish_non_exhaustive()
    }
}

impl Journal {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, JournalError> {
        let path = path.as_ref().to_path_buf();
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .append(true)
            .open(&path)?;
        let mut journal = Self {
            path,
            file,
            guard: ReplayGuard::new(),
        };
        journal.recover()?;
        Ok(journal)
    }

    pub fn append(&mut self, batch: &TransactionBatch) -> Result<ReplayDecision, JournalError> {
        let decision = self.guard.observe(batch)?;
        if decision == ReplayDecision::Duplicate {
            return Ok(decision);
        }
        let payload = encode_batch(batch)?;
        if payload.len() > MAX_RECORD_BYTES {
            return Err(JournalError::RecordTooLarge(payload.len()));
        }
        let crc = crc32c(&payload);
        let mut header = Vec::with_capacity(HEADER_LEN);
        header.extend_from_slice(&MAGIC);
        header.extend_from_slice(&VERSION.to_le_bytes());
        header.extend_from_slice(&0_u16.to_le_bytes());
        header.extend_from_slice(&(payload.len() as u64).to_le_bytes());
        header.extend_from_slice(&batch.commit_lsn().get().to_le_bytes());
        header.extend_from_slice(&batch.fingerprint().to_le_bytes());
        debug_assert_eq!(header.len(), HEADER_LEN);
        self.file.write_all(&header)?;
        self.file.write_all(&payload)?;
        self.file.write_all(&crc.to_le_bytes())?;
        self.file.sync_data()?;
        Ok(decision)
    }

    #[must_use]
    pub fn highest_commit_lsn(&self) -> LogSequenceNumber {
        self.guard.highest_commit_lsn()
    }

    pub fn replay(&mut self) -> Result<Vec<TransactionBatch>, JournalError> {
        self.file.flush()?;
        let mut reader = File::open(&self.path)?;
        let (records, _) = scan(&mut reader, false)?;
        Ok(records)
    }

    fn recover(&mut self) -> Result<(), JournalError> {
        self.file.flush()?;
        let mut reader = OpenOptions::new().read(true).write(true).open(&self.path)?;
        let (records, truncate_to) = scan(&mut reader, true)?;
        if let Some(len) = truncate_to {
            reader.set_len(len)?;
            reader.sync_data()?;
        }
        let mut guard = ReplayGuard::new();
        for record in &records {
            let decision = guard.observe(record)?;
            if decision != ReplayDecision::Apply {
                return Err(JournalError::DuplicateDurableRecord(record.commit_lsn()));
            }
        }
        self.guard = guard;
        Ok(())
    }
}

fn scan(file: &mut File, allow_incomplete_tail: bool) -> Result<(Vec<TransactionBatch>, Option<u64>), JournalError> {
    file.seek(SeekFrom::Start(0))?;
    let file_len = file.metadata()?.len();
    let mut records = Vec::new();
    let mut offset = 0_u64;
    loop {
        if offset == file_len {
            return Ok((records, None));
        }
        let remaining = file_len - offset;
        if remaining < HEADER_LEN as u64 {
            return if allow_incomplete_tail {
                Ok((records, Some(offset)))
            } else {
                Err(JournalError::IncompleteTail(offset))
            };
        }
        let mut header = [0_u8; HEADER_LEN];
        file.read_exact(&mut header)?;
        if header[0..4] != MAGIC {
            return Err(JournalError::InvalidMagic(offset));
        }
        let version = u16::from_le_bytes([header[4], header[5]]);
        if version != VERSION {
            return Err(JournalError::UnsupportedVersion(version));
        }
        let payload_len = u64::from_le_bytes(header[8..16].try_into().map_err(|_| JournalError::CorruptHeader)?);
        let commit_lsn = u64::from_le_bytes(header[16..24].try_into().map_err(|_| JournalError::CorruptHeader)?);
        let fingerprint = u64::from_le_bytes(header[24..32].try_into().map_err(|_| JournalError::CorruptHeader)?);
        if payload_len > MAX_RECORD_BYTES as u64 {
            return Err(JournalError::RecordTooLarge(payload_len as usize));
        }
        let total = HEADER_LEN as u64 + payload_len + 4;
        if remaining < total {
            return if allow_incomplete_tail {
                Ok((records, Some(offset)))
            } else {
                Err(JournalError::IncompleteTail(offset))
            };
        }
        let mut payload = vec![0_u8; payload_len as usize];
        file.read_exact(&mut payload)?;
        let mut crc_bytes = [0_u8; 4];
        file.read_exact(&mut crc_bytes)?;
        let expected_crc = u32::from_le_bytes(crc_bytes);
        if crc32c(&payload) != expected_crc {
            return Err(JournalError::ChecksumMismatch(offset));
        }
        let batch = decode_batch(&payload)?;
        if batch.commit_lsn().get() != commit_lsn || batch.fingerprint() != fingerprint {
            return Err(JournalError::HeaderPayloadMismatch(offset));
        }
        records.push(batch);
        offset += total;
    }
}

fn encode_batch(batch: &TransactionBatch) -> Result<Vec<u8>, JournalError> {
    if batch.changes().len() > MAX_CHANGES {
        return Err(JournalError::TooManyChanges(batch.changes().len()));
    }
    let mut out = Vec::new();
    out.extend_from_slice(&batch.xid().to_le_bytes());
    out.extend_from_slice(&batch.begin_lsn().get().to_le_bytes());
    out.extend_from_slice(&batch.commit_lsn().get().to_le_bytes());
    out.extend_from_slice(&batch.end_lsn().get().to_le_bytes());
    out.extend_from_slice(&(batch.changes().len() as u32).to_le_bytes());
    for change in batch.changes() {
        out.extend_from_slice(&change.relation_id.to_le_bytes());
        out.push(change.kind as u8);
        encode_optional(&mut out, &change.old_tuple)?;
        encode_optional(&mut out, &change.new_tuple)?;
    }
    Ok(out)
}

fn encode_optional(out: &mut Vec<u8>, tuple: &Option<Vec<u8>>) -> Result<(), JournalError> {
    match tuple {
        Some(bytes) => {
            if bytes.len() > MAX_RECORD_BYTES {
                return Err(JournalError::TupleTooLarge(bytes.len()));
            }
            out.push(1);
            out.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
            out.extend_from_slice(bytes);
        }
        None => out.push(0),
    }
    Ok(())
}

fn decode_batch(bytes: &[u8]) -> Result<TransactionBatch, JournalError> {
    let mut cursor = LittleCursor::new(bytes);
    let xid = cursor.u32()?;
    let begin = LogSequenceNumber::new(cursor.u64()?);
    let commit = LogSequenceNumber::new(cursor.u64()?);
    let end = LogSequenceNumber::new(cursor.u64()?);
    let count = cursor.u32()? as usize;
    if count > MAX_CHANGES {
        return Err(JournalError::TooManyChanges(count));
    }
    let mut changes = Vec::with_capacity(count);
    for _ in 0..count {
        let relation_id = cursor.u32()?;
        let kind = match cursor.u8()? {
            0 => ChangeKind::Insert,
            1 => ChangeKind::Update,
            2 => ChangeKind::Delete,
            3 => ChangeKind::Truncate,
            value => return Err(JournalError::InvalidChangeKind(value)),
        };
        let old_tuple = cursor.optional_bytes()?;
        let new_tuple = cursor.optional_bytes()?;
        changes.push(RowChange::new(relation_id, kind, old_tuple, new_tuple));
    }
    cursor.finish()?;
    TransactionBatch::try_new(xid, begin, commit, end, changes).map_err(JournalError::InvalidTransaction)
}

struct LittleCursor<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> LittleCursor<'a> {
    const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    fn take(&mut self, len: usize) -> Result<&'a [u8], JournalError> {
        let end = self.offset.checked_add(len).ok_or(JournalError::LengthOverflow)?;
        let slice = self.bytes.get(self.offset..end).ok_or(JournalError::UnexpectedEof)?;
        self.offset = end;
        Ok(slice)
    }

    fn u8(&mut self) -> Result<u8, JournalError> {
        Ok(self.take(1)?[0])
    }

    fn u32(&mut self) -> Result<u32, JournalError> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().map_err(|_| JournalError::UnexpectedEof)?))
    }

    fn u64(&mut self) -> Result<u64, JournalError> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().map_err(|_| JournalError::UnexpectedEof)?))
    }

    fn optional_bytes(&mut self) -> Result<Option<Vec<u8>>, JournalError> {
        match self.u8()? {
            0 => Ok(None),
            1 => {
                let len = self.u32()? as usize;
                if len > MAX_RECORD_BYTES {
                    return Err(JournalError::TupleTooLarge(len));
                }
                Ok(Some(self.take(len)?.to_vec()))
            }
            value => Err(JournalError::InvalidOptionTag(value)),
        }
    }

    fn finish(&self) -> Result<(), JournalError> {
        if self.offset == self.bytes.len() {
            Ok(())
        } else {
            Err(JournalError::TrailingBytes(self.bytes.len() - self.offset))
        }
    }
}

/// Portable software CRC32C (Castagnoli). SIMD acceleration can be added later behind runtime
/// dispatch without changing the journal format.
fn crc32c(bytes: &[u8]) -> u32 {
    let mut crc = !0_u32;
    for byte in bytes {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0x82f6_3b78 & mask);
        }
    }
    !crc
}

#[derive(Debug)]
pub enum JournalError {
    Io(io::Error),
    InvalidMagic(u64),
    UnsupportedVersion(u16),
    CorruptHeader,
    RecordTooLarge(usize),
    TupleTooLarge(usize),
    TooManyChanges(usize),
    IncompleteTail(u64),
    ChecksumMismatch(u64),
    HeaderPayloadMismatch(u64),
    UnexpectedEof,
    LengthOverflow,
    InvalidChangeKind(u8),
    InvalidOptionTag(u8),
    TrailingBytes(usize),
    InvalidTransaction(TransactionValidationError),
    ConflictingReplay(LogSequenceNumber),
    DuplicateDurableRecord(LogSequenceNumber),
}

impl fmt::Display for JournalError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "journal I/O error: {error}"),
            other => write!(formatter, "{other:?}"),
        }
    }
}

impl std::error::Error for JournalError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::InvalidTransaction(error) => Some(error),
            _ => None,
        }
    }
}

impl From<io::Error> for JournalError {
    fn from(value: io::Error) -> Self {
        Self::Io(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_path(label: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        std::env::temp_dir().join(format!("veyra-{label}-{}-{nanos}.journal", std::process::id()))
    }

    fn batch(commit: u64, value: u8) -> TransactionBatch {
        TransactionBatch::try_new(
            1,
            LogSequenceNumber::new(commit.saturating_sub(1)),
            LogSequenceNumber::new(commit),
            LogSequenceNumber::new(commit),
            vec![RowChange::new(5, ChangeKind::Insert, None, Some(vec![value]))],
        )
        .unwrap_or_else(|_| unreachable!())
    }

    #[test]
    fn crc32c_matches_standard_vector() {
        assert_eq!(crc32c(b"123456789"), 0xe306_9283);
    }

    #[test]
    fn replay_guard_is_idempotent_and_conflicts_fail_closed() {
        let mut guard = ReplayGuard::new();
        let one = batch(10, 1);
        assert_eq!(guard.observe(&one), Ok(ReplayDecision::Apply));
        assert_eq!(guard.observe(&one), Ok(ReplayDecision::Duplicate));
        assert_eq!(guard.highest_commit_lsn().get(), 10);
        assert!(matches!(guard.observe(&batch(10, 2)), Err(JournalError::ConflictingReplay(_))));
    }

    #[test]
    fn journal_round_trips_and_duplicate_append_is_noop() -> Result<(), JournalError> {
        let path = temp_path("roundtrip");
        let mut journal = Journal::open(&path)?;
        assert_eq!(journal.append(&batch(10, 1))?, ReplayDecision::Apply);
        assert_eq!(journal.append(&batch(10, 1))?, ReplayDecision::Duplicate);
        assert_eq!(journal.append(&batch(20, 2))?, ReplayDecision::Apply);
        assert_eq!(journal.highest_commit_lsn().get(), 20);
        assert_eq!(journal.replay()?.len(), 2);
        drop(journal);
        let reopened = Journal::open(&path)?;
        assert_eq!(reopened.highest_commit_lsn().get(), 20);
        let _ = std::fs::remove_file(path);
        Ok(())
    }

    #[test]
    fn incomplete_final_record_is_truncated_during_recovery() -> Result<(), JournalError> {
        let path = temp_path("tail");
        {
            let mut journal = Journal::open(&path)?;
            let _ = journal.append(&batch(10, 1))?;
        }
        let good_len = std::fs::metadata(&path)?.len();
        {
            let mut file = OpenOptions::new().append(true).open(&path)?;
            file.write_all(b"VY")?;
            file.sync_data()?;
        }
        let journal = Journal::open(&path)?;
        assert_eq!(journal.highest_commit_lsn().get(), 10);
        assert_eq!(std::fs::metadata(&path)?.len(), good_len);
        let _ = std::fs::remove_file(path);
        Ok(())
    }

    #[test]
    fn checksum_corruption_is_never_served() -> Result<(), Box<dyn std::error::Error>> {
        let path = temp_path("crc");
        {
            let mut journal = Journal::open(&path)?;
            let _ = journal.append(&batch(10, 1))?;
        }
        let len = std::fs::metadata(&path)?.len();
        {
            let mut file = OpenOptions::new().read(true).write(true).open(&path)?;
            file.seek(SeekFrom::Start(len - 1))?;
            file.write_all(&[0])?;
            file.sync_data()?;
        }
        let error = Journal::open(&path).err().ok_or("expected corruption error")?;
        assert!(matches!(error, JournalError::ChecksumMismatch(_)));
        let _ = std::fs::remove_file(path);
        Ok(())
    }

    #[test]
    fn codec_rejects_invalid_payload_tags_and_trailing_bytes() {
        let valid = batch(10, 1);
        let mut bytes = encode_batch(&valid).unwrap_or_else(|_| unreachable!());
        let change_kind_offset = 4 + 8 + 8 + 8 + 4 + 4;
        bytes[change_kind_offset] = 99;
        assert!(matches!(decode_batch(&bytes), Err(JournalError::InvalidChangeKind(99))));

        let mut bytes = encode_batch(&valid).unwrap_or_else(|_| unreachable!());
        bytes.push(0);
        assert!(matches!(decode_batch(&bytes), Err(JournalError::TrailingBytes(1))));
    }
}
