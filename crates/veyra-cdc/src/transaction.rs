use core::fmt;

use serde::{Deserialize, Serialize};
use veyra_types::LogSequenceNumber;

/// Logical row operation projected from `pgoutput`.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum ChangeKind {
    Insert,
    Update,
    Delete,
    Truncate,
}

/// One relation-scoped logical change. Tuple bytes remain PostgreSQL-typed data and are decoded
/// by the schema/projection layer after relation metadata validation.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RowChange {
    pub relation_id: u32,
    pub kind: ChangeKind,
    pub old_tuple: Option<Vec<u8>>,
    pub new_tuple: Option<Vec<u8>>,
}

impl RowChange {
    #[must_use]
    pub fn new(
        relation_id: u32,
        kind: ChangeKind,
        old_tuple: Option<Vec<u8>>,
        new_tuple: Option<Vec<u8>>,
    ) -> Self {
        Self {
            relation_id,
            kind,
            old_tuple,
            new_tuple,
        }
    }
}

/// A complete PostgreSQL transaction. Readers may only observe a batch after its commit has been
/// durably recorded and fully applied.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TransactionBatch {
    xid: u32,
    begin_lsn: LogSequenceNumber,
    commit_lsn: LogSequenceNumber,
    end_lsn: LogSequenceNumber,
    changes: Vec<RowChange>,
}

impl TransactionBatch {
    pub fn try_new(
        xid: u32,
        begin_lsn: LogSequenceNumber,
        commit_lsn: LogSequenceNumber,
        end_lsn: LogSequenceNumber,
        changes: Vec<RowChange>,
    ) -> Result<Self, TransactionValidationError> {
        if commit_lsn < begin_lsn {
            return Err(TransactionValidationError::CommitBeforeBegin);
        }
        if end_lsn < commit_lsn {
            return Err(TransactionValidationError::EndBeforeCommit);
        }
        Ok(Self {
            xid,
            begin_lsn,
            commit_lsn,
            end_lsn,
            changes,
        })
    }

    #[must_use]
    pub const fn xid(&self) -> u32 {
        self.xid
    }

    #[must_use]
    pub const fn begin_lsn(&self) -> LogSequenceNumber {
        self.begin_lsn
    }

    #[must_use]
    pub const fn commit_lsn(&self) -> LogSequenceNumber {
        self.commit_lsn
    }

    #[must_use]
    pub const fn end_lsn(&self) -> LogSequenceNumber {
        self.end_lsn
    }

    #[must_use]
    pub fn changes(&self) -> &[RowChange] {
        &self.changes
    }

    /// Stable non-cryptographic fingerprint used only to detect conflicting duplicate WAL replay.
    #[must_use]
    pub fn fingerprint(&self) -> u64 {
        let mut hash = 0xcbf2_9ce4_8422_2325_u64;
        fn feed(hash: &mut u64, bytes: &[u8]) {
            for byte in bytes {
                *hash ^= u64::from(*byte);
                *hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
            }
        }
        feed(&mut hash, &self.xid.to_le_bytes());
        feed(&mut hash, &self.begin_lsn.get().to_le_bytes());
        feed(&mut hash, &self.commit_lsn.get().to_le_bytes());
        feed(&mut hash, &self.end_lsn.get().to_le_bytes());
        for change in &self.changes {
            feed(&mut hash, &change.relation_id.to_le_bytes());
            feed(&mut hash, &[change.kind as u8]);
            for tuple in [&change.old_tuple, &change.new_tuple] {
                match tuple {
                    Some(bytes) => {
                        feed(&mut hash, &[1]);
                        feed(&mut hash, &(bytes.len() as u64).to_le_bytes());
                        feed(&mut hash, bytes);
                    }
                    None => feed(&mut hash, &[0]),
                }
            }
        }
        hash
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TransactionValidationError {
    CommitBeforeBegin,
    EndBeforeCommit,
}

impl fmt::Display for TransactionValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::CommitBeforeBegin => "commit_lsn is before begin_lsn",
            Self::EndBeforeCommit => "end_lsn is before commit_lsn",
        })
    }
}

