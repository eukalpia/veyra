use veyra_cdc::{
    ChangeKind, PgOutputDecoder, PgOutputError, PgOutputMessage, ReplicaIdentity, TupleColumn,
    TupleData,
};

const MAX_COLUMN_BYTES: usize = 8 * 1024 * 1024;

fn tuple(columns: &[(u8, &[u8])]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&u16::try_from(columns.len()).unwrap_or_default().to_be_bytes());
    for (kind, bytes) in columns {
        out.push(*kind);
        if matches!(*kind, b't' | b'b') {
            out.extend_from_slice(&u32::try_from(bytes.len()).unwrap_or_default().to_be_bytes());
            out.extend_from_slice(bytes);
        }
    }
    out
}

fn relation(identity: u8, namespace: &[u8], name: &[u8], columns: u16) -> Vec<u8> {
    let mut out = vec![b'R'];
    out.extend_from_slice(&7_u32.to_be_bytes());
    out.extend_from_slice(namespace);
    out.push(0);
    out.extend_from_slice(name);
    out.push(0);
    out.push(identity);
    out.extend_from_slice(&columns.to_be_bytes());
    out
}

fn insert_with_tuple(tuple: &[u8]) -> Vec<u8> {
    let mut out = vec![b'I'];
    out.extend_from_slice(&7_u32.to_be_bytes());
    out.push(b'N');
    out.extend_from_slice(tuple);
    out
}

fn update(first_tag: u8, old: Option<&[u8]>, next_tag: Option<u8>, new: &[u8]) -> Vec<u8> {
    let mut out = vec![b'U'];
    out.extend_from_slice(&7_u32.to_be_bytes());
    out.push(first_tag);
    if let Some(old) = old {
        out.extend_from_slice(old);
    }
    if let Some(next_tag) = next_tag {
        out.push(next_tag);
    }
    if first_tag == b'N' || next_tag == Some(b'N') {
        out.extend_from_slice(new);
    }
    out
}

fn delete(tag: u8, old: &[u8]) -> Vec<u8> {
    let mut out = vec![b'D'];
    out.extend_from_slice(&7_u32.to_be_bytes());
    out.push(tag);
    out.extend_from_slice(old);
    out
}

#[test]
fn tuple_encoding_preserves_every_kind_and_enforces_bounds() {
    let encoded = TupleData {
        columns: vec![
            TupleColumn::Null,
            TupleColumn::UnchangedToast,
            TupleColumn::Text(b"hello".to_vec()),
            TupleColumn::Binary(vec![0, 1, 2]),
        ],
    }
    .encode()
    .unwrap_or_else(|_| unreachable!());
    assert_eq!(&encoded[..2], &4_u16.to_le_bytes());
    assert!(encoded.contains(&b'n'));
    assert!(encoded.contains(&b'u'));
    assert!(encoded.contains(&b't'));
    assert!(encoded.contains(&b'b'));

    assert_eq!(
        TupleData {
            columns: vec![TupleColumn::Null; 65_536]
        }
        .encode(),
        Err(PgOutputError::TooManyColumns(65_536))
    );
    assert_eq!(
        TupleData {
            columns: vec![TupleColumn::Null; 1_025]
        }
        .encode(),
        Err(PgOutputError::TooManyColumns(1_025))
    );
    assert_eq!(
        TupleData {
            columns: vec![TupleColumn::Text(vec![0; MAX_COLUMN_BYTES + 1])]
        }
        .encode(),
        Err(PgOutputError::ColumnTooLarge(MAX_COLUMN_BYTES + 1))
    );
}

