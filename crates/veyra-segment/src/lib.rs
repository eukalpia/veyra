#![forbid(unsafe_code)]

//! Veyra-owned immutable segment format.

use core::fmt;
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use veyra_types::{GenerationId, LogSequenceNumber};

const MAGIC: [u8; 4] = *b"VYSG";
const FORMAT_VERSION: u16 = 1;
const HEADER_LEN: usize = 80;
const MAX_PAYLOAD_BYTES: usize = 512 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
#[repr(u16)]
pub enum SegmentType {
    Availability = 1,
    Property = 2,
    RoomType = 3,
    Pricing = 4,
    Rules = 5,
}

impl TryFrom<u16> for SegmentType {
    type Error = SegmentError;

    fn try_from(value: u16) -> Result<Self, Self::Error> {
        match value {
            1 => Ok(Self::Availability),
            2 => Ok(Self::Property),
            3 => Ok(Self::RoomType),
            4 => Ok(Self::Pricing),
            5 => Ok(Self::Rules),
            other => Err(SegmentError::UnknownSegmentType(other)),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SegmentHeader {
    pub segment_type: SegmentType,
    pub flags: u32,
    pub generation: GenerationId,
    pub start_lsn: LogSequenceNumber,
    pub end_lsn: LogSequenceNumber,
    pub record_count: u64,
    pub payload_length: u64,
    pub checksum: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Segment {
    header: SegmentHeader,
    payload: Vec<u8>,
}

impl Segment {
    pub fn build(
        segment_type: SegmentType,
        flags: u32,
        generation: GenerationId,
        start_lsn: LogSequenceNumber,
        end_lsn: LogSequenceNumber,
        record_count: u64,
        payload: Vec<u8>,
    ) -> Result<Self, SegmentError> {
        if end_lsn < start_lsn {
            return Err(SegmentError::LsnRangeReversed);
        }
        if payload.len() > MAX_PAYLOAD_BYTES {
            return Err(SegmentError::PayloadTooLarge(payload.len()));
        }
        let payload_length = payload.len() as u64;
        let checksum = crc32c(&payload);
        Ok(Self {
            header: SegmentHeader {
                segment_type,
                flags,
                generation,
                start_lsn,
                end_lsn,
                record_count,
                payload_length,
                checksum,
            },
            payload,
        })
    }

    #[must_use]
    pub const fn header(&self) -> SegmentHeader {
        self.header
    }

    #[must_use]
    pub fn payload(&self) -> &[u8] {
        &self.payload
    }

    pub fn encode(&self) -> Result<Vec<u8>, SegmentError> {
        let total = HEADER_LEN + self.payload.len();
        let mut out = Vec::with_capacity(total);
        out.extend_from_slice(&MAGIC);
        out.extend_from_slice(&FORMAT_VERSION.to_le_bytes());
        out.extend_from_slice(&(self.header.segment_type as u16).to_le_bytes());
        out.extend_from_slice(&self.header.flags.to_le_bytes());
        out.extend_from_slice(&0_u32.to_le_bytes());
        out.extend_from_slice(&self.header.generation.get().to_le_bytes());
        out.extend_from_slice(&self.header.start_lsn.get().to_le_bytes());
        out.extend_from_slice(&self.header.end_lsn.get().to_le_bytes());
        out.extend_from_slice(&self.header.record_count.to_le_bytes());
        let payload_offset = HEADER_LEN as u64;
        out.extend_from_slice(&payload_offset.to_le_bytes());
        out.extend_from_slice(&self.header.payload_length.to_le_bytes());
        out.extend_from_slice(&self.header.checksum.to_le_bytes());
        out.extend_from_slice(&0_u32.to_le_bytes());
        out.extend_from_slice(&0_u64.to_le_bytes());
        out.extend_from_slice(&self.payload);
        Ok(out)
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, SegmentError> {
        if bytes.len() < HEADER_LEN {
            return Err(SegmentError::TruncatedHeader(bytes.len()));
        }
        if bytes[0..4] != MAGIC {
            return Err(SegmentError::InvalidMagic);
        }
        let version = read_u16(&bytes[4..6]);
        if version != FORMAT_VERSION {
            return Err(SegmentError::UnsupportedVersion(version));
        }
        let segment_type = SegmentType::try_from(read_u16(&bytes[6..8]))?;
        let flags = read_u32(&bytes[8..12]);
        let generation = GenerationId::new(read_u64(&bytes[16..24]));
        let start_lsn = LogSequenceNumber::new(read_u64(&bytes[24..32]));
        let end_lsn = LogSequenceNumber::new(read_u64(&bytes[32..40]));
        if end_lsn < start_lsn {
            return Err(SegmentError::LsnRangeReversed);
        }
        let record_count = read_u64(&bytes[40..48]);
        let payload_offset = read_u64(&bytes[48..56]);
        let payload_length = read_u64(&bytes[56..64]);
        let checksum = read_u32(&bytes[64..68]);
        let expected_offset = HEADER_LEN as u64;
        if payload_offset != expected_offset {
            return Err(SegmentError::InvalidPayloadOffset(payload_offset));
        }
        if payload_length > MAX_PAYLOAD_BYTES as u64 {
            return Err(SegmentError::PayloadTooLarge(MAX_PAYLOAD_BYTES + 1));
        }
        let payload_len = payload_length as usize;
        let end = HEADER_LEN + payload_len;
        if bytes.len() != end {
            return Err(SegmentError::LengthMismatch {
                expected: end,
                actual: bytes.len(),
            });
        }
        let payload = bytes[HEADER_LEN..end].to_vec();
        if crc32c(&payload) != checksum {
            return Err(SegmentError::ChecksumMismatch);
        }
        Ok(Self {
            header: SegmentHeader {
                segment_type,
                flags,
                generation,
                start_lsn,
                end_lsn,
                record_count,
                payload_length,
                checksum,
            },
            payload,
        })
    }

    pub fn write_atomic(&self, path: impl AsRef<Path>) -> Result<(), SegmentError> {
        let path = path.as_ref();
        let temp = temp_path(path)?;
        let encoded = self.encode()?;
        let write_result = (|| -> Result<(), SegmentError> {
            let mut file = OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&temp)?;
            file.write_all(&encoded)?;
            file.sync_all()?;
            drop(file);
            std::fs::rename(&temp, path)?;
            sync_parent(path)?;
            Ok(())
        })();
        if write_result.is_err() {
            let _ = std::fs::remove_file(&temp);
        }
        write_result
    }

    pub fn read(path: impl AsRef<Path>) -> Result<Self, SegmentError> {
        let mut file = File::open(path)?;
        let metadata = file.metadata()?;
        let file_len = metadata.len();
        let max_file = HEADER_LEN + MAX_PAYLOAD_BYTES;
        if file_len > max_file as u64 {
            return Err(SegmentError::PayloadTooLarge(MAX_PAYLOAD_BYTES + 1));
        }
        let len = file_len as usize;
        let mut bytes = Vec::with_capacity(len);
        file.read_to_end(&mut bytes)?;
        Self::decode(&bytes)
    }
}

fn temp_path(path: &Path) -> Result<PathBuf, SegmentError> {
    let name = path
        .file_name()
        .ok_or(SegmentError::MissingFileName)?
        .to_string_lossy();
    Ok(path.with_file_name(format!(".{name}.{}.tmp", std::process::id())))
}

fn sync_parent(path: &Path) -> Result<(), SegmentError> {
    let Some(parent) = path.parent() else {
        return Ok(());
    };
    #[cfg(unix)]
    {
        File::open(parent)?.sync_all()?;
    }
    #[cfg(not(unix))]
    {
        let _ = std::fs::metadata(parent)?;
    }
    Ok(())
}

fn read_u16(bytes: &[u8]) -> u16 {
    u16::from_le_bytes([bytes[0], bytes[1]])
}
fn read_u32(bytes: &[u8]) -> u32 {
    u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
}
fn read_u64(bytes: &[u8]) -> u64 {
    u64::from_le_bytes([
        bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
    ])
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
pub enum SegmentError {
    Io(io::Error),
    InvalidMagic,
    UnsupportedVersion(u16),
    UnknownSegmentType(u16),
    TruncatedHeader(usize),
    MalformedHeader,
    LsnRangeReversed,
    PayloadTooLarge(usize),
    LengthOverflow,
    InvalidPayloadOffset(u64),
    LengthMismatch { expected: usize, actual: usize },
    ChecksumMismatch,
    InternalHeaderSize(usize),
    MissingFileName,
}

impl fmt::Display for SegmentError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "segment I/O error: {error}"),
            other => write!(formatter, "{other:?}"),
        }
    }
}
impl std::error::Error for SegmentError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            _ => None,
        }
    }
}
impl From<io::Error> for SegmentError {
    fn from(value: io::Error) -> Self {
        Self::Io(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn segment() -> Segment {
        Segment::build(
            SegmentType::Availability,
            3,
            GenerationId::new(7),
            LogSequenceNumber::new(10),
            LogSequenceNumber::new(20),
            2,
            vec![1, 2, 3, 4],
        )
        .unwrap_or_else(|_| unreachable!())
    }

    fn path(label: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "veyra-segment-{label}-{}-{nanos}.bin",
            std::process::id()
        ))
    }

