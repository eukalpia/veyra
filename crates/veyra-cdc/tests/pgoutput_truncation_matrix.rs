use veyra_cdc::{PgOutputDecoder, PgOutputMessage};

fn tuple() -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&4_u16.to_be_bytes());
    bytes.push(b'n');
    bytes.push(b'u');
    bytes.push(b't');
    bytes.extend_from_slice(&3_u32.to_be_bytes());
    bytes.extend_from_slice(b"abc");
    bytes.push(b'b');
    bytes.extend_from_slice(&2_u32.to_be_bytes());
    bytes.extend_from_slice(&[1, 2]);
    bytes
}

fn begin() -> Vec<u8> {
    let mut bytes = vec![b'B'];
    bytes.extend_from_slice(&10_u64.to_be_bytes());
    bytes.extend_from_slice(&20_i64.to_be_bytes());
    bytes.extend_from_slice(&30_u32.to_be_bytes());
    bytes
}

fn commit() -> Vec<u8> {
    let mut bytes = vec![b'C', 0];
    bytes.extend_from_slice(&40_u64.to_be_bytes());
    bytes.extend_from_slice(&41_u64.to_be_bytes());
    bytes.extend_from_slice(&50_i64.to_be_bytes());
    bytes
}

fn relation() -> Vec<u8> {
    let mut bytes = vec![b'R'];
    bytes.extend_from_slice(&7_u32.to_be_bytes());
    bytes.extend_from_slice(b"public\0inventory\0");
    bytes.push(b'f');
    bytes.extend_from_slice(&2_u16.to_be_bytes());
    for (flags, name, oid, modifier) in [
        (1_u8, b"id".as_slice(), 23_u32, -1_i32),
        (0_u8, b"available".as_slice(), 16_u32, -1_i32),
    ] {
        bytes.push(flags);
        bytes.extend_from_slice(name);
        bytes.push(0);
        bytes.extend_from_slice(&oid.to_be_bytes());
        bytes.extend_from_slice(&modifier.to_be_bytes());
    }
    bytes
}

fn insert() -> Vec<u8> {
    let mut bytes = vec![b'I'];
    bytes.extend_from_slice(&7_u32.to_be_bytes());
    bytes.push(b'N');
    bytes.extend_from_slice(&tuple());
    bytes
}

fn update() -> Vec<u8> {
    let mut bytes = vec![b'U'];
    bytes.extend_from_slice(&7_u32.to_be_bytes());
    bytes.push(b'O');
    bytes.extend_from_slice(&tuple());
    bytes.push(b'N');
    bytes.extend_from_slice(&tuple());
    bytes
}

fn delete() -> Vec<u8> {
    let mut bytes = vec![b'D'];
    bytes.extend_from_slice(&7_u32.to_be_bytes());
    bytes.push(b'K');
    bytes.extend_from_slice(&tuple());
    bytes
}

fn truncate() -> Vec<u8> {
    let mut bytes = vec![b'T'];
    bytes.extend_from_slice(&3_u32.to_be_bytes());
    bytes.push(3);
    bytes.extend_from_slice(&7_u32.to_be_bytes());
    bytes.extend_from_slice(&8_u32.to_be_bytes());
    bytes.extend_from_slice(&9_u32.to_be_bytes());
    bytes
}

#[test]
fn every_strict_prefix_of_every_supported_message_fails_closed() {
    let messages = [
        begin(),
        commit(),
        relation(),
        insert(),
        update(),
        delete(),
        truncate(),
    ];

    for message in messages {
        assert!(PgOutputDecoder::decode(&message).is_ok());
        for end in 0..message.len() {
            assert!(
                PgOutputDecoder::decode(&message[..end]).is_err(),
                "prefix {end}/{} unexpectedly decoded for tag {:?}",
                message.len(),
                message.first()
            );
        }
    }
}

#[test]
fn complete_messages_decode_to_the_expected_protocol_family() {
    assert!(matches!(
        PgOutputDecoder::decode(&begin()),
        Ok(PgOutputMessage::Begin { .. })
    ));
    assert!(matches!(
        PgOutputDecoder::decode(&commit()),
        Ok(PgOutputMessage::Commit { .. })
    ));
    assert!(matches!(
        PgOutputDecoder::decode(&relation()),
        Ok(PgOutputMessage::Relation(_))
    ));
    assert!(matches!(
        PgOutputDecoder::decode(&insert()),
        Ok(PgOutputMessage::Change(_))
    ));
    assert!(matches!(
        PgOutputDecoder::decode(&update()),
        Ok(PgOutputMessage::Change(_))
    ));
    assert!(matches!(
        PgOutputDecoder::decode(&delete()),
        Ok(PgOutputMessage::Change(_))
    ));
    assert!(matches!(
        PgOutputDecoder::decode(&truncate()),
        Ok(PgOutputMessage::Truncate { .. })
    ));
}
