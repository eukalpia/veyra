use core::fmt;

use veyra_types::LogSequenceNumber;

use crate::model::{
    BatchLimits, BatchValidationError, LogicalMessage, TransactionBatch, TransactionItem, WalChunk,
};

/// Transport-neutral logical replication event.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CdcEvent {
    KeepAlive {
        wal_end: LogSequenceNumber,
        server_time_micros: i64,
        reply_requested: bool,
    },
    Begin {
        final_lsn: LogSequenceNumber,
        xid: u32,
        commit_time_micros: i64,
    },
    XLogData {
        wal_start: LogSequenceNumber,
        wal_end: LogSequenceNumber,
        server_time_micros: i64,
        data: Vec<u8>,
    },
    Commit {
        lsn: LogSequenceNumber,
        end_lsn: LogSequenceNumber,
        commit_time_micros: i64,
    },
    Message {
        transactional: bool,
        lsn: LogSequenceNumber,
        prefix: String,
        content: Vec<u8>,
    },
    StoppedAt {
        reached: LogSequenceNumber,
    },
}

/// Transaction assembly output. `Transaction` is emitted only after COMMIT.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AssemblerAction {
    None,
    WalObserved(LogSequenceNumber),
    Transaction(TransactionBatch),
    StoppedAt(LogSequenceNumber),
}

#[derive(Clone, Debug)]
struct PendingTransaction {
    xid: u32,
    final_lsn: LogSequenceNumber,
    commit_time_micros: i64,
    items: Vec<TransactionItem>,
}

/// Single-writer `PostgreSQL` transaction-boundary assembler.
#[derive(Clone, Debug)]
pub struct CdcAssembler {
    pending: Option<PendingTransaction>,
    limits: BatchLimits,
}

impl CdcAssembler {
    /// Creates an empty assembler with explicit work bounds.
    #[must_use]
    pub const fn new(limits: BatchLimits) -> Self {
        Self {
            pending: None,
            limits,
        }
    }

    /// True while a `PostgreSQL` transaction is incomplete and therefore invisible.
    #[must_use]
    pub const fn in_transaction(&self) -> bool {
        self.pending.is_some()
    }

    /// Accepts one ordered replication event.
    pub fn push(&mut self, event: CdcEvent) -> Result<AssemblerAction, AssemblerError> {
        match event {
            CdcEvent::KeepAlive { wal_end, .. } => Ok(AssemblerAction::WalObserved(wal_end)),
            CdcEvent::Begin {
                final_lsn,
                xid,
                commit_time_micros,
            } => {
                if self.pending.is_some() {
                    return Err(AssemblerError::NestedBegin);
                }
                self.pending = Some(PendingTransaction {
                    xid,
                    final_lsn,
                    commit_time_micros,
                    items: Vec::new(),
                });
                Ok(AssemblerAction::None)
            }
            CdcEvent::XLogData {
                wal_start,
                wal_end,
                server_time_micros,
                data,
            } => {
                let pending = self
                    .pending
                    .as_mut()
                    .ok_or(AssemblerError::WalOutsideTransaction)?;
                if pending.items.len() >= self.limits.max_items {
                    return Err(AssemblerError::TooManyItems);
                }
                let chunk =
                    WalChunk::try_new(wal_start, wal_end, server_time_micros, data, self.limits)?;
                pending.items.push(TransactionItem::Wal(chunk));
                Ok(AssemblerAction::WalObserved(wal_end))
            }
            CdcEvent::Message {
                transactional,
                lsn,
                prefix,
                content,
            } => {
                if !transactional {
                    return Err(AssemblerError::NonTransactionalLogicalMessage);
                }
                let pending = self
                    .pending
                    .as_mut()
                    .ok_or(AssemblerError::MessageOutsideTransaction)?;
                if pending.items.len() >= self.limits.max_items {
                    return Err(AssemblerError::TooManyItems);
                }
                let message = LogicalMessage::try_new(lsn, prefix, content, self.limits)?;
                pending.items.push(TransactionItem::Message(message));
                Ok(AssemblerAction::WalObserved(lsn))
            }
            CdcEvent::Commit {
                lsn,
                end_lsn,
                commit_time_micros,
            } => {
                let pending = self
                    .pending
                    .take()
                    .ok_or(AssemblerError::CommitWithoutBegin)?;
                if pending.commit_time_micros != commit_time_micros {
                    return Err(AssemblerError::CommitTimeMismatch {
                        begin: pending.commit_time_micros,
                        commit: commit_time_micros,
                    });
                }
                let batch = TransactionBatch::try_new(
                    pending.xid,
                    pending.final_lsn,
                    lsn,
                    end_lsn,
                    commit_time_micros,
                    pending.items,
                    self.limits,
                )?;
                Ok(AssemblerAction::Transaction(batch))
            }
            CdcEvent::StoppedAt { reached } => {
                if self.pending.is_some() {
                    return Err(AssemblerError::StoppedInsideTransaction);
                }
                Ok(AssemblerAction::StoppedAt(reached))
            }
        }
    }
}

