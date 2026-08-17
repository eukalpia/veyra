use core::fmt;
use std::collections::BTreeMap;

use crate::{
    ChangeKind, PgOutputMessage, RelationMetadata, RowChange, TransactionBatch,
    TransactionBuildError, TransactionBuilder,
};

/// Stateful `pgoutput` transaction stream. It emits only fully committed transactions.
#[derive(Clone, Debug, Default)]
pub struct TransactionStream {
    builder: TransactionBuilder,
    relations: BTreeMap<u32, RelationMetadata>,
}

impl TransactionStream {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn consume(
        &mut self,
        message: PgOutputMessage,
    ) -> Result<Option<TransactionBatch>, StreamError> {
        match message {
            PgOutputMessage::Begin { final_lsn, xid, .. } => {
                self.begin_transaction(xid, final_lsn)?;
                Ok(None)
            }
            PgOutputMessage::Commit {
                commit_lsn,
                end_lsn,
                ..
            } => self.commit_transaction(commit_lsn, end_lsn).map(Some),
            PgOutputMessage::Relation(metadata) => {
                self.relations.insert(metadata.relation_id, metadata);
                Ok(None)
            }
            PgOutputMessage::Change(change) => {
                self.require_relation(change.relation_id)?;
                self.builder.push(change)?;
                Ok(None)
            }
            PgOutputMessage::Truncate { relation_ids, .. } => {
                for relation_id in relation_ids {
                    self.require_relation(relation_id)?;
                    self.builder.push(RowChange::new(
                        relation_id,
                        ChangeKind::Truncate,
                        None,
                        None,
                    ))?;
                }
                Ok(None)
            }
        }
    }

    pub(crate) fn begin_transaction(
        &mut self,
        xid: u32,
        final_lsn: veyra_types::LogSequenceNumber,
    ) -> Result<(), StreamError> {
        self.builder
            .begin(xid, final_lsn)
            .map_err(StreamError::from)
    }

    pub(crate) fn commit_transaction(
        &mut self,
        commit_lsn: veyra_types::LogSequenceNumber,
        end_lsn: veyra_types::LogSequenceNumber,
    ) -> Result<TransactionBatch, StreamError> {
        self.builder
            .commit(commit_lsn, end_lsn)
            .map_err(StreamError::from)
    }

    #[must_use]
    pub fn relation(&self, relation_id: u32) -> Option<&RelationMetadata> {
        self.relations.get(&relation_id)
    }

    #[must_use]
    pub const fn transaction_open(&self) -> bool {
        self.builder.is_open()
    }

    fn require_relation(&self, relation_id: u32) -> Result<(), StreamError> {
        if self.relations.contains_key(&relation_id) {
            Ok(())
        } else {
            Err(StreamError::UnknownRelation(relation_id))
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StreamError {
    Transaction(TransactionBuildError),
    UnknownRelation(u32),
}

impl fmt::Display for StreamError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Transaction(error) => write!(formatter, "transaction stream error: {error}"),
            Self::UnknownRelation(id) => write!(formatter, "unknown relation {id}"),
        }
    }
}
impl std::error::Error for StreamError {}
impl From<TransactionBuildError> for StreamError {
    fn from(value: TransactionBuildError) -> Self {
        Self::Transaction(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use veyra_types::LogSequenceNumber;

    fn relation(id: u32) -> RelationMetadata {
        RelationMetadata {
            relation_id: id,
            namespace: "public".to_owned(),
            name: "inventory".to_owned(),
            replica_identity: crate::ReplicaIdentity::Default,
            columns: Vec::new(),
        }
    }

    fn begin(xid: u32, lsn: u64) -> PgOutputMessage {
        PgOutputMessage::Begin {
            final_lsn: LogSequenceNumber::new(lsn),
            commit_timestamp_micros: 0,
            xid,
        }
    }

    fn commit(lsn: u64) -> PgOutputMessage {
        PgOutputMessage::Commit {
            flags: 0,
            commit_lsn: LogSequenceNumber::new(lsn),
            end_lsn: LogSequenceNumber::new(lsn + 1),
            commit_timestamp_micros: 0,
        }
    }

    #[test]
    fn emits_nothing_until_commit() {
        let mut stream = TransactionStream::new();
        assert_eq!(
            stream.consume(PgOutputMessage::Relation(relation(7))),
            Ok(None)
        );
        assert!(stream.relation(7).is_some());
        assert_eq!(stream.consume(begin(9, 20)), Ok(None));
        assert!(stream.transaction_open());
        assert_eq!(
            stream.consume(PgOutputMessage::Change(RowChange::new(
                7,
                ChangeKind::Insert,
                None,
                Some(vec![1]),
            ))),
            Ok(None)
        );
        let batch = stream
            .consume(commit(20))
            .unwrap_or_else(|_| unreachable!());
        let batch = batch.unwrap_or_else(|| unreachable!());
        assert_eq!(batch.xid(), 9);
        assert_eq!(batch.final_lsn().get(), 20);
        assert_eq!(batch.changes().len(), 1);
        assert!(!stream.transaction_open());
    }

    #[test]
    fn unknown_relation_fails_closed() {
        let mut stream = TransactionStream::new();
        stream
            .consume(begin(1, 5))
            .unwrap_or_else(|_| unreachable!());
        assert_eq!(
            stream.consume(PgOutputMessage::Change(RowChange::new(
                99,
                ChangeKind::Delete,
                Some(vec![1]),
                None,
            ))),
            Err(StreamError::UnknownRelation(99))
        );
        assert!(stream.transaction_open());
    }

    #[test]
    fn truncate_expands_all_relations_inside_one_transaction() {
        let mut stream = TransactionStream::new();
        stream
            .consume(PgOutputMessage::Relation(relation(1)))
            .unwrap_or_else(|_| unreachable!());
        stream
            .consume(PgOutputMessage::Relation(relation(2)))
            .unwrap_or_else(|_| unreachable!());
        stream
            .consume(begin(3, 10))
            .unwrap_or_else(|_| unreachable!());
        stream
            .consume(PgOutputMessage::Truncate {
                relation_ids: vec![1, 2],
                options: 3,
            })
            .unwrap_or_else(|_| unreachable!());
        let batch = stream
            .consume(commit(10))
            .unwrap_or_else(|_| unreachable!())
            .unwrap_or_else(|| unreachable!());
        assert_eq!(batch.changes().len(), 2);
        assert!(
            batch
                .changes()
                .iter()
                .all(|change| change.kind == ChangeKind::Truncate)
        );
    }

    #[test]
    fn sequence_errors_are_propagated() {
        let mut stream = TransactionStream::new();
        assert_eq!(
            stream.consume(commit(1)),
            Err(StreamError::Transaction(
                TransactionBuildError::CommitWithoutBegin
            ))
        );
    }

    #[test]
    fn error_messages_are_stable() {
        assert_eq!(
            StreamError::UnknownRelation(4).to_string(),
            "unknown relation 4"
        );
        assert_eq!(
            StreamError::Transaction(TransactionBuildError::CommitWithoutBegin).to_string(),
            "transaction stream error: commit without begin"
        );
    }
}
