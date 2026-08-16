use core::fmt;
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use veyra_types::LogSequenceNumber;

use crate::TransactionBatch;

const MAGIC: [u8; 4] = *b"VYAP";
const VERSION: u16 = 1;
const BODY_LEN: usize = 32;
const RECORD_LEN: usize = 36;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct AppliedState {
    commit_lsn: LogSequenceNumber,
    end_lsn: LogSequenceNumber,
    fingerprint: u64,
}

impl AppliedState {
    #[must_use]
    pub const fn commit_lsn(self) -> LogSequenceNumber {
        self.commit_lsn
    }

    #[must_use]
    pub const fn end_lsn(self) -> LogSequenceNumber {
        self.end_lsn
    }

    #[must_use]
    pub const fn fingerprint(self) -> u64 {
        self.fingerprint
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CheckpointAdvance {
    Advanced,
    Duplicate,
}

pub struct AppliedCheckpoint {
    path: PathBuf,
    file: File,
    state: AppliedState,
}

impl fmt::Debug for AppliedCheckpoint {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AppliedCheckpoint")
            .field("path", &self.path)
            .field("state", &self.state)
            .finish_non_exhaustive()
    }
}

impl AppliedCheckpoint {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, CheckpointError> {
        let path = path.as_ref().to_path_buf();
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .append(true)
            .open(&path)?;
        let mut checkpoint = Self {
            path,
            file,
            state: AppliedState::default(),
        };
        checkpoint.recover()?;
        Ok(checkpoint)
    }

    #[must_use]
    pub const fn state(&self) -> AppliedState {
        self.state
    }

    pub fn advance(
        &mut self,
        batch: &TransactionBatch,
    ) -> Result<CheckpointAdvance, CheckpointError> {
        let next = AppliedState {
            commit_lsn: batch.commit_lsn(),
            end_lsn: batch.end_lsn(),
            fingerprint: batch.fingerprint(),
        };
        validate_transition(self.state, next)?;
        if next == self.state {
            return Ok(CheckpointAdvance::Duplicate);
        }
        let record = encode_record(next);
        self.file.write_all(&record)?;
        self.file.sync_data()?;
        self.state = next;
        Ok(CheckpointAdvance::Advanced)
    }

    fn recover(&mut self) -> Result<(), CheckpointError> {
        self.file.flush()?;
        let mut reader = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&self.path)?;
        reader.seek(SeekFrom::Start(0))?;
        let file_len = reader.metadata()?.len();
        let record_len = u64::try_from(RECORD_LEN).map_err(|_| CheckpointError::LengthOverflow)?;
        let complete_len = file_len - (file_len % record_len);
        if complete_len != file_len {
            reader.set_len(complete_len)?;
            reader.sync_data()?;
        }

        let mut state = AppliedState::default();
        let mut offset = 0_u64;
        while offset < complete_len {
            let mut record = [0_u8; RECORD_LEN];
            reader.read_exact(&mut record)?;
            let next = decode_record(&record, offset)?;
            validate_transition(state, next)?;
            state = next;
            offset = offset
                .checked_add(record_len)
                .ok_or(CheckpointError::LengthOverflow)?;
        }
        self.state = state;
        Ok(())
    }
}

fn validate_transition(current: AppliedState, next: AppliedState) -> Result<(), CheckpointError> {
    if next.end_lsn < next.commit_lsn {
        return Err(CheckpointError::EndBeforeCommit);
    }
    if next.commit_lsn < current.commit_lsn || next.end_lsn < current.end_lsn {
        return Err(CheckpointError::Regressed);
    }
    if next.commit_lsn == current.commit_lsn && next != current {
        return Err(CheckpointError::ConflictingCommit(next.commit_lsn));
    }
    Ok(())
}

