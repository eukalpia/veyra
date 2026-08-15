use core::fmt;
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use crc32c::crc32c;
use veyra_types::LogSequenceNumber;

use crate::codec::{BatchCodecError, decode_transaction_batch, encode_transaction_batch};
use crate::model::{BatchLimits, TransactionBatch};

const FILE_MAGIC: &[u8; 8] = b"VYRCDC01";
const FILE_VERSION: u16 = 1;
const FILE_HEADER_LEN: u64 = 16;
const RECORD_MAGIC: &[u8; 4] = b"TXN1";
const RECORD_VERSION: u16 = 1;
const RECORD_HEADER_LEN: u64 = 32;

/// Result of opening and validating the durable log.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OpenOutcome {
    /// Highest durable commit checkpoint.
    pub last_durable_lsn: LogSequenceNumber,
    /// True when an incomplete final write was safely truncated.
    pub recovered_torn_tail: bool,
}

/// Idempotent append outcome.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AppendOutcome {
    /// A new transaction was appended and fsynced.
    Appended,
    /// The exact transaction already exists durably.
    Duplicate,
}

/// Veyra-owned, versioned append-only CDC durability log.
pub struct DurableCdcLog {
    path: PathBuf,
    file: File,
    limits: BatchLimits,
    last_durable_lsn: LogSequenceNumber,
}

impl DurableCdcLog {
    /// Opens, validates and repairs only an incomplete final append.
    pub fn open(path: impl AsRef<Path>, limits: BatchLimits) -> Result<(Self, OpenOutcome), DurableLogError> {
        let path = path.as_ref().to_path_buf();
        let mut file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .open(&path)?;
        let length = file.metadata()?.len();
        if length == 0 {
            write_file_header(&mut file)?;
        } else {
            validate_file_header(&mut file, length)?;
        }
        let scan = scan_records(&mut file, limits, true, None)?;
        file.seek(SeekFrom::End(0))?;
        let outcome = OpenOutcome {
            last_durable_lsn: scan.last_lsn,
            recovered_torn_tail: scan.recovered_torn_tail,
        };
        Ok((
            Self {
                path,
                file,
                limits,
                last_durable_lsn: scan.last_lsn,
            },
            outcome,
        ))
    }

    /// Highest transaction end LSN known durable after local `fsync`.
    #[must_use]
    pub const fn last_durable_lsn(&self) -> LogSequenceNumber {
        self.last_durable_lsn
    }

    /// Appends a transaction exactly once. Durability is acknowledged only after `sync_all`.
    pub fn append(&mut self, batch: &TransactionBatch) -> Result<AppendOutcome, DurableLogError> {
        let payload = encode_transaction_batch(batch, self.limits)?;
        let end_lsn = batch.end_lsn();
        if end_lsn <= self.last_durable_lsn {
            let existing = find_payload_at(&self.path, self.limits, end_lsn)?;
            return match existing {
                Some(bytes) if bytes == payload => Ok(AppendOutcome::Duplicate),
                Some(_) => Err(DurableLogError::ConflictingReplay(end_lsn)),
                None => Err(DurableLogError::MissingHistoricalCheckpoint(end_lsn)),
            };
        }

        let payload_len = u32::try_from(payload.len()).map_err(|_| DurableLogError::RecordLengthOverflow)?;
        let header = record_header(payload_len, crc32c(&payload), end_lsn);
        self.file.seek(SeekFrom::End(0))?;
        self.file.write_all(&header)?;
        self.file.write_all(&payload)?;
        self.file.sync_all()?;
        self.last_durable_lsn = end_lsn;
        Ok(AppendOutcome::Appended)
    }

    /// Replays all durable transactions strictly after `after`.
    pub fn replay_from(
        path: impl AsRef<Path>,
        limits: BatchLimits,
        after: LogSequenceNumber,
    ) -> Result<Vec<TransactionBatch>, DurableLogError> {
        let mut file = File::open(path)?;
        let file_len = file.metadata()?.len();
    validate_file_header(&mut file, file_len)?;
        Ok(scan_records(&mut file, limits, false, Some(after))?.collected)
    }
}