    #[test]
    fn round_trip_preserves_header_and_payload() {
        let segment = segment();
        let bytes = segment.encode().unwrap_or_else(|_| unreachable!());
        let decoded = Segment::decode(&bytes).unwrap_or_else(|_| unreachable!());
        assert_eq!(decoded, segment);
        assert_eq!(decoded.header().generation.get(), 7);
        assert_eq!(decoded.payload(), &[1, 2, 3, 4]);
    }

    #[test]
    fn corrupt_and_unknown_formats_fail_closed() {
        let mut bytes = segment().encode().unwrap_or_else(|_| unreachable!());
        bytes[0] = b'X';
        assert!(matches!(
            Segment::decode(&bytes),
            Err(SegmentError::InvalidMagic)
        ));
        let mut bytes = segment().encode().unwrap_or_else(|_| unreachable!());
        bytes[4..6].copy_from_slice(&99_u16.to_le_bytes());
        assert!(matches!(
            Segment::decode(&bytes),
            Err(SegmentError::UnsupportedVersion(99))
        ));
        let mut bytes = segment().encode().unwrap_or_else(|_| unreachable!());
        bytes[6..8].copy_from_slice(&99_u16.to_le_bytes());
        assert!(matches!(
            Segment::decode(&bytes),
            Err(SegmentError::UnknownSegmentType(99))
        ));
    }