fn encode_record(state: AppliedState) -> [u8; RECORD_LEN] {
    let mut record = [0_u8; RECORD_LEN];
    record[0..4].copy_from_slice(&MAGIC);
    record[4..6].copy_from_slice(&VERSION.to_le_bytes());
    record[8..16].copy_from_slice(&state.commit_lsn.get().to_le_bytes());
    record[16..24].copy_from_slice(&state.end_lsn.get().to_le_bytes());
    record[24..32].copy_from_slice(&state.fingerprint.to_le_bytes());
    let checksum = crc32c(&record[..BODY_LEN]);
    record[BODY_LEN..RECORD_LEN].copy_from_slice(&checksum.to_le_bytes());
    record
}

fn decode_record(record: &[u8; RECORD_LEN], offset: u64) -> Result<AppliedState, CheckpointError> {
    if record[0..4] != MAGIC {
        return Err(CheckpointError::InvalidMagic(offset));
    }
    let version = u16::from_le_bytes([record[4], record[5]]);
    if version != VERSION {
        return Err(CheckpointError::UnsupportedVersion(version));
    }
    let expected = u32::from_le_bytes(
        record[BODY_LEN..RECORD_LEN]
            .try_into()
            .map_err(|_| CheckpointError::CorruptRecord)?,
    );
    if crc32c(&record[..BODY_LEN]) != expected {
        return Err(CheckpointError::ChecksumMismatch(offset));
    }
    Ok(AppliedState {
        commit_lsn: LogSequenceNumber::new(read_u64(&record[8..16])?),
        end_lsn: LogSequenceNumber::new(read_u64(&record[16..24])?),
        fingerprint: read_u64(&record[24..32])?,
    })
}

fn read_u64(bytes: &[u8]) -> Result<u64, CheckpointError> {
    Ok(u64::from_le_bytes(
        bytes.try_into().map_err(|_| CheckpointError::CorruptRecord)?,
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

#[derive(Debug)]
pub enum CheckpointError {
    Io(io::Error),
    InvalidMagic(u64),
    UnsupportedVersion(u16),
    ChecksumMismatch(u64),
    CorruptRecord,
    EndBeforeCommit,
    Regressed,
    ConflictingCommit(LogSequenceNumber),
    DurableMismatch(LogSequenceNumber),
    MissingFromJournal(LogSequenceNumber),
    LengthOverflow,
}

impl fmt::Display for CheckpointError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "checkpoint I/O: {error}"),
            Self::InvalidMagic(offset) => write!(formatter, "invalid checkpoint magic at {offset}"),
            Self::UnsupportedVersion(version) => {
                write!(formatter, "unsupported checkpoint version {version}")
            }
            Self::ChecksumMismatch(offset) => {
                write!(formatter, "checkpoint checksum mismatch at {offset}")
            }
            Self::CorruptRecord => formatter.write_str("corrupt checkpoint record"),
            Self::EndBeforeCommit => formatter.write_str("checkpoint end LSN is before commit LSN"),
            Self::Regressed => formatter.write_str("checkpoint LSN regressed"),
            Self::ConflictingCommit(lsn) => write!(formatter, "conflicting checkpoint at {lsn:?}"),
            Self::DurableMismatch(lsn) => {
                write!(formatter, "checkpoint does not match durable transaction at {lsn:?}")
            }
            Self::MissingFromJournal(lsn) => {
                write!(formatter, "checkpoint transaction is missing from journal at {lsn:?}")
            }
            Self::LengthOverflow => formatter.write_str("checkpoint length overflow"),
        }
    }
}

impl std::error::Error for CheckpointError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            _ => None,
        }
    }
}

