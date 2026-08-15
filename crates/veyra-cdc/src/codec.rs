use core::fmt;

use veyra_types::LogSequenceNumber;

use crate::model::{
    BatchLimits, BatchValidationError, LogicalMessage, TransactionBatch, TransactionItem, WalChunk,
};

const MAGIC: &[u8; 8] = b"VYRTXN01";
const VERSION: u16 = 1;
const HEADER_LEN: usize = 48;
#[cfg(test)]
const WAL_FIXED_LEN: usize = 29;
#[cfg(test)]
const MESSAGE_FIXED_LEN: usize = 13;
const ITEM_WAL: u8 = 1;
const ITEM_MESSAGE: u8 = 2;

/// Encodes one complete transaction using Veyra-owned canonical little-endian bytes.
pub fn encode_transaction_batch(
    batch: &TransactionBatch,
    limits: BatchLimits,
) -> Result<Vec<u8>, BatchCodecError> {
    let item_count = u32::try_from(batch.items().len()).map_err(|_| BatchCodecError::LengthOverflow)?;
    let mut capacity = HEADER_LEN;
    for item in batch.items() {
        capacity = capacity
            .checked_add(encoded_item_len(item)?)
            .ok_or(BatchCodecError::LengthOverflow)?;
    }
    if capacity > limits.max_encoded_bytes {
        return Err(BatchCodecError::EncodedTooLarge {
            actual: capacity,
            maximum: limits.max_encoded_bytes,
        });
    }

    let mut output = Vec::with_capacity(capacity);
    output.extend_from_slice(MAGIC);
    put_u16(&mut output, VERSION);
    put_u16(&mut output, 0);
    put_u32(&mut output, batch.xid());
    put_u64(&mut output, batch.begin_final_lsn().get());
    put_u64(&mut output, batch.commit_lsn().get());
    put_u64(&mut output, batch.end_lsn().get());
    put_i64(&mut output, batch.commit_time_micros());
    put_u32(&mut output, item_count);

    for item in batch.items() {
        match item {
            TransactionItem::Wal(chunk) => {
                output.push(ITEM_WAL);
                put_u64(&mut output, chunk.wal_start().get());
                put_u64(&mut output, chunk.wal_end().get());
                put_i64(&mut output, chunk.server_time_micros());
                put_u32(&mut output, checked_u32(chunk.data().len())?);
                output.extend_from_slice(chunk.data());
            }
            TransactionItem::Message(message) => {
                output.push(ITEM_MESSAGE);
                put_u64(&mut output, message.lsn().get());
                put_u32(&mut output, checked_u32(message.prefix().len())?);
                output.extend_from_slice(message.prefix().as_bytes());
                put_u32(&mut output, checked_u32(message.content().len())?);
                output.extend_from_slice(message.content());
            }
        }
    }

    debug_assert_eq!(output.len(), capacity);
    Ok(output)
}