fn write_file_header(file: &mut File) -> Result<(), DurableLogError> {
    let mut header = [0u8; FILE_HEADER_LEN as usize];
    header[..8].copy_from_slice(FILE_MAGIC);
    header[8..10].copy_from_slice(&FILE_VERSION.to_le_bytes());
    header[10..12].copy_from_slice(&0u16.to_le_bytes());
    let crc = crc32c(&header[..12]);
    header[12..16].copy_from_slice(&crc.to_le_bytes());
    file.seek(SeekFrom::Start(0))?;
    file.write_all(&header)?;
    file.sync_all()?;
    Ok(())
}

fn validate_file_header(file: &mut File, file_len: u64) -> Result<(), DurableLogError> {
    if file_len < FILE_HEADER_LEN {
        return Err(DurableLogError::TornFileHeader);
    }
    file.seek(SeekFrom::Start(0))?;
    let mut header = [0u8; FILE_HEADER_LEN as usize];
    file.read_exact(&mut header)?;
    if header[..8] != FILE_MAGIC[..] {
        return Err(DurableLogError::InvalidFileMagic);
    }
    let version = u16::from_le_bytes([header[8], header[9]]);
    if version != FILE_VERSION {
        return Err(DurableLogError::UnsupportedFileVersion(version));
    }
    if u16::from_le_bytes([header[10], header[11]]) != 0 {
        return Err(DurableLogError::NonZeroFileReserved);
    }
    let expected_crc = u32::from_le_bytes([header[12], header[13], header[14], header[15]]);
    if crc32c(&header[..12]) != expected_crc {
        return Err(DurableLogError::FileHeaderChecksumMismatch);
    }
    Ok(())
}

fn record_header(payload_len: u32, payload_crc: u32, end_lsn: LogSequenceNumber) -> [u8; 32] {
    let mut header = [0u8; 32];
    header[..4].copy_from_slice(RECORD_MAGIC);
    header[4..6].copy_from_slice(&RECORD_VERSION.to_le_bytes());
    header[6..8].copy_from_slice(&0u16.to_le_bytes());
    header[8..12].copy_from_slice(&payload_len.to_le_bytes());
    header[12..16].copy_from_slice(&payload_crc.to_le_bytes());
    header[16..24].copy_from_slice(&end_lsn.get().to_le_bytes());
    header[24..28].copy_from_slice(&0u32.to_le_bytes());
    let crc = crc32c(&header[..28]);
    header[28..32].copy_from_slice(&crc.to_le_bytes());
    header
}

struct ParsedRecordHeader {
    payload_len: usize,
    payload_crc: u32,
    end_lsn: LogSequenceNumber,
}

fn parse_record_header(
    header: &[u8; 32],
    limits: BatchLimits,
) -> Result<ParsedRecordHeader, DurableLogError> {
    if header[..4] != RECORD_MAGIC[..] {
        return Err(DurableLogError::InvalidRecordMagic);
    }
    let version = u16::from_le_bytes([header[4], header[5]]);
    if version != RECORD_VERSION {
        return Err(DurableLogError::UnsupportedRecordVersion(version));
    }
    if u16::from_le_bytes([header[6], header[7]]) != 0
        || u32::from_le_bytes([header[24], header[25], header[26], header[27]]) != 0
    {
        return Err(DurableLogError::NonZeroRecordReserved);
    }
    let expected_header_crc =
        u32::from_le_bytes([header[28], header[29], header[30], header[31]]);
    if crc32c(&header[..28]) != expected_header_crc {
        return Err(DurableLogError::RecordHeaderChecksumMismatch);
    }
    let payload_len_u32 =
        u32::from_le_bytes([header[8], header[9], header[10], header[11]]);
    let payload_len =
        usize::try_from(payload_len_u32).map_err(|_| DurableLogError::RecordLengthOverflow)?;
    if payload_len > limits.max_encoded_bytes {
        return Err(DurableLogError::RecordTooLarge {
            actual: payload_len,
            maximum: limits.max_encoded_bytes,
        });
    }
    let payload_crc =
        u32::from_le_bytes([header[12], header[13], header[14], header[15]]);
    let end_lsn = LogSequenceNumber::new(u64::from_le_bytes([
        header[16], header[17], header[18], header[19], header[20], header[21], header[22],
        header[23],
    ]));
    Ok(ParsedRecordHeader {
        payload_len,
        payload_crc,
        end_lsn,
    })
}