#[test]
fn relation_decoding_covers_all_identities_and_column_metadata() {
    for (raw, expected) in [
        (b'd', ReplicaIdentity::Default),
        (b'n', ReplicaIdentity::Nothing),
        (b'f', ReplicaIdentity::Full),
        (b'i', ReplicaIdentity::Index),
    ] {
        let mut bytes = relation(raw, b"public", b"inventory", 1);
        bytes.push(1);
        bytes.extend_from_slice(b"id\0");
        bytes.extend_from_slice(&23_u32.to_be_bytes());
        bytes.extend_from_slice(&(-1_i32).to_be_bytes());
        let decoded = PgOutputDecoder::decode(&bytes).unwrap_or_else(|_| unreachable!());
        match decoded {
            PgOutputMessage::Relation(metadata) => {
                assert_eq!(metadata.relation_id, 7);
                assert_eq!(metadata.namespace, "public");
                assert_eq!(metadata.name, "inventory");
                assert_eq!(metadata.replica_identity, expected);
                assert_eq!(metadata.columns.len(), 1);
                assert_eq!(metadata.columns[0].flags, 1);
                assert_eq!(metadata.columns[0].name, "id");
                assert_eq!(metadata.columns[0].type_oid, 23);
                assert_eq!(metadata.columns[0].type_modifier, -1);
            }
            _ => unreachable!(),
        }
    }

    assert_eq!(
        PgOutputDecoder::decode(&relation(b'x', b"public", b"x", 0)),
        Err(PgOutputError::InvalidReplicaIdentity(b'x'))
    );
    assert_eq!(
        PgOutputDecoder::decode(&relation(b'd', b"public", b"x", 1_025)),
        Err(PgOutputError::TooManyColumns(1_025))
    );
}

#[test]
fn row_change_decoding_covers_insert_update_and_delete_shapes() {
    let all_columns = tuple(&[(b'n', b""), (b'u', b""), (b't', b"x"), (b'b', &[1, 2])]);
    let inserted = PgOutputDecoder::decode(&insert_with_tuple(&all_columns))
        .unwrap_or_else(|_| unreachable!());
    assert!(matches!(
        inserted,
        PgOutputMessage::Change(change)
            if change.kind() == ChangeKind::Insert
                && change.old_tuple().is_none()
                && change.new_tuple().is_some()
    ));

    for old_tag in [b'K', b'O'] {
        let decoded = PgOutputDecoder::decode(&update(
            old_tag,
            Some(&tuple(&[(b't', b"old")])),
            Some(b'N'),
            &tuple(&[(b't', b"new")]),
        ))
        .unwrap_or_else(|_| unreachable!());
        assert!(matches!(
            decoded,
            PgOutputMessage::Change(change)
                if change.kind() == ChangeKind::Update
                    && change.old_tuple().is_some()
                    && change.new_tuple().is_some()
        ));
    }
    let decoded = PgOutputDecoder::decode(&update(
        b'N',
        None,
        None,
        &tuple(&[(b't', b"new")]),
    ))
    .unwrap_or_else(|_| unreachable!());
    assert!(matches!(
        decoded,
        PgOutputMessage::Change(change)
            if change.kind() == ChangeKind::Update && change.old_tuple().is_none()
    ));

    for old_tag in [b'K', b'O'] {
        let decoded = PgOutputDecoder::decode(&delete(old_tag, &tuple(&[(b'b', &[9])])))
            .unwrap_or_else(|_| unreachable!());
        assert!(matches!(
            decoded,
            PgOutputMessage::Change(change)
                if change.kind() == ChangeKind::Delete
                    && change.old_tuple().is_some()
                    && change.new_tuple().is_none()
        ));
    }

    assert_eq!(
        PgOutputDecoder::decode(&update(b'X', None, None, &[])),
        Err(PgOutputError::InvalidTupleTag(b'X'))
    );
    assert_eq!(
        PgOutputDecoder::decode(&update(
            b'K',
            Some(&tuple(&[])),
            Some(b'X'),
            &[]
        )),
        Err(PgOutputError::InvalidTupleTag(b'X'))
    );
    assert_eq!(
        PgOutputDecoder::decode(&delete(b'N', &tuple(&[]))),
        Err(PgOutputError::InvalidTupleTag(b'N'))
    );
}