/// Decodes and validates untrusted durable bytes.
pub fn decode_transaction_batch(
    bytes: &[u8],
    limits: BatchLimits,
) -> Result<TransactionBatch, BatchCodecError> {
    if bytes.len() > limits.max_encoded_bytes {
        return Err(BatchCodecError::EncodedTooLarge {
            actual: bytes.len(),
            maximum: limits.max_encoded_bytes,
        });
    }
    let mut cursor = Cursor::new(bytes);
    if cursor.take(8)? != MAGIC {
        return Err(BatchCodecError::InvalidMagic);
    }
    let version = cursor.u16()?;
    if version != VERSION {
        return Err(BatchCodecError::UnsupportedVersion(version));
    }
    if cursor.u16()? != 0 {
        return Err(BatchCodecError::NonZeroReserved);
    }
    let xid = cursor.u32()?;
    let begin_final_lsn = LogSequenceNumber::new(cursor.u64()?);
    let commit_lsn = LogSequenceNumber::new(cursor.u64()?);
    let end_lsn = LogSequenceNumber::new(cursor.u64()?);
    let commit_time_micros = cursor.i64()?;
    let item_count = usize::try_from(cursor.u32()?).map_err(|_| BatchCodecError::LengthOverflow)?;
    if item_count > limits.max_items {
        return Err(BatchCodecError::TooManyItems {
            actual: item_count,
            maximum: limits.max_items,
        });
    }
    let mut items = Vec::with_capacity(item_count);
    for _ in 0..item_count {
        let tag = cursor.u8()?;
        match tag {
            ITEM_WAL => {
                let wal_start = LogSequenceNumber::new(cursor.u64()?);
                let wal_end = LogSequenceNumber::new(cursor.u64()?);
                let server_time_micros = cursor.i64()?;
                let len = cursor.bounded_len(limits.max_item_bytes)?;
                let data = cursor.take(len)?.to_vec();
                items.push(TransactionItem::Wal(WalChunk::try_new(
                    wal_start,
                    wal_end,
                    server_time_micros,
                    data,
                    limits,
                )?));
            }
            ITEM_MESSAGE => {
                let lsn = LogSequenceNumber::new(cursor.u64()?);
                let prefix_len = cursor.bounded_len(limits.max_prefix_bytes)?;
                let prefix_bytes = cursor.take(prefix_len)?;
                let prefix = core::str::from_utf8(prefix_bytes)
                    .map_err(|_| BatchCodecError::InvalidUtf8Prefix)?
                    .to_owned();
                let content_len = cursor.bounded_len(limits.max_item_bytes)?;
                let content = cursor.take(content_len)?.to_vec();
                items.push(TransactionItem::Message(LogicalMessage::try_new(
                    lsn, prefix, content, limits,
                )?));
            }
            other => return Err(BatchCodecError::UnknownItemTag(other)),
        }
    }
    if cursor.remaining() != 0 {
        return Err(BatchCodecError::TrailingBytes(cursor.remaining()));
    }
    TransactionBatch::try_new(
        xid,
        begin_final_lsn,
        commit_lsn,
        end_lsn,
        commit_time_micros,
        items,
        limits,
    )
    .map_err(BatchCodecError::Validation)
}

fn encoded_item_len(item: &TransactionItem) -> Result<usize, BatchCodecError> {
    match item {
        TransactionItem::Wal(chunk) => 29usize
            .checked_add(chunk.data().len())
            .ok_or(BatchCodecError::LengthOverflow),
        TransactionItem::Message(message) => 17usize
            .checked_add(message.prefix().len())
            .and_then(|value| value.checked_add(message.content().len()))
            .ok_or(BatchCodecError::LengthOverflow),
    }
}

fn checked_u32(value: usize) -> Result<u32, BatchCodecError> {
    u32::try_from(value).map_err(|_| BatchCodecError::LengthOverflow)
}

fn put_u16(output: &mut Vec<u8>, value: u16) {
    output.extend_from_slice(&value.to_le_bytes());
}

fn put_u32(output: &mut Vec<u8>, value: u32) {
    output.extend_from_slice(&value.to_le_bytes());
}

fn put_u64(output: &mut Vec<u8>, value: u64) {
    output.extend_from_slice(&value.to_le_bytes());
}

fn put_i64(output: &mut Vec<u8>, value: i64) {
    output.extend_from_slice(&value.to_le_bytes());
}

struct Cursor<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> Cursor<'a> {
    const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    fn take(&mut self, len: usize) -> Result<&'a [u8], BatchCodecError> {
        let end = self
            .offset
            .checked_add(len)
            .ok_or(BatchCodecError::LengthOverflow)?;
        let slice = self
            .bytes
            .get(self.offset..end)
            .ok_or(BatchCodecError::UnexpectedEof)?;
        self.offset = end;
        Ok(slice)
    }

    fn remaining(&self) -> usize {
        self.bytes.len().saturating_sub(self.offset)
    }

    fn u8(&mut self) -> Result<u8, BatchCodecError> {
        Ok(self.take(1)?[0])
    }

    fn u16(&mut self) -> Result<u16, BatchCodecError> {
        let bytes = self.take(2)?;
        Ok(u16::from_le_bytes([bytes[0], bytes[1]]))
    }

    fn u32(&mut self) -> Result<u32, BatchCodecError> {
        let bytes = self.take(4)?;
        Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    fn u64(&mut self) -> Result<u64, BatchCodecError> {
        let bytes = self.take(8)?;
        Ok(u64::from_le_bytes([
            bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
        ]))
    }

    fn i64(&mut self) -> Result<i64, BatchCodecError> {
        let bytes = self.take(8)?;
        Ok(i64::from_le_bytes([
            bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
        ]))
    }

    fn bounded_len(&mut self, maximum: usize) -> Result<usize, BatchCodecError> {
        let value = usize::try_from(self.u32()?).map_err(|_| BatchCodecError::LengthOverflow)?;
        if value > maximum {
            return Err(BatchCodecError::LengthTooLarge {
                actual: value,
                maximum,
            });
        }
        Ok(value)
    }
}