/// Invalid transaction event sequence. Veyra fails closed on every variant.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AssemblerError {
    NestedBegin,
    WalOutsideTransaction,
    MessageOutsideTransaction,
    NonTransactionalLogicalMessage,
    CommitWithoutBegin,
    StoppedInsideTransaction,
    TooManyItems,
    CommitTimeMismatch { begin: i64, commit: i64 },
    InvalidBatch(BatchValidationError),
}

impl From<BatchValidationError> for AssemblerError {
    fn from(value: BatchValidationError) -> Self {
        Self::InvalidBatch(value)
    }
}

impl fmt::Display for AssemblerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NestedBegin => formatter.write_str("nested PostgreSQL BEGIN in logical stream"),
            Self::WalOutsideTransaction => {
                formatter.write_str("PostgreSQL WAL data outside transaction")
            }
            Self::MessageOutsideTransaction => {
                formatter.write_str("transactional logical message outside transaction")
            }
            Self::NonTransactionalLogicalMessage => {
                formatter.write_str("non-transactional logical messages are unsupported")
            }
            Self::CommitWithoutBegin => formatter.write_str("PostgreSQL COMMIT without BEGIN"),
            Self::StoppedInsideTransaction => {
                formatter.write_str("replication stopped inside incomplete transaction")
            }
            Self::TooManyItems => {
                formatter.write_str("transaction item limit reached before COMMIT")
            }
            Self::CommitTimeMismatch { begin, commit } => write!(
                formatter,
                "BEGIN commit time {begin} differs from COMMIT time {commit}"
            ),
            Self::InvalidBatch(error) => {
                write!(formatter, "invalid committed transaction: {error}")
            }
        }
    }
}

