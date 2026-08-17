use core::fmt;
use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use veyra_types::LogSequenceNumber;

use crate::{ChangeKind, RowChange, TransactionBatch, TransactionValidationError};

const MAGIC: [u8; 4] = *b"VYCD";
const VERSION: u16 = 1;
const HEADER_LEN: usize = 32;
const CRC_LEN: u64 = 4;
const MAX_RECORD_BYTES: usize = 64 * 1024 * 1024;
const MAX_CHANGES: usize = 1_000_000;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReplayDecision {
    Apply,
    Duplicate,
}

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
        let decision = self.classify(batch)?;
        if decision == ReplayDecision::Apply {
            self.record(batch);
        }
        Ok(decision)
    }

    fn classify(&self, batch: &TransactionBatch) -> Result<ReplayDecision, JournalError> {
        let lsn = batch.commit_lsn();
        let fingerprint = batch.fingerprint();
        match self.fingerprints.get(&lsn) {
            Some(existing) if *existing == fingerprint => Ok(ReplayDecision::Duplicate),
            Some(_) => Err(JournalError::ConflictingReplay(lsn)),
            None => Ok(ReplayDecision::Apply),
        }
    }

    fn record(&mut self, batch: &TransactionBatch) {
        self.fingerprints
            .insert(batch.commit_lsn(), batch.fingerprint());
    }

    #[must_use]
    pub fn highest_commit_lsn(&self) -> LogSequenceNumber {
        self.fingerprints
            .last_key_value()
            .map_or(LogSequenceNumber::ZERO, |(lsn, _)| *lsn)
    }
}

/// Append-only durable journal for complete logical transactions.
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

    /// Appends a complete transaction and calls `sync_data` before reporting success.
    pub fn append(&mut self, batch: &TransactionBatch) -> Result<ReplayDecision, JournalError> {
        let decision = self.guard.classify(batch)?;
        if decision == ReplayDecision::Duplicate {
            return Ok(decision);
        }
        let payload = encode_batch(batch)?;
        if payload.len() > MAX_RECORD_BYTES {
            return Err(JournalError::RecordTooLarge(payload.len()));
        }
        let payload_len = payload.len() as u64;
        let mut header = [0_u8; HEADER_LEN];
        header[0..4].copy_from_slice(&MAGIC);
        header[4..6].copy_from_slice(&VERSION.to_le_bytes());
        header[8..16].copy_from_slice(&payload_len.to_le_bytes());
        header[16..24].copy_from_slice(&batch.commit_lsn().get().to_le_bytes());
        header[24..32].copy_from_slice(&batch.fingerprint().to_le_bytes());
        self.file.write_all(&header)?;
        self.file.write_all(&payload)?;
        self.file.write_all(&crc32c(&payload).to_le_bytes())?;
        self.file.sync_data()?;
        self.guard.record(batch);
        Ok(decision)
    }

    #[must_use]
    pub fn highest_commit_lsn(&self) -> LogSequenceNumber {
        self.guard.highest_commit_lsn()
    }

    pub fn replay(&mut self) -> Result<Vec<TransactionBatch>, JournalError> {
        // `File` is unbuffered and append durability is established with `sync_data`.
        let mut reader = File::open(&self.path)?;
        let (records, _) = scan(&mut reader, false)?;
        Ok(records)
    }

    fn recover(&mut self) -> Result<(), JournalError> {
        let mut reader = OpenOptions::new().read(true).write(true).open(&self.path)?;
        let (records, truncate_to) = scan(&mut reader, true)?;
        if let Some(len) = truncate_to {
            reader.set_len(len)?;
            reader.sync_data()?;
        }
        let mut guard = ReplayGuard::new();
        for record in &records {
            if guard.observe(record)? != ReplayDecision::Apply {
                return Err(JournalError::DuplicateDurableRecord(record.commit_lsn()));
            }
        }
        self.guard = guard;
        Ok(())
    }
}

