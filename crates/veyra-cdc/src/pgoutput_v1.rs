use core::fmt;

use veyra_types::LogSequenceNumber;

use crate::{ChangeKind, RowChange};

const MAX_COLUMNS: usize = 1_024;
const MAX_COLUMN_BYTES: usize = 8 * 1024 * 1024;
const MAX_RELATION_NAME_BYTES: usize = 1_024;
const MAX_TRUNCATE_RELATIONS: usize = 4_096;

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
        let count = u16::try_from(self.columns.len())
            .map_err(|_| PgOutputError::TooManyColumns(self.columns.len()))?;
        if self.columns.len() > MAX_COLUMNS {
            return Err(PgOutputError::TooManyColumns(self.columns.len()));
        }
        let mut out = Vec::new();
        out.extend_from_slice(&count.to_le_bytes());
        for column in &self.columns {
            match column {
                TupleColumn::Null => out.push(b'n'),
                TupleColumn::UnchangedToast => out.push(b'u'),
                TupleColumn::Text(bytes) => encode_bytes(&mut out, b't', bytes)?,
                TupleColumn::Binary(bytes) => encode_bytes(&mut out, b'b', bytes)?,
            }
        }
        Ok(out)
    }
}

fn encode_bytes(out: &mut Vec<u8>, kind: u8, bytes: &[u8]) -> Result<(), PgOutputError> {
    if bytes.len() > MAX_COLUMN_BYTES {
        return Err(PgOutputError::ColumnTooLarge(bytes.len()));
    }
    let len = u32::try_from(bytes.len()).map_err(|_| PgOutputError::ColumnTooLarge(bytes.len()))?;
    out.push(kind);
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(bytes);
    Ok(())
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
    let tuple = decode_tuple(cursor)?.encode()?;
    Ok(RowChange::new(relation_id, ChangeKind::Insert, None, Some(tuple)))
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
    Ok(RelationMetadata { relation_id, namespace, name, replica_identity, columns })
}

fn decode_update(cursor: &mut Cursor<'_>) -> Result<RowChange, PgOutputError> {
    let relation_id = cursor.u32_be()?;
    let first = cursor.u8()?;
    let (old_tuple, next_tag) = match first {
        b'K' | b'O' => (Some(decode_tuple(cursor)?.encode()?), cursor.u8()?),
        b'N' => (None, b'N'),
        other => return Err(PgOutputError::InvalidTupleTag(other)),
    };
    if next_tag != b'N' {
        return Err(PgOutputError::InvalidTupleTag(next_tag));
    }
    let new_tuple = decode_tuple(cursor)?.encode()?;
    Ok(RowChange::new(relation_id, ChangeKind::Update, old_tuple, Some(new_tuple)))
}

fn decode_delete(cursor: &mut Cursor<'_>) -> Result<RowChange, PgOutputError> {
    let relation_id = cursor.u32_be()?;
    let tag = cursor.u8()?;
    if !matches!(tag, b'K' | b'O') {
        return Err(PgOutputError::InvalidTupleTag(tag));
    }
    let old_tuple = decode_tuple(cursor)?.encode()?;
    Ok(RowChange::new(relation_id, ChangeKind::Delete, Some(old_tuple), None))
}

