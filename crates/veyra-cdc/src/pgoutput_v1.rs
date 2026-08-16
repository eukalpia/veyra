use core::fmt;

use veyra_types::LogSequenceNumber;

use crate::{ChangeKind, RowChange};

const MAX_COLUMNS: usize = 1_024;
const MAX_COLUMN_BYTES: usize = 8 * 1024 * 1024;
const MAX_RELATION_NAME_BYTES: usize = 1_024;
const MAX_TRUNCATE_RELATIONS: u32 = 4_096;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReplicaIdentity {
    Default,
    Nothing,
    Full,
    Index,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ColumnMetadata {
    pub flags: u8,
    pub name: String,
    pub type_oid: u32,
    pub type_modifier: i32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RelationMetadata {
    pub relation_id: u32,
    pub namespace: String,
    pub name: String,
    pub replica_identity: ReplicaIdentity,
    pub columns: Vec<ColumnMetadata>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TupleColumn {
    Null,
    UnchangedToast,
    Text(Vec<u8>),
    Binary(Vec<u8>),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TupleData {
    pub columns: Vec<TupleColumn>,
}

impl TupleData {
    /// Canonical internal encoding preserving tuple kinds without interpreting `PostgreSQL` types.
    pub fn encode(&self) -> Result<Vec<u8>, PgOutputError> {
        if self.columns.len() > MAX_COLUMNS {
            return Err(PgOutputError::TooManyColumns(self.columns.len()));
        }
        for column in &self.columns {
            match column {
                TupleColumn::Text(bytes) | TupleColumn::Binary(bytes)
                    if bytes.len() > MAX_COLUMN_BYTES =>
                {
                    return Err(PgOutputError::ColumnTooLarge(bytes.len()));
                }
                _ => {}
            }
        }
        Ok(self.encode_validated())
    }

    fn encode_validated(&self) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&usize_to_u16(self.columns.len()).to_le_bytes());
        for column in &self.columns {
            match column {
                TupleColumn::Null => out.push(b'n'),
                TupleColumn::UnchangedToast => out.push(b'u'),
                TupleColumn::Text(bytes) => encode_bytes_validated(&mut out, b't', bytes),
                TupleColumn::Binary(bytes) => encode_bytes_validated(&mut out, b'b', bytes),
            }
        }
        out
    }
}

fn encode_bytes_validated(out: &mut Vec<u8>, kind: u8, bytes: &[u8]) {
    out.push(kind);
    out.extend_from_slice(&usize_to_u32(bytes.len()).to_le_bytes());
    out.extend_from_slice(bytes);
}

#[allow(clippy::cast_possible_truncation)]
const fn usize_to_u16(value: usize) -> u16 {
    value as u16
}

#[allow(clippy::cast_possible_truncation)]
const fn usize_to_u32(value: usize) -> u32 {
    value as u32
}

#[allow(clippy::cast_possible_truncation)]
const fn u32_to_usize(value: u32) -> usize {
    value as usize
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PgOutputMessage {
    Begin {
        final_lsn: LogSequenceNumber,
        commit_timestamp_micros: i64,
        xid: u32,
    },
    Commit {
        flags: u8,
        commit_lsn: LogSequenceNumber,
        end_lsn: LogSequenceNumber,
        commit_timestamp_micros: i64,
    },
    Relation(RelationMetadata),
    Change(RowChange),
    Truncate {
        relation_ids: Vec<u32>,
        options: u8,
    },
}

#[derive(Clone, Copy, Debug, Default)]
pub struct PgOutputDecoder;

impl PgOutputDecoder {
    pub fn decode(input: &[u8]) -> Result<PgOutputMessage, PgOutputError> {
        let (&tag, payload) = input.split_first().ok_or(PgOutputError::UnexpectedEof)?;
        let mut cursor = Cursor::new(payload);
        let message = match tag {
            b'B' => PgOutputMessage::Begin {
                final_lsn: LogSequenceNumber::new(cursor.u64_be()?),
                commit_timestamp_micros: cursor.i64_be()?,
                xid: cursor.u32_be()?,
            },
            b'C' => PgOutputMessage::Commit {
                flags: cursor.u8()?,
                commit_lsn: LogSequenceNumber::new(cursor.u64_be()?),
                end_lsn: LogSequenceNumber::new(cursor.u64_be()?),
                commit_timestamp_micros: cursor.i64_be()?,
            },
            b'R' => PgOutputMessage::Relation(decode_relation(&mut cursor)?),
            b'I' => PgOutputMessage::Change(decode_insert(&mut cursor)?),
            b'U' => PgOutputMessage::Change(decode_update(&mut cursor)?),
            b'D' => PgOutputMessage::Change(decode_delete(&mut cursor)?),
            b'T' => decode_truncate(&mut cursor)?,
            other => return Err(PgOutputError::UnsupportedMessage(other)),
        };
        cursor.finish()?;
        Ok(message)
    }
}

fn decode_insert(cursor: &mut Cursor<'_>) -> Result<RowChange, PgOutputError> {
    let relation_id = cursor.u32_be()?;
    cursor.tag(b'N')?;
    let tuple = decode_tuple(cursor)?.encode_validated();
    Ok(RowChange::new(
        relation_id,
        ChangeKind::Insert,
        None,
        Some(tuple),
    ))
}

fn decode_relation(cursor: &mut Cursor<'_>) -> Result<RelationMetadata, PgOutputError> {
    let relation_id = cursor.u32_be()?;
    let namespace = cursor.cstring(MAX_RELATION_NAME_BYTES)?;
    let name = cursor.cstring(MAX_RELATION_NAME_BYTES)?;
    let replica_identity = match cursor.u8()? {
        b'd' => ReplicaIdentity::Default,
        b'n' => ReplicaIdentity::Nothing,
        b'f' => ReplicaIdentity::Full,
        b'i' => ReplicaIdentity::Index,
        other => return Err(PgOutputError::InvalidReplicaIdentity(other)),
    };
    let count = usize::from(cursor.u16_be()?);
    if count > MAX_COLUMNS {
        return Err(PgOutputError::TooManyColumns(count));
    }
    let mut columns = Vec::with_capacity(count);
    for _ in 0..count {
        columns.push(ColumnMetadata {
            flags: cursor.u8()?,
            name: cursor.cstring(MAX_RELATION_NAME_BYTES)?,
            type_oid: cursor.u32_be()?,
            type_modifier: cursor.i32_be()?,
        });
    }
    Ok(RelationMetadata {
        relation_id,
        namespace,
        name,
        replica_identity,
        columns,
    })
}

fn decode_update(cursor: &mut Cursor<'_>) -> Result<RowChange, PgOutputError> {
    let relation_id = cursor.u32_be()?;
    let first = cursor.u8()?;
    let (old_tuple, next_tag) = match first {
        b'K' | b'O' => (Some(decode_tuple(cursor)?.encode_validated()), cursor.u8()?),
        b'N' => (None, b'N'),
        other => return Err(PgOutputError::InvalidTupleTag(other)),
    };
    if next_tag != b'N' {
        return Err(PgOutputError::InvalidTupleTag(next_tag));
    }
    let new_tuple = decode_tuple(cursor)?.encode_validated();
    Ok(RowChange::new(
        relation_id,
        ChangeKind::Update,
        old_tuple,
        Some(new_tuple),
    ))
}

fn decode_delete(cursor: &mut Cursor<'_>) -> Result<RowChange, PgOutputError> {
    let relation_id = cursor.u32_be()?;
    let tag = cursor.u8()?;
    if !matches!(tag, b'K' | b'O') {
        return Err(PgOutputError::InvalidTupleTag(tag));
    }
    let old_tuple = decode_tuple(cursor)?.encode_validated();
    Ok(RowChange::new(
        relation_id,
        ChangeKind::Delete,
        Some(old_tuple),
        None,
    ))
}

fn decode_truncate(cursor: &mut Cursor<'_>) -> Result<PgOutputMessage, PgOutputError> {
    let raw_count = cursor.u32_be()?;
    let count = u32_to_usize(raw_count);
    if raw_count == 0 || raw_count > MAX_TRUNCATE_RELATIONS {
        return Err(PgOutputError::InvalidTruncateCount(count));
    }
    let options = cursor.u8()?;
    let mut relation_ids = Vec::with_capacity(count);
    for _ in 0..count {
        relation_ids.push(cursor.u32_be()?);
    }
    Ok(PgOutputMessage::Truncate {
        relation_ids,
        options,
    })
}

fn decode_tuple(cursor: &mut Cursor<'_>) -> Result<TupleData, PgOutputError> {
    let count = usize::from(cursor.u16_be()?);
    if count > MAX_COLUMNS {
        return Err(PgOutputError::TooManyColumns(count));
    }
    let mut columns = Vec::with_capacity(count);
    for _ in 0..count {
        columns.push(match cursor.u8()? {
            b'n' => TupleColumn::Null,
            b'u' => TupleColumn::UnchangedToast,
            b't' => TupleColumn::Text(cursor.bytes_with_u32_len(MAX_COLUMN_BYTES)?),
            b'b' => TupleColumn::Binary(cursor.bytes_with_u32_len(MAX_COLUMN_BYTES)?),
            other => return Err(PgOutputError::InvalidTupleColumnKind(other)),
        });
    }
    Ok(TupleData { columns })
}

struct Cursor<'a> {
    input: &'a [u8],
    offset: usize,
}

impl<'a> Cursor<'a> {
    const fn new(input: &'a [u8]) -> Self {
        Self { input, offset: 0 }
    }

    fn take(&mut self, len: usize) -> Result<&'a [u8], PgOutputError> {
        let tail = &self.input[self.offset..];
        let bytes = tail.get(..len).ok_or(PgOutputError::UnexpectedEof)?;
        self.offset += len;
        Ok(bytes)
    }

    fn u8(&mut self) -> Result<u8, PgOutputError> {
        Ok(self.take(1)?[0])
    }

    fn u16_be(&mut self) -> Result<u16, PgOutputError> {
        let bytes = self.take(2)?;
        Ok(u16::from_be_bytes([bytes[0], bytes[1]]))
    }

    fn u32_be(&mut self) -> Result<u32, PgOutputError> {
        let bytes = self.take(4)?;
        Ok(u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    fn i32_be(&mut self) -> Result<i32, PgOutputError> {
        let bytes = self.take(4)?;
        Ok(i32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    fn u64_be(&mut self) -> Result<u64, PgOutputError> {
        let bytes = self.take(8)?;
        Ok(u64::from_be_bytes([
            bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
        ]))
    }

    fn i64_be(&mut self) -> Result<i64, PgOutputError> {
        let bytes = self.take(8)?;
        Ok(i64::from_be_bytes([
            bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
        ]))
    }

    fn cstring(&mut self, max: usize) -> Result<String, PgOutputError> {
        let tail = &self.input[self.offset..];
        let terminator = tail
            .iter()
            .position(|byte| *byte == 0)
            .ok_or(PgOutputError::UnterminatedCString)?;
        if terminator > max {
            return Err(PgOutputError::StringTooLong(terminator));
        }
        let bytes = &tail[..terminator];
        self.offset += terminator + 1;
        Ok(core::str::from_utf8(bytes)
            .map_err(|_| PgOutputError::InvalidUtf8)?
            .to_owned())
    }

    fn bytes_with_u32_len(&mut self, max: usize) -> Result<Vec<u8>, PgOutputError> {
        let len = u32_to_usize(self.u32_be()?);
        if len > max {
            return Err(PgOutputError::ColumnTooLarge(len));
        }
        Ok(self.take(len)?.to_vec())
    }

    fn tag(&mut self, expected: u8) -> Result<(), PgOutputError> {
        let actual = self.u8()?;
        if actual == expected {
            Ok(())
        } else {
            Err(PgOutputError::InvalidTupleTag(actual))
        }
    }

    fn finish(&self) -> Result<(), PgOutputError> {
        if self.offset == self.input.len() {
            Ok(())
        } else {
            Err(PgOutputError::TrailingBytes(self.input.len() - self.offset))
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PgOutputError {
    UnexpectedEof,
    LengthOverflow,
    UnsupportedMessage(u8),
    InvalidReplicaIdentity(u8),
    InvalidTupleTag(u8),
    InvalidTupleColumnKind(u8),
    TooManyColumns(usize),
    ColumnTooLarge(usize),
    StringTooLong(usize),
    UnterminatedCString,
    InvalidUtf8,
    InvalidTruncateCount(usize),
    TrailingBytes(usize),
}

impl fmt::Display for PgOutputError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{self:?}")
    }
}
impl std::error::Error for PgOutputError {}

#[cfg(test)]
#[path = "pgoutput_tests.rs"]
mod tests;
