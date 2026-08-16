use core::fmt;

use serde::{Deserialize, Serialize};
use veyra_types::LogSequenceNumber;

/// Logical row operation projected from `pgoutput`.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[repr(u8)]
pub enum ChangeKind {
    Insert = 0,
    Update = 1,
    Delete = 2,
    Truncate = 3,
}

/// One relation-scoped logical change.
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

/// A complete `PostgreSQL` transaction. It is the smallest CDC durability/apply unit.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TransactionBatch {
    xid: u32,
    final_lsn: LogSequenceNumber,
    commit_lsn: LogSequenceNumber,
    end_lsn: LogSequenceNumber,
    changes: Vec<RowChange>,
}

impl TransactionBatch {
    pub fn try_new(
        xid: u32,
        final_lsn: LogSequenceNumber,
        commit_lsn: LogSequenceNumber,
        end_lsn: LogSequenceNumber,
        changes: Vec<RowChange>,
    ) -> Result<Self, TransactionValidationError> {
        if end_lsn < commit_lsn {
            return Err(TransactionValidationError::EndBeforeCommit);
        }
        Ok(Self {
            xid,
            final_lsn,
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
    pub const fn final_lsn(&self) -> LogSequenceNumber {
        self.final_lsn
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

    /// Stable non-cryptographic fingerprint used only for duplicate-WAL conflict detection.
    #[must_use]
    pub fn fingerprint(&self) -> u64 {
        let mut hash = 0xcbf2_9ce4_8422_2325_u64;
        feed_fingerprint(&mut hash, &self.xid.to_le_bytes());
        feed_fingerprint(&mut hash, &self.final_lsn.get().to_le_bytes());
        feed_fingerprint(&mut hash, &self.commit_lsn.get().to_le_bytes());
        feed_fingerprint(&mut hash, &self.end_lsn.get().to_le_bytes());
        for change in &self.changes {
            feed_fingerprint(&mut hash, &change.relation_id.to_le_bytes());
            feed_fingerprint(&mut hash, &[change.kind as u8]);
            for tuple in [change.old_tuple.as_deref(), change.new_tuple.as_deref()] {
                match tuple {
                    Some(bytes) => {
                        feed_fingerprint(&mut hash, &[1]);
                        feed_fingerprint(&mut hash, &usize_to_u64(bytes.len()).to_le_bytes());
                        feed_fingerprint(&mut hash, bytes);
                    }
                    None => feed_fingerprint(&mut hash, &[0]),
                }
            }
        }
        hash
    }
}

fn feed_fingerprint(hash: &mut u64, bytes: &[u8]) {
    for byte in bytes {
        *hash ^= u64::from(*byte);
        *hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
}

fn usize_to_u64(value: usize) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TransactionValidationError {
    EndBeforeCommit,
}

impl fmt::Display for TransactionValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("end_lsn is before commit_lsn")
    }
}
impl std::error::Error for TransactionValidationError {}

#[derive(Clone, Debug)]
struct OpenTransaction {
    xid: u32,
    final_lsn: LogSequenceNumber,
    changes: Vec<RowChange>,
}

/// Single-writer transaction assembler that never exposes an open transaction.
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
        final_lsn: LogSequenceNumber,
    ) -> Result<(), TransactionBuildError> {
        if self.open.is_some() {
            return Err(TransactionBuildError::NestedTransaction);
        }
        self.open = Some(OpenTransaction {
            xid,
            final_lsn,
            changes: Vec::new(),
        });
        Ok(())
    }

    pub fn push(&mut self, change: RowChange) -> Result<(), TransactionBuildError> {
        self.open
            .as_mut()
            .ok_or(TransactionBuildError::ChangeOutsideTransaction)?
            .changes
            .push(change);
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
        let batch =
            TransactionBatch::try_new(open.xid, open.final_lsn, commit_lsn, end_lsn, open.changes)
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
    fn batch_contract_and_fingerprint_are_deterministic() {
        let batch = TransactionBatch::try_new(9, lsn(20), lsn(20), lsn(21), vec![change(1)])
            .unwrap_or_else(|_| unreachable!());
        assert_eq!(
            (
                batch.xid(),
                batch.final_lsn().get(),
                batch.commit_lsn().get(),
                batch.end_lsn().get()
            ),
            (9, 20, 20, 21)
        );
        assert_eq!(batch.changes().len(), 1);
        assert_eq!(batch.fingerprint(), batch.clone().fingerprint());
        let different = TransactionBatch::try_new(9, lsn(20), lsn(20), lsn(21), vec![change(2)])
            .unwrap_or_else(|_| unreachable!());
        assert_ne!(batch.fingerprint(), different.fingerprint());
    }

    #[test]
    fn batch_rejects_end_before_commit() {
        assert_eq!(
            TransactionBatch::try_new(1, lsn(3), lsn(3), lsn(2), Vec::new()),
            Err(TransactionValidationError::EndBeforeCommit)
        );
    }

    #[test]
    fn builder_preserves_boundaries_and_rejects_bad_sequences() {
        let mut builder = TransactionBuilder::new();
        assert_eq!(
            builder.push(change(1)),
            Err(TransactionBuildError::ChangeOutsideTransaction)
        );
        assert_eq!(
            builder.commit(lsn(1), lsn(1)),
            Err(TransactionBuildError::CommitWithoutBegin)
        );
        builder.begin(4, lsn(20)).unwrap_or_else(|_| unreachable!());
        assert!(builder.is_open());
        assert_eq!(
            builder.begin(5, lsn(21)),
            Err(TransactionBuildError::NestedTransaction)
        );
        builder.push(change(1)).unwrap_or_else(|_| unreachable!());
        let batch = builder
            .commit(lsn(20), lsn(21))
            .unwrap_or_else(|_| unreachable!());
        assert_eq!(batch.final_lsn(), lsn(20));
        assert_eq!(batch.changes().len(), 1);
        assert_eq!(builder.last_commit_lsn(), lsn(20));
        assert!(!builder.is_open());
    }

    #[test]
    fn commit_regression_keeps_transaction_open() {
        let mut builder = TransactionBuilder::new();
        builder.begin(1, lsn(5)).unwrap_or_else(|_| unreachable!());
        let _ = builder
            .commit(lsn(5), lsn(5))
            .unwrap_or_else(|_| unreachable!());
        builder.begin(2, lsn(4)).unwrap_or_else(|_| unreachable!());
        assert_eq!(
            builder.commit(lsn(4), lsn(4)),
            Err(TransactionBuildError::CommitLsnRegressed)
        );
        assert!(builder.is_open());
    }

    #[test]
    fn invalid_open_transaction_is_rejected() {
        let mut builder = TransactionBuilder::new();
        builder.begin(1, lsn(9)).unwrap_or_else(|_| unreachable!());
        assert_eq!(
            builder.commit(lsn(9), lsn(8)),
            Err(TransactionBuildError::InvalidTransaction(
                TransactionValidationError::EndBeforeCommit
            ))
        );
    }

    #[test]
    fn errors_have_stable_messages() {
        assert_eq!(
            TransactionValidationError::EndBeforeCommit.to_string(),
            "end_lsn is before commit_lsn"
        );
        assert_eq!(
            TransactionBuildError::NestedTransaction.to_string(),
            "nested transaction begin"
        );
        assert_eq!(
            TransactionBuildError::ChangeOutsideTransaction.to_string(),
            "row change outside transaction"
        );
        assert_eq!(
            TransactionBuildError::CommitWithoutBegin.to_string(),
            "commit without begin"
        );
        assert_eq!(
            TransactionBuildError::CommitLsnRegressed.to_string(),
            "commit LSN regressed"
        );
        assert_eq!(
            TransactionBuildError::InvalidTransaction(TransactionValidationError::EndBeforeCommit)
                .to_string(),
            "invalid transaction: end_lsn is before commit_lsn"
        );
    }
}