fn scan_records(
    file: &mut File,
    limits: BatchLimits,
    repair_torn_tail: bool,
    collect_after: Option<LogSequenceNumber>,
) -> Result<ScanResult, DurableLogError> {
    let mut file_len = file.metadata()?.len();
    let mut offset = FILE_HEADER_LEN;
    let mut previous = LogSequenceNumber::ZERO;
    let mut recovered_torn_tail = false;
    let mut collected = Vec::new();

    while offset < file_len {
        let remaining = file_len - offset;
        if remaining < RECORD_HEADER_LEN {
            if repair_torn_tail {
                truncate_tail(file, offset)?;
                file_len = offset;
                recovered_torn_tail = true;
                continue;
            }
            return Err(DurableLogError::TornTail);
        }

        file.seek(SeekFrom::Start(offset))?;
        let mut header = [0u8; RECORD_HEADER_LEN as usize];
        file.read_exact(&mut header)?;
        let parsed = parse_record_header(&header, limits)?;
        let payload_len_u64 =
            u64::try_from(parsed.payload_len).map_err(|_| DurableLogError::RecordLengthOverflow)?;
        let record_len = RECORD_HEADER_LEN
            .checked_add(payload_len_u64)
            .ok_or(DurableLogError::RecordLengthOverflow)?;

        if remaining < record_len {
            if repair_torn_tail {
                truncate_tail(file, offset)?;
                file_len = offset;
                recovered_torn_tail = true;
                continue;
            }
            return Err(DurableLogError::TornTail);
        }

        let mut payload = vec![0u8; parsed.payload_len];
        file.read_exact(&mut payload)?;
        if crc32c(&payload) != parsed.payload_crc {
            return Err(DurableLogError::PayloadChecksumMismatch {
                end_lsn: parsed.end_lsn,
            });
        }
        let batch = decode_transaction_batch(&payload, limits)?;
        if batch.end_lsn() != parsed.end_lsn {
            return Err(DurableLogError::RecordLsnMismatch {
                header: parsed.end_lsn,
                payload: batch.end_lsn(),
            });
        }
        if parsed.end_lsn <= previous {
            return Err(DurableLogError::NonMonotonicRecord {
                previous,
                current: parsed.end_lsn,
            });
        }

        previous = parsed.end_lsn;
        if collect_after.is_some_and(|after| parsed.end_lsn > after) {
            collected.push(batch);
        }
        offset = offset
            .checked_add(record_len)
            .ok_or(DurableLogError::RecordLengthOverflow)?;
    }

    Ok(ScanResult {
        last_lsn: previous,
        recovered_torn_tail,
        collected,
    })
}

fn truncate_tail(file: &mut File, offset: u64) -> Result<(), DurableLogError> {
    file.set_len(offset)?;
    file.sync_all()?;
    Ok(())
}