impl std::error::Error for TransactionValidationError {}

#[derive(Clone, Debug)]
struct OpenTransaction {
    xid: u32,
    begin_lsn: LogSequenceNumber,
    changes: Vec<RowChange>,
}

/// Single-writer transaction assembler.
#[derive(Clone, Debug, Default)]
pub struct TransactionBuilder {
    open: Option<OpenTransaction>,
    last_commit_lsn: LogSequenceNumber,
}

impl TransactionBuilder {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            open: None,
            last_commit_lsn: LogSequenceNumber::ZERO,
        }
    }

    pub fn begin(
        &mut self,
        xid: u32,
        begin_lsn: LogSequenceNumber,
    ) -> Result<(), TransactionBuildError> {
        if self.open.is_some() {
            return Err(TransactionBuildError::NestedTransaction);
        }
        self.open = Some(OpenTransaction {
            xid,
            begin_lsn,
            changes: Vec::new(),
        });
        Ok(())
    }

    pub fn push(&mut self, change: RowChange) -> Result<(), TransactionBuildError> {
        let open = self
            .open
            .as_mut()
            .ok_or(TransactionBuildError::ChangeOutsideTransaction)?;
        open.changes.push(change);
        Ok(())
    }

    pub fn commit(
        &mut self,
        commit_lsn: LogSequenceNumber,
        end_lsn: LogSequenceNumber,
    ) -> Result<TransactionBatch, TransactionBuildError> {
        let open = self
            .open
            .take()
            .ok_or(TransactionBuildError::CommitWithoutBegin)?;
        if commit_lsn < self.last_commit_lsn {
            self.open = Some(open);
            return Err(TransactionBuildError::CommitLsnRegressed);
        }
        let batch = TransactionBatch::try_new(
            open.xid,
            open.begin_lsn,
            commit_lsn,
            end_lsn,
            open.changes,
        )
        .map_err(TransactionBuildError::InvalidTransaction)?;
        self.last_commit_lsn = commit_lsn;
        Ok(batch)
    }

    #[must_use]
    pub const fn is_open(&self) -> bool {
        self.open.is_some()
    }

    #[must_use]
    pub const fn last_commit_lsn(&self) -> LogSequenceNumber {
        self.last_commit_lsn
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TransactionBuildError {
    NestedTransaction,
    ChangeOutsideTransaction,
    CommitWithoutBegin,
    CommitLsnRegressed,
    InvalidTransaction(TransactionValidationError),
}

impl fmt::Display for TransactionBuildError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NestedTransaction => formatter.write_str("nested transaction begin"),
            Self::ChangeOutsideTransaction => formatter.write_str("row change outside transaction"),
            Self::CommitWithoutBegin => formatter.write_str("commit without begin"),
            Self::CommitLsnRegressed => formatter.write_str("commit LSN regressed"),
            Self::InvalidTransaction(error) => write!(formatter, "invalid transaction: {error}"),
        }
    }
}

impl std::error::Error for TransactionBuildError {}

#[cfg(test)]
mod tests {
    use super::*;

    fn lsn(value: u64) -> LogSequenceNumber {
        LogSequenceNumber::new(value)
    }

    fn change(value: u8) -> RowChange {
        RowChange::new(7, ChangeKind::Insert, None, Some(vec![value]))
    }

    #[test]
    fn batch_validates_lsn_order_and_fingerprint() {
        let batch = TransactionBatch::try_new(9, lsn(10), lsn(20), lsn(21), vec![change(1)]);
        assert!(batch.is_ok());
        let batch = batch.unwrap_or_else(|_| unreachable!());
        assert_eq!(batch.xid(), 9);
        assert_eq!(batch.begin_lsn().get(), 10);
        assert_eq!(batch.commit_lsn().get(), 20);
        assert_eq!(batch.end_lsn().get(), 21);
        assert_eq!(batch.changes().len(), 1);
        assert_eq!(batch.fingerprint(), batch.clone().fingerprint());
        let different = TransactionBatch::try_new(9, lsn(10), lsn(20), lsn(21), vec![change(2)])
            .unwrap_or_else(|_| unreachable!());
        assert_ne!(batch.fingerprint(), different.fingerprint());
    }