/// Corrupt, incompatible or unbounded transaction bytes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BatchCodecError {
    InvalidMagic,
    UnsupportedVersion(u16),
    NonZeroReserved,
    UnexpectedEof,
    LengthOverflow,
    LengthTooLarge { actual: usize, maximum: usize },
    TooManyItems { actual: usize, maximum: usize },
    InvalidUtf8Prefix,
    UnknownItemTag(u8),
    TrailingBytes(usize),
    EncodedTooLarge { actual: usize, maximum: usize },
    Validation(BatchValidationError),
}

impl From<BatchValidationError> for BatchCodecError {
    fn from(value: BatchValidationError) -> Self {
        Self::Validation(value)
    }
}

impl fmt::Display for BatchCodecError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidMagic => formatter.write_str("invalid Veyra transaction magic"),
            Self::UnsupportedVersion(version) => {
                write!(formatter, "unsupported Veyra transaction version {version}")
            }
            Self::NonZeroReserved => formatter.write_str("non-zero reserved transaction field"),
            Self::UnexpectedEof => formatter.write_str("unexpected end of transaction bytes"),
            Self::LengthOverflow => formatter.write_str("transaction length overflow"),
            Self::LengthTooLarge { actual, maximum } => {
                write!(formatter, "encoded field is {actual} bytes; maximum is {maximum}")
            }
            Self::TooManyItems { actual, maximum } => {
                write!(formatter, "encoded transaction has {actual} items; maximum is {maximum}")
            }
            Self::InvalidUtf8Prefix => formatter.write_str("logical message prefix is not valid UTF-8"),
            Self::UnknownItemTag(tag) => write!(formatter, "unknown transaction item tag {tag}"),
            Self::TrailingBytes(count) => write!(formatter, "transaction contains {count} trailing bytes"),
            Self::EncodedTooLarge { actual, maximum } => {
                write!(formatter, "encoded transaction is {actual} bytes; maximum is {maximum}")
            }
            Self::Validation(error) => write!(formatter, "transaction validation failed: {error}"),
        }
    }
}