fn find_payload_at(
    path: &Path,
    limits: BatchLimits,
    target: LogSequenceNumber,
) -> Result<Option<Vec<u8>>, DurableLogError> {
    let mut file = File::open(path)?;
    let file_len = file.metadata()?.len();
    validate_file_header(&mut file, file_len)?;
    let file_len = file.metadata()?.len();
    let mut offset = FILE_HEADER_LEN;

    while offset < file_len {
        if file_len - offset < RECORD_HEADER_LEN {
            return Err(DurableLogError::TornTail);
        }
        file.seek(SeekFrom::Start(offset))?;
        let mut header = [0u8; RECORD_HEADER_LEN as usize];
        file.read_exact(&mut header)?;
        let parsed = parse_record_header(&header, limits)?;
        let payload_len_u64 =
            u64::try_from(parsed.payload_len).map_err(|_| DurableLogError::RecordLengthOverflow)?;
        let record_len = RECORD_HEADER_LEN
            .checked_add(payload_len_u64)
            .ok_or(DurableLogError::RecordLengthOverflow)?;
        if file_len - offset < record_len {
            return Err(DurableLogError::TornTail);
        }
        let mut payload = vec![0u8; parsed.payload_len];
        file.read_exact(&mut payload)?;
        if parsed.end_lsn == target {
            if crc32c(&payload) != parsed.payload_crc {
                return Err(DurableLogError::PayloadChecksumMismatch { end_lsn: target });
            }
            return Ok(Some(payload));
        }
        if parsed.end_lsn > target {
            return Ok(None);
        }
        offset = offset
            .checked_add(record_len)
            .ok_or(DurableLogError::RecordLengthOverflow)?;
    }
    Ok(None)
}

struct ScanResult {
    last_lsn: LogSequenceNumber,
    recovered_torn_tail: bool,
    collected: Vec<TransactionBatch>,
}

/// Durable CDC file validation or I/O failure.
#[derive(Debug)]
pub enum DurableLogError {
    Io(io::Error),
    Codec(BatchCodecError),
    TornFileHeader,
    InvalidFileMagic,
    UnsupportedFileVersion(u16),
    NonZeroFileReserved,
    FileHeaderChecksumMismatch,
    TornTail,
    InvalidRecordMagic,
    UnsupportedRecordVersion(u16),
    NonZeroRecordReserved,
    RecordHeaderChecksumMismatch,
    RecordTooLarge { actual: usize, maximum: usize },
    RecordLengthOverflow,
    PayloadChecksumMismatch { end_lsn: LogSequenceNumber },
    RecordLsnMismatch {
        header: LogSequenceNumber,
        payload: LogSequenceNumber,
    },
    NonMonotonicRecord {
        previous: LogSequenceNumber,
        current: LogSequenceNumber,
    },
    ConflictingReplay(LogSequenceNumber),
    MissingHistoricalCheckpoint(LogSequenceNumber),
}

impl From<io::Error> for DurableLogError {
    fn from(value: io::Error) -> Self {
        Self::Io(value)
    }
}

impl From<BatchCodecError> for DurableLogError {
    fn from(value: BatchCodecError) -> Self {
        Self::Codec(value)
    }
}

impl fmt::Display for DurableLogError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "CDC log I/O error: {error}"),
            Self::Codec(error) => write!(formatter, "CDC log transaction decode error: {error}"),
            Self::TornFileHeader => formatter.write_str("CDC log has a torn file header"),
            Self::InvalidFileMagic => formatter.write_str("CDC log file magic mismatch"),
            Self::UnsupportedFileVersion(version) => write!(formatter, "unsupported CDC log file version {version}"),
            Self::NonZeroFileReserved => formatter.write_str("CDC log file header reserved field is non-zero"),
            Self::FileHeaderChecksumMismatch => formatter.write_str("CDC log file header checksum mismatch"),
            Self::TornTail => formatter.write_str("CDC log has an incomplete final record"),
            Self::InvalidRecordMagic => formatter.write_str("CDC record magic mismatch"),
            Self::UnsupportedRecordVersion(version) => write!(formatter, "unsupported CDC record version {version}"),
            Self::NonZeroRecordReserved => formatter.write_str("CDC record reserved field is non-zero"),
            Self::RecordHeaderChecksumMismatch => formatter.write_str("CDC record header checksum mismatch"),
            Self::RecordTooLarge { actual, maximum } => write!(formatter, "CDC record is {actual} bytes; maximum is {maximum}"),
            Self::RecordLengthOverflow => formatter.write_str("CDC record length overflow"),
            Self::PayloadChecksumMismatch { end_lsn } => write!(formatter, "CDC record payload checksum mismatch at LSN {}", end_lsn.get()),
            Self::RecordLsnMismatch { header, payload } => write!(formatter, "CDC record LSN {} differs from payload LSN {}", header.get(), payload.get()),
            Self::NonMonotonicRecord { previous, current } => write!(formatter, "CDC record LSN {} is not greater than previous {}", current.get(), previous.get()),
            Self::ConflictingReplay(lsn) => write!(formatter, "different transaction replayed for durable LSN {}", lsn.get()),
            Self::MissingHistoricalCheckpoint(lsn) => write!(formatter, "replayed LSN {} is older than durable tail but absent from log", lsn.get()),
        }
    }
}