impl From<io::Error> for CheckpointError {
    fn from(value: io::Error) -> Self {
        Self::Io(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::OpenOptions;
    use std::io::{Read, Write};
    use std::time::{SystemTime, UNIX_EPOCH};

    fn batch(commit: u64, end: u64, value: u8) -> TransactionBatch {
        TransactionBatch::try_new(
            1,
            LogSequenceNumber::new(commit),
            LogSequenceNumber::new(commit),
            LogSequenceNumber::new(end),
            vec![crate::RowChange::new(
                7,
                crate::ChangeKind::Insert,
                None,
                Some(vec![value]),
            )],
        )
        .unwrap_or_else(|_| unreachable!())
    }

    fn path(label: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "veyra-applied-{label}-{}-{nanos}.checkpoint",
            std::process::id()
        ))
    }

    #[test]
    fn persists_recovers_and_deduplicates_last_applied() -> Result<(), Box<dyn std::error::Error>> {
        let path = path("roundtrip");
        let first = batch(10, 11, 1);
        let second = batch(20, 21, 2);
        {
            let mut checkpoint = AppliedCheckpoint::open(&path)?;
            assert_eq!(checkpoint.advance(&first)?, CheckpointAdvance::Advanced);
            assert_eq!(checkpoint.advance(&first)?, CheckpointAdvance::Duplicate);
            assert_eq!(checkpoint.advance(&second)?, CheckpointAdvance::Advanced);
        }
        let checkpoint = AppliedCheckpoint::open(&path)?;
        assert_eq!(checkpoint.state().commit_lsn().get(), 20);
        assert_eq!(checkpoint.state().end_lsn().get(), 21);
        assert_eq!(checkpoint.state().fingerprint(), second.fingerprint());
        let _ = std::fs::remove_file(path);
        Ok(())
    }

    #[test]
    fn incomplete_tail_is_truncated_on_recovery() -> Result<(), Box<dyn std::error::Error>> {
        let path = path("tail");
        let record = batch(10, 11, 1);
        {
            let mut checkpoint = AppliedCheckpoint::open(&path)?;
            checkpoint.advance(&record)?;
        }
        OpenOptions::new()
            .append(true)
            .open(&path)?
            .write_all(&[1, 2, 3])?;
        let checkpoint = AppliedCheckpoint::open(&path)?;
        assert_eq!(checkpoint.state().commit_lsn().get(), 10);
        assert_eq!(
            std::fs::metadata(&path)?.len(),
            u64::try_from(RECORD_LEN).unwrap_or_default()
        );
        let _ = std::fs::remove_file(path);
        Ok(())
    }

    #[test]
    fn corrupted_complete_record_fails_closed() -> Result<(), Box<dyn std::error::Error>> {
        let path = path("corrupt");
        let record = batch(10, 11, 1);
        {
            let mut checkpoint = AppliedCheckpoint::open(&path)?;
            checkpoint.advance(&record)?;
        }
        let mut bytes = Vec::new();
        OpenOptions::new().read(true).open(&path)?.read_to_end(&mut bytes)?;
        bytes[24] ^= 0x55;
        let mut file = OpenOptions::new().write(true).truncate(true).open(&path)?;
        file.write_all(&bytes)?;
        file.sync_data()?;
        drop(file);
        assert!(matches!(
            AppliedCheckpoint::open(&path),
            Err(CheckpointError::ChecksumMismatch(0))
        ));
        let _ = std::fs::remove_file(path);
        Ok(())
    }

    #[test]
    fn conflicting_and_regressed_transitions_fail_closed() -> Result<(), Box<dyn std::error::Error>> {
        let path = path("ordering");
        let mut checkpoint = AppliedCheckpoint::open(&path)?;
        checkpoint.advance(&batch(20, 21, 1))?;
        assert!(matches!(
            checkpoint.advance(&batch(20, 22, 2)),
            Err(CheckpointError::ConflictingCommit(_))
        ));
        assert!(matches!(
            checkpoint.advance(&batch(10, 11, 1)),
            Err(CheckpointError::Regressed)
        ));
        let _ = std::fs::remove_file(path);
        Ok(())
    }

    #[test]
    fn display_is_stable() {
        assert_eq!(CheckpointError::Regressed.to_string(), "checkpoint LSN regressed");
    }
}