impl std::error::Error for BatchCodecError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Validation(error) => Some(error),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::sample_batch;

    #[test]
    fn batch_round_trip_is_deterministic() -> Result<(), BatchCodecError> {
        let batch = sample_batch();
        let a = encode_transaction_batch(&batch, BatchLimits::default())?;
        let b = encode_transaction_batch(&batch, BatchLimits::default())?;
        assert_eq!(a, b);
        assert_eq!(decode_transaction_batch(&a, BatchLimits::default())?, batch);
        Ok(())
    }

    #[test]
    fn canonical_sizes_are_exact() {
        assert_eq!(HEADER_LEN, 48);
        assert_eq!(WAL_FIXED_LEN, 29);
        assert_eq!(MESSAGE_FIXED_LEN, 13);
    }

    #[test]
    fn rejects_corrupt_header_variants() {
        let batch = sample_batch();
        let bytes = encode_transaction_batch(&batch, BatchLimits::default())
            .unwrap_or_else(|error| unreachable!("static batch must encode: {error}"));

        let mut invalid_magic = bytes.clone();
        invalid_magic[0] ^= 1;
        assert_eq!(
            decode_transaction_batch(&invalid_magic, BatchLimits::default()),
            Err(BatchCodecError::InvalidMagic)
        );

        let mut unsupported = bytes.clone();
        unsupported[8..10].copy_from_slice(&2u16.to_le_bytes());
        assert_eq!(
            decode_transaction_batch(&unsupported, BatchLimits::default()),
            Err(BatchCodecError::UnsupportedVersion(2))
        );

        let mut reserved = bytes.clone();
        reserved[10..12].copy_from_slice(&1u16.to_le_bytes());
        assert_eq!(
            decode_transaction_batch(&reserved, BatchLimits::default()),
            Err(BatchCodecError::NonZeroReserved)
        );
    }

    #[test]
    fn rejects_unknown_tags_trailing_and_utf8() {
        let batch = sample_batch();
        let bytes = encode_transaction_batch(&batch, BatchLimits::default())
            .unwrap_or_else(|error| unreachable!("static batch must encode: {error}"));

        let mut unknown = bytes.clone();
        unknown[HEADER_LEN] = 99;
        assert_eq!(
            decode_transaction_batch(&unknown, BatchLimits::default()),
            Err(BatchCodecError::UnknownItemTag(99))
        );

        let mut trailing = bytes.clone();
        trailing.push(0);
        assert_eq!(
            decode_transaction_batch(&trailing, BatchLimits::default()),
            Err(BatchCodecError::TrailingBytes(1))
        );

        let mut invalid_utf8 = bytes.clone();
        let message_tag = HEADER_LEN + WAL_FIXED_LEN + 3;
        invalid_utf8[message_tag] = 0xFF;
        assert!(matches!(
            decode_transaction_batch(&invalid_utf8, BatchLimits::default()),
            Err(BatchCodecError::InvalidUtf8Prefix)
        ));
    }

    #[test]
    fn bounds_and_eof_fail_closed() {
        let batch = sample_batch();
        let bytes = encode_transaction_batch(&batch, BatchLimits::default())
            .unwrap_or_else(|error| unreachable!("static batch must encode: {error}"));
        for end in 0..HEADER_LEN {
            assert!(matches!(
                decode_transaction_batch(&bytes[..end], BatchLimits::default()),
                Err(BatchCodecError::UnexpectedEof)
                    | Err(BatchCodecError::InvalidMagic)
            ));
        }

        let tiny = BatchLimits {
            max_encoded_bytes: 1,
            ..BatchLimits::default()
        };
        assert!(matches!(
            decode_transaction_batch(&bytes, tiny),
            Err(BatchCodecError::EncodedTooLarge { .. })
        ));
        assert!(matches!(
            encode_transaction_batch(&batch, tiny),
            Err(BatchCodecError::EncodedTooLarge { .. })
        ));

        let mut too_many = bytes.clone();
        too_many[44..48].copy_from_slice(&3u32.to_le_bytes());
        let item_bound = BatchLimits {
            max_items: 2,
            ..BatchLimits::default()
        };
        assert!(matches!(
            decode_transaction_batch(&too_many, item_bound),
            Err(BatchCodecError::TooManyItems { .. })
        ));

        let mut item_too_large = bytes.clone();
        let wal_len_offset = HEADER_LEN + 1 + 8 + 8 + 8;
        item_too_large[wal_len_offset..wal_len_offset + 4]
            .copy_from_slice(&10u32.to_le_bytes());
        let small_item = BatchLimits {
            max_item_bytes: 4,
            ..BatchLimits::default()
        };
        assert!(matches!(
            decode_transaction_batch(&item_too_large, small_item),
            Err(BatchCodecError::LengthTooLarge { .. })
        ));
    }

    #[test]
    fn semantic_validation_is_reapplied_on_decode() {
        let batch = sample_batch();
        let mut bytes = encode_transaction_batch(&batch, BatchLimits::default())
            .unwrap_or_else(|error| unreachable!("static batch must encode: {error}"));
        bytes[16..24].copy_from_slice(&99u64.to_le_bytes());
        assert!(matches!(
            decode_transaction_batch(&bytes, BatchLimits::default()),
            Err(BatchCodecError::Validation(BatchValidationError::CommitLsnMismatch { .. }))
        ));
    }

    #[test]
    fn codec_diagnostics_and_sources_are_stable() {
        let errors = [
            BatchCodecError::InvalidMagic,
            BatchCodecError::UnsupportedVersion(2),
            BatchCodecError::NonZeroReserved,
            BatchCodecError::UnexpectedEof,
            BatchCodecError::LengthOverflow,
            BatchCodecError::LengthTooLarge { actual: 2, maximum: 1 },
            BatchCodecError::TooManyItems { actual: 2, maximum: 1 },
            BatchCodecError::InvalidUtf8Prefix,
            BatchCodecError::UnknownItemTag(3),
            BatchCodecError::TrailingBytes(2),
            BatchCodecError::EncodedTooLarge { actual: 2, maximum: 1 },
        ];
        for error in errors {
            assert!(!error.to_string().is_empty());
            assert!(std::error::Error::source(&error).is_none());
        }
        let validation = BatchCodecError::Validation(BatchValidationError::EndBeforeCommit);
        assert!(std::error::Error::source(&validation).is_some());
    }
}