fn scan(
    file: &mut File,
    allow_incomplete_tail: bool,
) -> Result<(Vec<TransactionBatch>, Option<u64>), JournalError> {
    // Every caller supplies a freshly opened handle whose cursor is at offset zero.
    let file_len = file.metadata()?.len();
    let header_len = HEADER_LEN as u64;
    let mut records = Vec::new();
    let mut offset = 0_u64;
    loop {
        if offset == file_len {
            return Ok((records, None));
        }
        let remaining = file_len - offset;
        if remaining < header_len {
            return tail(records, offset, allow_incomplete_tail);
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
        let payload_len = header_u64(&header[8..16]);
        let commit_lsn = header_u64(&header[16..24]);
        let fingerprint = header_u64(&header[24..32]);
        if payload_len > MAX_RECORD_BYTES as u64 {
            return Err(JournalError::RecordTooLarge(MAX_RECORD_BYTES + 1));
        }
        #[expect(
            clippy::cast_possible_truncation,
            reason = "payload_len is rejected above 64 MiB, which fits every supported usize target"
        )]
        let payload_len_usize = payload_len as usize;
        let total = header_len + payload_len + CRC_LEN;
        if remaining < total {
            return tail(records, offset, allow_incomplete_tail);
        }
        let mut payload = vec![0_u8; payload_len_usize];
        file.read_exact(&mut payload)?;
        let mut crc = [0_u8; 4];
        file.read_exact(&mut crc)?;
        if crc32c(&payload) != u32::from_le_bytes(crc) {
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

fn tail(
    records: Vec<TransactionBatch>,
    offset: u64,
    allow: bool,
) -> Result<(Vec<TransactionBatch>, Option<u64>), JournalError> {
    if allow {
        Ok((records, Some(offset)))
    } else {
        Err(JournalError::IncompleteTail(offset))
    }
}

fn header_u64(bytes: &[u8]) -> u64 {
    u64::from_le_bytes([
        bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
    ])
}

fn encode_batch(batch: &TransactionBatch) -> Result<Vec<u8>, JournalError> {
    let count = batch.changes().len();
    if count > MAX_CHANGES {
        return Err(JournalError::TooManyChanges(count));
    }
    #[expect(
        clippy::cast_possible_truncation,
        reason = "change count is rejected above 1,000,000, which is below u32::MAX"
    )]
    let count = count as u32;
    let mut out = Vec::new();
    out.extend_from_slice(&batch.xid().to_le_bytes());
    out.extend_from_slice(&batch.final_lsn().get().to_le_bytes());
    out.extend_from_slice(&batch.commit_lsn().get().to_le_bytes());
    out.extend_from_slice(&batch.end_lsn().get().to_le_bytes());
    out.extend_from_slice(&count.to_le_bytes());
    for change in batch.changes() {
        out.extend_from_slice(&change.relation_id.to_le_bytes());
        out.push(change.kind as u8);
        encode_optional(&mut out, change.old_tuple.as_deref())?;
        encode_optional(&mut out, change.new_tuple.as_deref())?;
    }
    Ok(out)
}

fn encode_optional(out: &mut Vec<u8>, tuple: Option<&[u8]>) -> Result<(), JournalError> {
    match tuple {
        Some(bytes) => {
            if bytes.len() > MAX_RECORD_BYTES {
                return Err(JournalError::TupleTooLarge(bytes.len()));
            }
            #[expect(
                clippy::cast_possible_truncation,
                reason = "tuple bytes are rejected above 64 MiB, which is below u32::MAX"
            )]
            let len = bytes.len() as u32;
            out.push(1);
            out.extend_from_slice(&len.to_le_bytes());
            out.extend_from_slice(bytes);
        }
        None => out.push(0),
    }
    Ok(())
}

fn decode_batch(bytes: &[u8]) -> Result<TransactionBatch, JournalError> {
    let mut cursor = Cursor::new(bytes);
    let xid = cursor.u32()?;
    let final_lsn = LogSequenceNumber::new(cursor.u64()?);
    let commit_lsn = LogSequenceNumber::new(cursor.u64()?);
    let end_lsn = LogSequenceNumber::new(cursor.u64()?);
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
        changes.push(RowChange::new(
            relation_id,
            kind,
            cursor.optional_bytes()?,
            cursor.optional_bytes()?,
        ));
    }
    cursor.finish()?;
    TransactionBatch::try_new(xid, final_lsn, commit_lsn, end_lsn, changes)
        .map_err(JournalError::InvalidTransaction)
}