impl std::error::Error for DurableLogError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Codec(error) => Some(error),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::sample_batch;
    use std::io::{Read as _, Seek as _, Write as _};
    use tempfile::tempdir;

    fn batch_with_lsn(lsn: u64) -> TransactionBatch {
        TransactionBatch::try_new(
            u32::try_from(lsn).unwrap_or(u32::MAX),
            LogSequenceNumber::new(lsn),
            LogSequenceNumber::new(lsn),
            LogSequenceNumber::new(lsn),
            i64::try_from(lsn).unwrap_or(i64::MAX),
            Vec::new(),
            BatchLimits::default(),
        )
        .unwrap_or_else(|error| unreachable!("static batch must be valid: {error}"))
    }

    fn open_temp() -> (tempfile::TempDir, PathBuf, DurableCdcLog) {
        let directory = tempdir().unwrap_or_else(|error| unreachable!("tempdir must work: {error}"));
        let path = directory.path().join("cdc.log");
        let (log, _) = DurableCdcLog::open(&path, BatchLimits::default())
            .unwrap_or_else(|error| unreachable!("new log must open: {error}"));
        (directory, path, log)
    }

    #[test]
    fn new_log_is_synced_and_reopenable() {
        let (_directory, path, log) = open_temp();
        assert_eq!(log.last_durable_lsn(), LogSequenceNumber::ZERO);
        drop(log);
        let (_, outcome) = DurableCdcLog::open(path, BatchLimits::default())
            .unwrap_or_else(|error| unreachable!("valid log must reopen: {error}"));
        assert_eq!(outcome.last_durable_lsn, LogSequenceNumber::ZERO);
        assert!(!outcome.recovered_torn_tail);
    }

    #[test]
    fn append_reopen_and_replay_preserve_transactions() {
        let (_directory, path, mut log) = open_temp();
        let first = sample_batch();
        let second = batch_with_lsn(200);
        assert_eq!(
            log.append(&first).unwrap_or_else(|error| unreachable!("append: {error}")),
            AppendOutcome::Appended
        );
        assert_eq!(
            log.append(&second).unwrap_or_else(|error| unreachable!("append: {error}")),
            AppendOutcome::Appended
        );
        drop(log);
        let (log, outcome) = DurableCdcLog::open(&path, BatchLimits::default())
            .unwrap_or_else(|error| unreachable!("reopen: {error}"));
        assert_eq!(outcome.last_durable_lsn, LogSequenceNumber::new(200));
        assert_eq!(log.last_durable_lsn(), LogSequenceNumber::new(200));
        drop(log);
        assert_eq!(
            DurableCdcLog::replay_from(&path, BatchLimits::default(), LogSequenceNumber::new(101))
                .unwrap_or_else(|error| unreachable!("replay: {error}")),
            vec![second]
        );
    }

    #[test]
    fn exact_duplicate_is_idempotent_even_when_historical() {
        let (_directory, _path, mut log) = open_temp();
        let first = batch_with_lsn(10);
        let second = batch_with_lsn(20);
        assert_eq!(log.append(&first).unwrap_or_else(|e| unreachable!("{e}")), AppendOutcome::Appended);
        assert_eq!(log.append(&second).unwrap_or_else(|e| unreachable!("{e}")), AppendOutcome::Appended);
        assert_eq!(log.append(&first).unwrap_or_else(|e| unreachable!("{e}")), AppendOutcome::Duplicate);
        assert_eq!(log.last_durable_lsn(), LogSequenceNumber::new(20));
    }

    #[test]
    fn conflicting_or_missing_old_replay_fails_closed() {
        let (_directory, _path, mut log) = open_temp();
        let first = batch_with_lsn(10);
        let second = batch_with_lsn(20);
        log.append(&first).unwrap_or_else(|e| unreachable!("{e}"));
        log.append(&second).unwrap_or_else(|e| unreachable!("{e}"));
        let conflicting = TransactionBatch::try_new(
            999,
            LogSequenceNumber::new(10),
            LogSequenceNumber::new(10),
            LogSequenceNumber::new(10),
            0,
            Vec::new(),
            BatchLimits::default(),
        )
        .unwrap_or_else(|e| unreachable!("{e}"));
        assert!(matches!(log.append(&conflicting), Err(DurableLogError::ConflictingReplay(_))));
        assert!(matches!(log.append(&batch_with_lsn(15)), Err(DurableLogError::MissingHistoricalCheckpoint(_))));
    }

    #[test]
    fn torn_record_header_is_truncated_on_open() {
        let (_directory, path, log) = open_temp();
        drop(log);
        let mut file = OpenOptions::new().append(true).open(&path)
            .unwrap_or_else(|e| unreachable!("open: {e}"));
        file.write_all(b"TXN1").unwrap_or_else(|e| unreachable!("write: {e}"));
        file.sync_all().unwrap_or_else(|e| unreachable!("sync: {e}"));
        drop(file);
        let (_, outcome) = DurableCdcLog::open(&path, BatchLimits::default())
            .unwrap_or_else(|e| unreachable!("repair: {e}"));
        assert!(outcome.recovered_torn_tail);
        assert_eq!(std::fs::metadata(path).unwrap_or_else(|e| unreachable!("metadata: {e}")).len(), FILE_HEADER_LEN);
    }

    #[test]
    fn torn_payload_is_truncated_on_open() {
        let (_directory, path, mut log) = open_temp();
        log.append(&batch_with_lsn(10)).unwrap_or_else(|e| unreachable!("{e}"));
        drop(log);
        let valid_len = std::fs::metadata(&path).unwrap_or_else(|e| unreachable!("{e}")).len();
        let payload = encode_transaction_batch(&batch_with_lsn(20), BatchLimits::default())
            .unwrap_or_else(|e| unreachable!("{e}"));
        let header = record_header(
            u32::try_from(payload.len()).unwrap_or(u32::MAX),
            crc32c(&payload),
            LogSequenceNumber::new(20),
        );
        let mut file = OpenOptions::new().append(true).open(&path)
            .unwrap_or_else(|e| unreachable!("{e}"));
        file.write_all(&header).unwrap_or_else(|e| unreachable!("{e}"));
        file.write_all(&payload[..1]).unwrap_or_else(|e| unreachable!("{e}"));
        file.sync_all().unwrap_or_else(|e| unreachable!("{e}"));
        drop(file);
        let (_, outcome) = DurableCdcLog::open(&path, BatchLimits::default())
            .unwrap_or_else(|e| unreachable!("{e}"));
        assert!(outcome.recovered_torn_tail);
        assert_eq!(std::fs::metadata(path).unwrap_or_else(|e| unreachable!("{e}")).len(), valid_len);
    }

    #[test]
    fn complete_payload_corruption_is_never_repaired_silently() {
        let (_directory, path, mut log) = open_temp();
        log.append(&batch_with_lsn(10)).unwrap_or_else(|e| unreachable!("{e}"));
        drop(log);
        let mut file = OpenOptions::new().read(true).write(true).open(&path)
            .unwrap_or_else(|e| unreachable!("{e}"));
        file.seek(SeekFrom::Start(FILE_HEADER_LEN + RECORD_HEADER_LEN))
            .unwrap_or_else(|e| unreachable!("{e}"));
        let mut byte = [0u8; 1];
        file.read_exact(&mut byte).unwrap_or_else(|e| unreachable!("{e}"));
        byte[0] ^= 1;
        file.seek(SeekFrom::Start(FILE_HEADER_LEN + RECORD_HEADER_LEN))
            .unwrap_or_else(|e| unreachable!("{e}"));
        file.write_all(&byte).unwrap_or_else(|e| unreachable!("{e}"));
        file.sync_all().unwrap_or_else(|e| unreachable!("{e}"));
        assert!(matches!(DurableCdcLog::open(path, BatchLimits::default()), Err(DurableLogError::PayloadChecksumMismatch { .. })));
    }

    #[test]
    fn corrupt_file_and_record_headers_fail_closed() {
        let (_directory, path, log) = open_temp();
        drop(log);
        let mut file = OpenOptions::new().read(true).write(true).open(&path)
            .unwrap_or_else(|e| unreachable!("{e}"));
        file.seek(SeekFrom::Start(0)).unwrap_or_else(|e| unreachable!("{e}"));
        file.write_all(b"X").unwrap_or_else(|e| unreachable!("{e}"));
        file.sync_all().unwrap_or_else(|e| unreachable!("{e}"));
        drop(file);
        assert!(matches!(DurableCdcLog::open(&path, BatchLimits::default()), Err(DurableLogError::InvalidFileMagic)));

        let (_directory2, path2, mut log2) = open_temp();
        log2.append(&batch_with_lsn(10)).unwrap_or_else(|e| unreachable!("{e}"));
        drop(log2);
        let mut file = OpenOptions::new().read(true).write(true).open(&path2)
            .unwrap_or_else(|e| unreachable!("{e}"));
        file.seek(SeekFrom::Start(FILE_HEADER_LEN + 28)).unwrap_or_else(|e| unreachable!("{e}"));
        let mut crc = [0u8; 1];
        file.read_exact(&mut crc).unwrap_or_else(|e| unreachable!("{e}"));
        crc[0] ^= 1;
        file.seek(SeekFrom::Start(FILE_HEADER_LEN + 28)).unwrap_or_else(|e| unreachable!("{e}"));
        file.write_all(&crc).unwrap_or_else(|e| unreachable!("{e}"));
        file.sync_all().unwrap_or_else(|e| unreachable!("{e}"));
        drop(file);
        assert!(matches!(DurableCdcLog::open(path2, BatchLimits::default()), Err(DurableLogError::RecordHeaderChecksumMismatch)));
    }

    #[test]
    fn diagnostics_cover_all_non_io_variants() {
        let variants = [
            DurableLogError::TornFileHeader,
            DurableLogError::InvalidFileMagic,
            DurableLogError::UnsupportedFileVersion(2),
            DurableLogError::NonZeroFileReserved,
            DurableLogError::FileHeaderChecksumMismatch,
            DurableLogError::TornTail,
            DurableLogError::InvalidRecordMagic,
            DurableLogError::UnsupportedRecordVersion(2),
            DurableLogError::NonZeroRecordReserved,
            DurableLogError::RecordHeaderChecksumMismatch,
            DurableLogError::RecordTooLarge { actual: 2, maximum: 1 },
            DurableLogError::RecordLengthOverflow,
            DurableLogError::PayloadChecksumMismatch { end_lsn: LogSequenceNumber::new(1) },
            DurableLogError::RecordLsnMismatch { header: LogSequenceNumber::new(1), payload: LogSequenceNumber::new(2) },
            DurableLogError::NonMonotonicRecord { previous: LogSequenceNumber::new(2), current: LogSequenceNumber::new(1) },
            DurableLogError::ConflictingReplay(LogSequenceNumber::new(1)),
            DurableLogError::MissingHistoricalCheckpoint(LogSequenceNumber::new(1)),
        ];
        for error in variants {
            assert!(!error.to_string().is_empty());
            assert!(std::error::Error::source(&error).is_none());
        }
        let codec = DurableLogError::Codec(BatchCodecError::InvalidMagic);
        assert!(std::error::Error::source(&codec).is_some());
    }
}