impl std::error::Error for AssemblerError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::InvalidBatch(error) => Some(error),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn begin() -> CdcEvent {
        CdcEvent::Begin {
            final_lsn: LogSequenceNumber::new(10),
            xid: 1,
            commit_time_micros: 7,
        }
    }

    fn commit() -> CdcEvent {
        CdcEvent::Commit {
            lsn: LogSequenceNumber::new(10),
            end_lsn: LogSequenceNumber::new(11),
            commit_time_micros: 7,
        }
    }

    #[test]
    fn incomplete_transaction_is_never_exposed() -> Result<(), AssemblerError> {
        let mut assembler = CdcAssembler::new(BatchLimits::default());
        assert!(!assembler.in_transaction());
        assert_eq!(assembler.push(begin())?, AssemblerAction::None);
        assert!(assembler.in_transaction());
        assert_eq!(
            assembler.push(CdcEvent::XLogData {
                wal_start: LogSequenceNumber::new(9),
                wal_end: LogSequenceNumber::new(10),
                server_time_micros: 7,
                data: vec![1],
            })?,
            AssemblerAction::WalObserved(LogSequenceNumber::new(10))
        );
        assert!(assembler.in_transaction());
        let action = assembler.push(commit())?;
        let AssemblerAction::Transaction(batch) = action else {
            unreachable!("COMMIT must produce a complete batch");
        };
        assert_eq!(batch.xid(), 1);
        assert_eq!(batch.items().len(), 1);
        assert!(!assembler.in_transaction());
        Ok(())
    }

    #[test]
    fn keepalive_and_stop_are_visible_without_mutating_transaction_state()
    -> Result<(), AssemblerError> {
        let mut assembler = CdcAssembler::new(BatchLimits::default());
        assert_eq!(
            assembler.push(CdcEvent::KeepAlive {
                wal_end: LogSequenceNumber::new(20),
                server_time_micros: 0,
                reply_requested: true,
            })?,
            AssemblerAction::WalObserved(LogSequenceNumber::new(20))
        );
        assert_eq!(
            assembler.push(CdcEvent::StoppedAt {
                reached: LogSequenceNumber::new(21),
            })?,
            AssemblerAction::StoppedAt(LogSequenceNumber::new(21))
        );
        Ok(())
    }

    #[test]
    fn logical_messages_are_transactional_only() -> Result<(), AssemblerError> {
        let mut assembler = CdcAssembler::new(BatchLimits::default());
        assert_eq!(
            assembler.push(CdcEvent::Message {
                transactional: false,
                lsn: LogSequenceNumber::new(1),
                prefix: "x".into(),
                content: vec![],
            }),
            Err(AssemblerError::NonTransactionalLogicalMessage)
        );
        assert_eq!(assembler.push(begin())?, AssemblerAction::None);
        assert_eq!(
            assembler.push(CdcEvent::Message {
                transactional: true,
                lsn: LogSequenceNumber::new(9),
                prefix: "x".into(),
                content: vec![1],
            })?,
            AssemblerAction::WalObserved(LogSequenceNumber::new(9))
        );
        let AssemblerAction::Transaction(batch) = assembler.push(commit())? else {
            unreachable!("commit produces a batch");
        };
        assert!(matches!(batch.items(), [TransactionItem::Message(_)]));
        Ok(())
    }

    #[test]
    fn invalid_sequences_fail_closed() -> Result<(), AssemblerError> {
        let mut assembler = CdcAssembler::new(BatchLimits::default());
        assert_eq!(
            assembler.push(commit()),
            Err(AssemblerError::CommitWithoutBegin)
        );
        assert_eq!(
            assembler.push(CdcEvent::XLogData {
                wal_start: LogSequenceNumber::ZERO,
                wal_end: LogSequenceNumber::ZERO,
                server_time_micros: 0,
                data: vec![],
            }),
            Err(AssemblerError::WalOutsideTransaction)
        );
        assert_eq!(
            assembler.push(CdcEvent::Message {
                transactional: true,
                lsn: LogSequenceNumber::ZERO,
                prefix: String::new(),
                content: Vec::new(),
            }),
            Err(AssemblerError::MessageOutsideTransaction)
        );
        assert_eq!(assembler.push(begin())?, AssemblerAction::None);
        assert_eq!(assembler.push(begin()), Err(AssemblerError::NestedBegin));
        assert_eq!(
            assembler.push(CdcEvent::StoppedAt {
                reached: LogSequenceNumber::new(10),
            }),
            Err(AssemblerError::StoppedInsideTransaction)
        );
        Ok(())
    }

    #[test]
    fn bounds_and_commit_consistency_fail_closed() -> Result<(), AssemblerError> {
        let limits = BatchLimits {
            max_items: 0,
            ..BatchLimits::default()
        };
        let mut assembler = CdcAssembler::new(limits);
        assembler.push(begin())?;
        assert_eq!(
            assembler.push(CdcEvent::XLogData {
                wal_start: LogSequenceNumber::new(9),
                wal_end: LogSequenceNumber::new(10),
                server_time_micros: 7,
                data: Vec::new(),
            }),
            Err(AssemblerError::TooManyItems)
        );

        let mut assembler = CdcAssembler::new(BatchLimits::default());
        assembler.push(begin())?;
        assert_eq!(
            assembler.push(CdcEvent::Commit {
                lsn: LogSequenceNumber::new(10),
                end_lsn: LogSequenceNumber::new(11),
                commit_time_micros: 8,
            }),
            Err(AssemblerError::CommitTimeMismatch {
                begin: 7,
                commit: 8
            })
        );
        Ok(())
    }

    #[test]
    fn diagnostics_and_sources_are_stable() {
        let simple = [
            AssemblerError::NestedBegin,
            AssemblerError::WalOutsideTransaction,
            AssemblerError::MessageOutsideTransaction,
            AssemblerError::NonTransactionalLogicalMessage,
            AssemblerError::CommitWithoutBegin,
            AssemblerError::StoppedInsideTransaction,
            AssemblerError::TooManyItems,
            AssemblerError::CommitTimeMismatch {
                begin: 1,
                commit: 2,
            },
        ];
        for error in simple {
            assert!(!error.to_string().is_empty());
            assert!(std::error::Error::source(&error).is_none());
        }
        let invalid = AssemblerError::InvalidBatch(BatchValidationError::EndBeforeCommit);
        assert!(
            invalid
                .to_string()
                .contains("invalid committed transaction")
        );
        assert!(std::error::Error::source(&invalid).is_some());
    }
}