struct Cursor<'a> {
    bytes: &'a [u8],
    offset: usize,
}
impl<'a> Cursor<'a> {
    const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }
    fn take(&mut self, len: usize) -> Result<&'a [u8], JournalError> {
        // Journal payloads are capped at 64 MiB, so cursor arithmetic cannot overflow `usize`
        // on any supported 64-bit target.
        let end = self.offset + len;
        let slice = self
            .bytes
            .get(self.offset..end)
            .ok_or(JournalError::UnexpectedEof)?;
        self.offset = end;
        Ok(slice)
    }
    fn u8(&mut self) -> Result<u8, JournalError> {
        Ok(self.take(1)?[0])
    }
    fn u32(&mut self) -> Result<u32, JournalError> {
        let bytes = self.take(4)?;
        Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }
    fn u64(&mut self) -> Result<u64, JournalError> {
        let bytes = self.take(8)?;
        Ok(u64::from_le_bytes([
            bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
        ]))
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
            tag => Err(JournalError::InvalidOptionTag(tag)),
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
    use std::io::{Seek, SeekFrom};
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
            LogSequenceNumber::new(commit),
            vec![RowChange::new(
                5,
                ChangeKind::Insert,
                None,
                Some(vec![value]),
            )],
        )
        .unwrap_or_else(|_| unreachable!())
    }

    #[test]
    fn crc_matches_standard_vector() {
        assert_eq!(crc32c(b"123456789"), 0xe306_9283);
    }

    #[test]
    fn replay_guard_handles_duplicate_and_conflict() {
        let mut guard = ReplayGuard::new();
        let one = batch(10, 1);
        assert_eq!(guard.observe(&one), Ok(ReplayDecision::Apply));
        assert_eq!(guard.observe(&one), Ok(ReplayDecision::Duplicate));
        assert_eq!(guard.highest_commit_lsn().get(), 10);
        assert!(matches!(
            guard.observe(&batch(10, 2)),
            Err(JournalError::ConflictingReplay(_))
        ));
    }

    #[test]
    fn journal_round_trips_and_reopens() -> Result<(), JournalError> {
        let path = temp_path("roundtrip");
        let mut journal = Journal::open(&path)?;
        assert_eq!(journal.append(&batch(10, 1))?, ReplayDecision::Apply);
        assert_eq!(journal.append(&batch(10, 1))?, ReplayDecision::Duplicate);
        assert_eq!(journal.append(&batch(20, 2))?, ReplayDecision::Apply);
        assert_eq!(journal.replay()?.len(), 2);
        drop(journal);
        assert_eq!(Journal::open(&path)?.highest_commit_lsn().get(), 20);
        let _ = std::fs::remove_file(path);
        Ok(())
    }

    #[test]
    fn recovery_truncates_incomplete_tail() -> Result<(), JournalError> {
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
        assert_eq!(Journal::open(&path)?.highest_commit_lsn().get(), 10);
        assert_eq!(std::fs::metadata(&path)?.len(), good_len);
        let _ = std::fs::remove_file(path);
        Ok(())
    }

    #[test]
    fn checksum_corruption_is_rejected() -> Result<(), Box<dyn std::error::Error>> {
        let path = temp_path("crc");
        {
            let mut journal = Journal::open(&path)?;
            let _ = journal.append(&batch(10, 1))?;
        }
        let pos = std::fs::metadata(&path)?.len() - 1;
        {
            let mut file = OpenOptions::new().read(true).write(true).open(&path)?;
            file.seek(SeekFrom::Start(pos))?;
            let mut byte = [0_u8; 1];
            file.read_exact(&mut byte)?;
            file.seek(SeekFrom::Start(pos))?;
            file.write_all(&[byte[0] ^ 0xff])?;
            file.sync_data()?;
        }
        assert!(matches!(
            Journal::open(&path),
            Err(JournalError::ChecksumMismatch(_))
        ));
        let _ = std::fs::remove_file(path);
        Ok(())
    }

    #[test]
    fn codec_rejects_invalid_kind_and_trailing_bytes() {
        let valid = batch(10, 1);
        let mut bytes = encode_batch(&valid).unwrap_or_else(|_| unreachable!());
        let kind_offset = 4 + 8 + 8 + 8 + 4 + 4;
        bytes[kind_offset] = 99;
        assert!(matches!(
            decode_batch(&bytes),
            Err(JournalError::InvalidChangeKind(99))
        ));
        let mut bytes = encode_batch(&valid).unwrap_or_else(|_| unreachable!());
        bytes.push(0);
        assert!(matches!(
            decode_batch(&bytes),
            Err(JournalError::TrailingBytes(1))
        ));
    }
}
