use veyra_cdc::{
    ChangeKind, ColumnMetadata, PgOutputMessage, RelationMetadata, ReplicaIdentity, RowChange,
    StreamError, TransactionBuildError, TransactionStream,
};
use veyra_types::LogSequenceNumber;

fn relation(id: u32) -> RelationMetadata {
    RelationMetadata {
        relation_id: id,
        namespace: "public".to_owned(),
        name: format!("room_{id}"),
        replica_identity: ReplicaIdentity::Default,
        columns: Vec::<ColumnMetadata>::new(),
    }
}

fn begin(xid: u32) -> PgOutputMessage {
    PgOutputMessage::Begin {
        final_lsn: LogSequenceNumber::new(10),
        commit_timestamp_micros: 0,
        xid,
    }
}

#[test]
fn every_transaction_stream_propagation_boundary_fails_closed() {
    let mut nested = TransactionStream::new();
    assert_eq!(nested.consume(begin(1)), Ok(None));
    assert_eq!(
        nested.consume(begin(2)),
        Err(StreamError::Transaction(
            TransactionBuildError::NestedTransaction
        ))
    );

    let mut change = TransactionStream::new();
    assert_eq!(
        change.consume(PgOutputMessage::Relation(relation(7))),
        Ok(None)
    );
    assert_eq!(
        change.consume(PgOutputMessage::Change(RowChange::new(
            7,
            ChangeKind::Insert,
            None,
            Some(vec![1]),
        ))),
        Err(StreamError::Transaction(
            TransactionBuildError::ChangeOutsideTransaction
        ))
    );

    let mut truncate_unknown = TransactionStream::new();
    assert_eq!(
        truncate_unknown.consume(PgOutputMessage::Relation(relation(1))),
        Ok(None)
    );
    assert_eq!(truncate_unknown.consume(begin(3)), Ok(None));
    assert_eq!(
        truncate_unknown.consume(PgOutputMessage::Truncate {
            relation_ids: vec![1, 99],
            options: 0,
        }),
        Err(StreamError::UnknownRelation(99))
    );

    let mut truncate_outside = TransactionStream::new();
    assert_eq!(
        truncate_outside.consume(PgOutputMessage::Relation(relation(1))),
        Ok(None)
    );
    assert_eq!(
        truncate_outside.consume(PgOutputMessage::Truncate {
            relation_ids: vec![1],
            options: 0,
        }),
        Err(StreamError::Transaction(
            TransactionBuildError::ChangeOutsideTransaction
        ))
    );
}