    #[test]
    fn checksum_length_and_lsn_errors_fail_closed() {
        let mut bytes = segment().encode().unwrap_or_else(|_| unreachable!());
        let last = bytes.len() - 1;
        bytes[last] ^= 0xff;
        assert!(matches!(
            Segment::decode(&bytes),
            Err(SegmentError::ChecksumMismatch)
        ));
        let mut bytes = segment().encode().unwrap_or_else(|_| unreachable!());
        bytes.pop();
        assert!(matches!(
            Segment::decode(&bytes),
            Err(SegmentError::LengthMismatch { .. })
        ));
        assert!(matches!(
            Segment::build(
                SegmentType::Property,
                0,
                GenerationId::new(1),
                LogSequenceNumber::new(2),
                LogSequenceNumber::new(1),
                0,
                Vec::new()
            ),
            Err(SegmentError::LsnRangeReversed)
        ));
    }

    #[test]
    fn atomic_file_round_trip() -> Result<(), SegmentError> {
        let path = path("atomic");
        let segment = segment();
        segment.write_atomic(&path)?;
        assert_eq!(Segment::read(&path)?, segment);
        let _ = std::fs::remove_file(path);
        Ok(())
    }

    #[test]
    fn crc_standard_vector() {
        assert_eq!(crc32c(b"123456789"), 0xe306_9283);
    }
}