#[test]
fn truncate_and_message_boundaries_fail_closed() {
    let mut truncate = vec![b'T'];
    truncate.extend_from_slice(&2_u32.to_be_bytes());
    truncate.push(3);
    truncate.extend_from_slice(&7_u32.to_be_bytes());
    truncate.extend_from_slice(&8_u32.to_be_bytes());
    assert_eq!(
        PgOutputDecoder::decode(&truncate),
        Ok(PgOutputMessage::Truncate {
            relation_ids: vec![7, 8],
            options: 3
        })
    );

    for count in [0_u32, 4_097] {
        let mut invalid = vec![b'T'];
        invalid.extend_from_slice(&count.to_be_bytes());
        invalid.push(0);
        assert_eq!(
            PgOutputDecoder::decode(&invalid),
            Err(PgOutputError::InvalidTruncateCount(
                usize::try_from(count).unwrap_or(usize::MAX)
            ))
        );
    }
    assert_eq!(PgOutputDecoder::decode(&[]), Err(PgOutputError::UnexpectedEof));
    assert_eq!(
        PgOutputDecoder::decode(&[0xff]),
        Err(PgOutputError::UnsupportedMessage(0xff))
    );

    let mut begin = vec![b'B'];
    begin.extend_from_slice(&1_u64.to_be_bytes());
    begin.extend_from_slice(&2_i64.to_be_bytes());
    begin.extend_from_slice(&3_u32.to_be_bytes());
    begin.push(0);
    assert_eq!(
        PgOutputDecoder::decode(&begin),
        Err(PgOutputError::TrailingBytes(1))
    );
}

#[test]
fn malformed_strings_tuples_and_lengths_are_rejected() {
    let mut unterminated = vec![b'R'];
    unterminated.extend_from_slice(&7_u32.to_be_bytes());
    unterminated.extend_from_slice(b"public");
    assert_eq!(
        PgOutputDecoder::decode(&unterminated),
        Err(PgOutputError::UnterminatedCString)
    );

    let too_long_name = vec![b'a'; 1_025];
    assert_eq!(
        PgOutputDecoder::decode(&relation(b'd', &too_long_name, b"x", 0)),
        Err(PgOutputError::StringTooLong(1_025))
    );
    assert_eq!(
        PgOutputDecoder::decode(&relation(b'd', &[0xff], b"x", 0)),
        Err(PgOutputError::InvalidUtf8)
    );

    let mut too_many_tuple_columns = vec![b'I'];
    too_many_tuple_columns.extend_from_slice(&7_u32.to_be_bytes());
    too_many_tuple_columns.push(b'N');
    too_many_tuple_columns.extend_from_slice(&1_025_u16.to_be_bytes());
    assert_eq!(
        PgOutputDecoder::decode(&too_many_tuple_columns),
        Err(PgOutputError::TooManyColumns(1_025))
    );

    let mut invalid_kind = insert_with_tuple(&tuple(&[(b'x', b"")]));
    assert_eq!(
        PgOutputDecoder::decode(&invalid_kind),
        Err(PgOutputError::InvalidTupleColumnKind(b'x'))
    );
    invalid_kind.clear();

    let mut oversized = vec![b'I'];
    oversized.extend_from_slice(&7_u32.to_be_bytes());
    oversized.push(b'N');
    oversized.extend_from_slice(&1_u16.to_be_bytes());
    oversized.push(b't');
    oversized.extend_from_slice(&(u32::try_from(MAX_COLUMN_BYTES).unwrap_or_default() + 1).to_be_bytes());
    assert_eq!(
        PgOutputDecoder::decode(&oversized),
        Err(PgOutputError::ColumnTooLarge(MAX_COLUMN_BYTES + 1))
    );

    for truncated in [
        vec![b'B'],
        vec![b'C'],
        vec![b'R'],
        vec![b'I'],
        vec![b'U'],
        vec![b'D'],
        vec![b'T'],
    ] {
        assert_eq!(
            PgOutputDecoder::decode(&truncated),
            Err(PgOutputError::UnexpectedEof)
        );
    }
}

#[test]
fn every_public_error_has_a_stable_display_surface() {
    for error in [
        PgOutputError::UnexpectedEof,
        PgOutputError::LengthOverflow,
        PgOutputError::UnsupportedMessage(1),
        PgOutputError::InvalidReplicaIdentity(2),
        PgOutputError::InvalidTupleTag(3),
        PgOutputError::InvalidTupleColumnKind(4),
        PgOutputError::TooManyColumns(5),
        PgOutputError::ColumnTooLarge(6),
        PgOutputError::StringTooLong(7),
        PgOutputError::UnterminatedCString,
        PgOutputError::InvalidUtf8,
        PgOutputError::InvalidTruncateCount(8),
        PgOutputError::TrailingBytes(9),
    ] {
        assert!(!error.to_string().is_empty());
    }
}