fn decode_truncate(cursor: &mut Cursor<'_>) -> Result<PgOutputMessage, PgOutputError> {
    let raw_count = cursor.u32_be()?;
    let count = usize::try_from(raw_count).map_err(|_| PgOutputError::InvalidTruncateCount(usize::MAX))?;
    if count == 0 || count > MAX_TRUNCATE_RELATIONS {
        return Err(PgOutputError::InvalidTruncateCount(count));
    }
    let options = cursor.u8()?;
    let mut relation_ids = Vec::with_capacity(count);
    for _ in 0..count {
        relation_ids.push(cursor.u32_be()?);
    }
    Ok(PgOutputMessage::Truncate { relation_ids, options })
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
    const fn new(input: &'a [u8]) -> Self { Self { input, offset: 0 } }

    fn take(&mut self, len: usize) -> Result<&'a [u8], PgOutputError> {
        let end = self.offset.checked_add(len).ok_or(PgOutputError::LengthOverflow)?;
        let bytes = self.input.get(self.offset..end).ok_or(PgOutputError::UnexpectedEof)?;
        self.offset = end;
        Ok(bytes)
    }
    fn u8(&mut self) -> Result<u8, PgOutputError> { Ok(self.take(1)?[0]) }
    fn u16_be(&mut self) -> Result<u16, PgOutputError> {
        Ok(u16::from_be_bytes(self.take(2)?.try_into().map_err(|_| PgOutputError::UnexpectedEof)?))
    }
    fn u32_be(&mut self) -> Result<u32, PgOutputError> {
        Ok(u32::from_be_bytes(self.take(4)?.try_into().map_err(|_| PgOutputError::UnexpectedEof)?))
    }
    fn i32_be(&mut self) -> Result<i32, PgOutputError> {
        Ok(i32::from_be_bytes(self.take(4)?.try_into().map_err(|_| PgOutputError::UnexpectedEof)?))
    }
    fn u64_be(&mut self) -> Result<u64, PgOutputError> {
        Ok(u64::from_be_bytes(self.take(8)?.try_into().map_err(|_| PgOutputError::UnexpectedEof)?))
    }
    fn i64_be(&mut self) -> Result<i64, PgOutputError> {
        Ok(i64::from_be_bytes(self.take(8)?.try_into().map_err(|_| PgOutputError::UnexpectedEof)?))
    }
    fn cstring(&mut self, max: usize) -> Result<String, PgOutputError> {
        let tail = self.input.get(self.offset..).ok_or(PgOutputError::UnexpectedEof)?;
        let terminator = tail.iter().position(|byte| *byte == 0).ok_or(PgOutputError::UnterminatedCString)?;
        if terminator > max { return Err(PgOutputError::StringTooLong(terminator)); }
        let bytes = self.take(terminator)?;
        let _ = self.u8()?;
        Ok(core::str::from_utf8(bytes).map_err(|_| PgOutputError::InvalidUtf8)?.to_owned())
    }
    fn bytes_with_u32_len(&mut self, max: usize) -> Result<Vec<u8>, PgOutputError> {
        let raw = self.u32_be()?;
        let len = usize::try_from(raw).map_err(|_| PgOutputError::LengthOverflow)?;
        if len > max { return Err(PgOutputError::ColumnTooLarge(len)); }
        Ok(self.take(len)?.to_vec())
    }
    fn tag(&mut self, expected: u8) -> Result<(), PgOutputError> {
        let actual = self.u8()?;
        if actual == expected { Ok(()) } else { Err(PgOutputError::InvalidTupleTag(actual)) }
    }
    fn finish(&self) -> Result<(), PgOutputError> {
        if self.offset == self.input.len() { Ok(()) } else { Err(PgOutputError::TrailingBytes(self.input.len() - self.offset)) }
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
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result { write!(formatter, "{self:?}") }
}
impl std::error::Error for PgOutputError {}

#[cfg(test)]
mod tests {
    use super::*;

    fn tuple_text(bytes: &[u8]) -> Vec<u8> {
        let mut out = vec![0, 1, b't'];
        let len = u32::try_from(bytes.len()).unwrap_or_default();
        out.extend_from_slice(&len.to_be_bytes());
        out.extend_from_slice(bytes);
        out
    }

    #[test]
    fn decodes_begin_commit_and_relation() {
        let mut begin = vec![b'B'];
        begin.extend_from_slice(&10_u64.to_be_bytes());
        begin.extend_from_slice(&20_i64.to_be_bytes());
        begin.extend_from_slice(&30_u32.to_be_bytes());
        assert_eq!(PgOutputDecoder::decode(&begin), Ok(PgOutputMessage::Begin { final_lsn: LogSequenceNumber::new(10), commit_timestamp_micros: 20, xid: 30 }));

        let mut commit = vec![b'C', 0];
        commit.extend_from_slice(&40_u64.to_be_bytes());
        commit.extend_from_slice(&41_u64.to_be_bytes());
        commit.extend_from_slice(&50_i64.to_be_bytes());
        assert!(matches!(PgOutputDecoder::decode(&commit), Ok(PgOutputMessage::Commit { commit_lsn, .. }) if commit_lsn.get() == 40));

        let mut relation = vec![b'R'];
        relation.extend_from_slice(&7_u32.to_be_bytes());
        relation.extend_from_slice(b"public\0hotels\0");
        relation.push(b'd');
        relation.extend_from_slice(&1_u16.to_be_bytes());
        relation.push(1);
        relation.extend_from_slice(b"id\0");
        relation.extend_from_slice(&2950_u32.to_be_bytes());
        relation.extend_from_slice(&(-1_i32).to_be_bytes());
        let decoded = PgOutputDecoder::decode(&relation).unwrap_or_else(|_| unreachable!());
        assert!(matches!(decoded, PgOutputMessage::Relation(meta) if meta.relation_id == 7 && meta.columns.len() == 1));
    }

    #[test]
    fn decodes_row_changes_and_truncate() {
        let tuple = tuple_text(b"abc");
        let mut insert = vec![b'I'];
        insert.extend_from_slice(&7_u32.to_be_bytes());
        insert.push(b'N');
        insert.extend_from_slice(&tuple);
        assert!(matches!(PgOutputDecoder::decode(&insert), Ok(PgOutputMessage::Change(change)) if change.kind == ChangeKind::Insert));

        let mut update = vec![b'U'];
        update.extend_from_slice(&7_u32.to_be_bytes());
        update.push(b'K');
        update.extend_from_slice(&tuple);
        update.push(b'N');
        update.extend_from_slice(&tuple);
        assert!(matches!(PgOutputDecoder::decode(&update), Ok(PgOutputMessage::Change(change)) if change.kind == ChangeKind::Update && change.old_tuple.is_some()));

        let mut delete = vec![b'D'];
        delete.extend_from_slice(&7_u32.to_be_bytes());
        delete.push(b'O');
        delete.extend_from_slice(&tuple);
        assert!(matches!(PgOutputDecoder::decode(&delete), Ok(PgOutputMessage::Change(change)) if change.kind == ChangeKind::Delete));

        let mut truncate = vec![b'T'];
        truncate.extend_from_slice(&2_u32.to_be_bytes());
        truncate.push(3);
        truncate.extend_from_slice(&9_u32.to_be_bytes());
        truncate.extend_from_slice(&10_u32.to_be_bytes());
        assert_eq!(PgOutputDecoder::decode(&truncate), Ok(PgOutputMessage::Truncate { relation_ids: vec![9, 10], options: 3 }));
    }

    #[test]
    fn tuple_encoding_preserves_all_kinds() {
        let tuple = TupleData { columns: vec![TupleColumn::Null, TupleColumn::UnchangedToast, TupleColumn::Text(b"x".to_vec()), TupleColumn::Binary(vec![1, 2])] };
        let encoded = tuple.encode().unwrap_or_else(|_| unreachable!());
        assert!(!encoded.is_empty());
    }

    #[test]
    fn rejects_malformed_and_bounded_inputs() {
        assert_eq!(PgOutputDecoder::decode(&[]), Err(PgOutputError::UnexpectedEof));
        assert_eq!(PgOutputDecoder::decode(b"X"), Err(PgOutputError::UnsupportedMessage(b'X')));
        let mut bad = vec![b'I'];
        bad.extend_from_slice(&1_u32.to_be_bytes());
        bad.push(b'K');
        assert_eq!(PgOutputDecoder::decode(&bad), Err(PgOutputError::InvalidTupleTag(b'K')));

        let mut huge = vec![b'I'];
        huge.extend_from_slice(&1_u32.to_be_bytes());
        huge.push(b'N');
        huge.extend_from_slice(&1_u16.to_be_bytes());
        huge.push(b't');
        huge.extend_from_slice(&((MAX_COLUMN_BYTES as u32) + 1).to_be_bytes());
        assert_eq!(PgOutputDecoder::decode(&huge), Err(PgOutputError::ColumnTooLarge(MAX_COLUMN_BYTES + 1)));
    }

    #[test]
    fn rejects_relation_and_tuple_semantic_errors() {
        let mut relation = vec![b'R'];
        relation.extend_from_slice(&1_u32.to_be_bytes());
        relation.extend_from_slice(b"p\0t\0");
        relation.push(b'x');
        relation.extend_from_slice(&0_u16.to_be_bytes());
        assert_eq!(PgOutputDecoder::decode(&relation), Err(PgOutputError::InvalidReplicaIdentity(b'x')));

        let mut trailing = vec![b'B'];
        trailing.extend_from_slice(&1_u64.to_be_bytes());
        trailing.extend_from_slice(&2_i64.to_be_bytes());
        trailing.extend_from_slice(&3_u32.to_be_bytes());
        trailing.push(0);
        assert_eq!(PgOutputDecoder::decode(&trailing), Err(PgOutputError::TrailingBytes(1)));
    }
}