    #[test]
    fn batch_rejects_invalid_lsn_order() {
        assert_eq!(
            TransactionBatch::try_new(1, lsn(2), lsn(1), lsn(1), Vec::new()),
            Err(TransactionValidationError::CommitBeforeBegin)
        );
        assert_eq!(
            TransactionBatch::try_new(1, lsn(1), lsn(3), lsn(2), Vec::new()),
            Err(TransactionValidationError::EndBeforeCommit)
        );
    }

    #[test]
    fn builder_preserves_transaction_boundaries() {
        let mut builder = TransactionBuilder::new();
        assert!(!builder.is_open());
        builder.begin(4, lsn(10)).unwrap_or_else(|_| unreachable!());
        assert!(builder.is_open());
        builder.push(change(1)).unwrap_or_else(|_| unreachable!());
        builder.push(change(2)).unwrap_or_else(|_| unreachable!());
        let batch = builder.commit(lsn(20), lsn(21)).unwrap_or_else(|_| unreachable!());
        assert_eq!(batch.changes().len(), 2);
        assert_eq!(builder.last_commit_lsn().get(), 20);
        assert!(!builder.is_open());
    }

    #[test]
    fn builder_fails_closed_on_invalid_sequence() {
        let mut builder = TransactionBuilder::new();
        assert_eq!(builder.push(change(1)), Err(TransactionBuildError::ChangeOutsideTransaction));
        assert_eq!(
            builder.commit(lsn(1), lsn(1)),
            Err(TransactionBuildError::CommitWithoutBegin)
        );
        builder.begin(1, lsn(10)).unwrap_or_else(|_| unreachable!());
        assert_eq!(builder.begin(2, lsn(11)), Err(TransactionBuildError::NestedTransaction));
        let invalid = builder.commit(lsn(9), lsn(9));
        assert_eq!(
            invalid,
            Err(TransactionBuildError::InvalidTransaction(
                TransactionValidationError::CommitBeforeBegin
            ))
        );
    }

    #[test]
    fn builder_rejects_commit_regression_without_losing_open_transaction() {
        let mut builder = TransactionBuilder::new();
        builder.begin(1, lsn(1)).unwrap_or_else(|_| unreachable!());
        let _first = builder.commit(lsn(5), lsn(5)).unwrap_or_else(|_| unreachable!());
        builder.begin(2, lsn(2)).unwrap_or_else(|_| unreachable!());
        assert_eq!(
            builder.commit(lsn(4), lsn(4)),
            Err(TransactionBuildError::CommitLsnRegressed)
        );
        assert!(builder.is_open());
    }

    #[test]
    fn error_messages_are_stable() {
        assert_eq!(TransactionValidationError::CommitBeforeBegin.to_string(), "commit_lsn is before begin_lsn");
        assert_eq!(TransactionValidationError::EndBeforeCommit.to_string(), "end_lsn is before commit_lsn");
        assert_eq!(TransactionBuildError::NestedTransaction.to_string(), "nested transaction begin");
        assert_eq!(TransactionBuildError::ChangeOutsideTransaction.to_string(), "row change outside transaction");
        assert_eq!(TransactionBuildError::CommitWithoutBegin.to_string(), "commit without begin");
        assert_eq!(TransactionBuildError::CommitLsnRegressed.to_string(), "commit LSN regressed");
        assert_eq!(
            TransactionBuildError::InvalidTransaction(TransactionValidationError::EndBeforeCommit).to_string(),
            "invalid transaction: end_lsn is before commit_lsn"
        );
    }
}
