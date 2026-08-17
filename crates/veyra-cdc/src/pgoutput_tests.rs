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
    assert_eq!(
        PgOutputDecoder::decode(&begin),
        Ok(PgOutputMessage::Begin {
            final_lsn: LogSequenceNumber::new(10),
            commit_timestamp_micros: 20,
            xid: 30
        })
    );

    let mut commit = vec![b'C', 0];
    commit.extend_from_slice(&40_u64.to_be_bytes());
    commit.extend_from_slice(&41_u64.to_be_bytes());
    commit.extend_from_slice(&50_i64.to_be_bytes());
    assert!(
        matches!(PgOutputDecoder::decode(&commit), Ok(PgOutputMessage::Commit { commit_lsn, .. }) if commit_lsn.get() == 40)
    );

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
    assert!(
        matches!(decoded, PgOutputMessage::Relation(meta) if meta.relation_id == 7 && meta.columns.len() == 1)
    );
}

#[test]
fn decodes_row_changes_and_truncate() {
    let tuple = tuple_text(b"abc");
    let mut insert = vec![b'I'];
    insert.extend_from_slice(&7_u32.to_be_bytes());
    insert.push(b'N');
    insert.extend_from_slice(&tuple);
    assert!(
        matches!(PgOutputDecoder::decode(&insert), Ok(PgOutputMessage::Change(change)) if change.kind == ChangeKind::Insert)
    );

    let mut update = vec![b'U'];
    update.extend_from_slice(&7_u32.to_be_bytes());
    update.push(b'K');
    update.extend_from_slice(&tuple);
    update.push(b'N');
    update.extend_from_slice(&tuple);
    assert!(
        matches!(PgOutputDecoder::decode(&update), Ok(PgOutputMessage::Change(change)) if change.kind == ChangeKind::Update && change.old_tuple.is_some())
    );

    let mut delete = vec![b'D'];
    delete.extend_from_slice(&7_u32.to_be_bytes());
    delete.push(b'O');
    delete.extend_from_slice(&tuple);
    assert!(
        matches!(PgOutputDecoder::decode(&delete), Ok(PgOutputMessage::Change(change)) if change.kind == ChangeKind::Delete)
    );

    let mut truncate = vec![b'T'];
    truncate.extend_from_slice(&2_u32.to_be_bytes());
    truncate.push(3);
    truncate.extend_from_slice(&9_u32.to_be_bytes());
    truncate.extend_from_slice(&10_u32.to_be_bytes());
    assert_eq!(
        PgOutputDecoder::decode(&truncate),
        Ok(PgOutputMessage::Truncate {
            relation_ids: vec![9, 10],
            options: 3
        })
    );
}

#[test]
fn tuple_encoding_preserves_all_kinds() {
    let tuple = TupleData {
        columns: vec![
            TupleColumn::Null,
            TupleColumn::UnchangedToast,
            TupleColumn::Text(b"x".to_vec()),
            TupleColumn::Binary(vec![1, 2]),
        ],
    };
    let encoded = tuple.encode().unwrap_or_else(|_| unreachable!());
    assert!(!encoded.is_empty());
}

#[test]
fn rejects_malformed_and_bounded_inputs() {
    assert_eq!(
        PgOutputDecoder::decode(&[]),
        Err(PgOutputError::UnexpectedEof)
    );
    assert_eq!(
        PgOutputDecoder::decode(b"X"),
        Err(PgOutputError::UnsupportedMessage(b'X'))
    );
    let mut bad = vec![b'I'];
    bad.extend_from_slice(&1_u32.to_be_bytes());
    bad.push(b'K');
    assert_eq!(
        PgOutputDecoder::decode(&bad),
        Err(PgOutputError::InvalidTupleTag(b'K'))
    );

    let mut huge = vec![b'I'];
    huge.extend_from_slice(&1_u32.to_be_bytes());
    huge.push(b'N');
    huge.extend_from_slice(&1_u16.to_be_bytes());
    huge.push(b't');
    let oversized = u32::try_from(MAX_COLUMN_BYTES)
        .unwrap_or(u32::MAX)
        .saturating_add(1);
    huge.extend_from_slice(&oversized.to_be_bytes());
    assert_eq!(
        PgOutputDecoder::decode(&huge),
        Err(PgOutputError::ColumnTooLarge(MAX_COLUMN_BYTES + 1))
    );
}

#[test]
fn rejects_relation_and_tuple_semantic_errors() {
    let mut relation = vec![b'R'];
    relation.extend_from_slice(&1_u32.to_be_bytes());
    relation.extend_from_slice(b"p\0t\0");
    relation.push(b'x');
    relation.extend_from_slice(&0_u16.to_be_bytes());
    assert_eq!(
        PgOutputDecoder::decode(&relation),
        Err(PgOutputError::InvalidReplicaIdentity(b'x'))
    );

    let mut trailing = vec![b'B'];
    trailing.extend_from_slice(&1_u64.to_be_bytes());
    trailing.extend_from_slice(&2_i64.to_be_bytes());
    trailing.extend_from_slice(&3_u32.to_be_bytes());
    trailing.push(0);
    assert_eq!(
        PgOutputDecoder::decode(&trailing),
        Err(PgOutputError::TrailingBytes(1))
    );
}
