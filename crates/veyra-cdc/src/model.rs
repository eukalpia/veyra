use core::fmt;

use veyra_types::LogSequenceNumber;

/// Hard bounds for one decoded or assembled `PostgreSQL` transaction.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BatchLimits {
    /// Maximum number of logical items in one transaction.
    pub max_items: usize,
    /// Maximum bytes accepted for one WAL chunk or logical message body.
    pub max_item_bytes: usize,
    /// Maximum UTF-8 bytes accepted for a logical message prefix.
    pub max_prefix_bytes: usize,
    /// Maximum deterministic encoded transaction payload.
    pub max_encoded_bytes: usize,
}

impl Default for BatchLimits {
    fn default() -> Self {
        Self {
            max_items: 262_144,
            max_item_bytes: 64 * 1024 * 1024,
            max_prefix_bytes: 4 * 1024,
            max_encoded_bytes: 128 * 1024 * 1024,
        }
    }
}

/// Opaque `pgoutput` WAL bytes observed inside one `PostgreSQL` transaction.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WalChunk {
    wal_start: LogSequenceNumber,
    wal_end: LogSequenceNumber,
    server_time_micros: i64,
    data: Vec<u8>,
}

impl WalChunk {
    /// Creates a bounded opaque WAL chunk.
    pub fn try_new(
        wal_start: LogSequenceNumber,
        wal_end: LogSequenceNumber,
        server_time_micros: i64,
        data: Vec<u8>,
        limits: BatchLimits,
    ) -> Result<Self, BatchValidationError> {
        if wal_end < wal_start {
            return Err(BatchValidationError::WalEndBeforeStart);
        }
        if data.len() > limits.max_item_bytes {
            return Err(BatchValidationError::ItemTooLarge {
                actual: data.len(),
                maximum: limits.max_item_bytes,
            });
        }
        Ok(Self {
            wal_start,
            wal_end,
            server_time_micros,
            data,
        })
    }

    /// Start WAL coordinate from the replication envelope.
    #[must_use]
    pub const fn wal_start(&self) -> LogSequenceNumber {
        self.wal_start
    }

    /// End WAL coordinate reported by `PostgreSQL` for this message.
    #[must_use]
    pub const fn wal_end(&self) -> LogSequenceNumber {
        self.wal_end
    }

    /// `PostgreSQL` server timestamp from the replication envelope.
    #[must_use]
    pub const fn server_time_micros(&self) -> i64 {
        self.server_time_micros
    }

    /// Opaque protocol bytes. Semantics are intentionally deferred.
    #[must_use]
    pub fn data(&self) -> &[u8] {
        &self.data
    }
}

/// `PostgreSQL` logical decoding message attached to a transaction.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LogicalMessage {
    lsn: LogSequenceNumber,
    prefix: String,
    content: Vec<u8>,
}

impl LogicalMessage {
    /// Creates a bounded transactional logical message.
    pub fn try_new(
        lsn: LogSequenceNumber,
        prefix: String,
        content: Vec<u8>,
        limits: BatchLimits,
    ) -> Result<Self, BatchValidationError> {
        if prefix.len() > limits.max_prefix_bytes {
            return Err(BatchValidationError::PrefixTooLarge {
                actual: prefix.len(),
                maximum: limits.max_prefix_bytes,
            });
        }
        if content.len() > limits.max_item_bytes {
            return Err(BatchValidationError::ItemTooLarge {
                actual: content.len(),
                maximum: limits.max_item_bytes,
            });
        }
        Ok(Self {
            lsn,
            prefix,
            content,
        })
    }

    /// Message LSN.
    #[must_use]
    pub const fn lsn(&self) -> LogSequenceNumber {
        self.lsn
    }

    /// Logical message prefix.
    #[must_use]
    pub fn prefix(&self) -> &str {
        &self.prefix
    }

    /// Logical message content.
    #[must_use]
    pub fn content(&self) -> &[u8] {
        &self.content
    }
}

/// One ordered item within a committed `PostgreSQL` transaction.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TransactionItem {
    /// Opaque `pgoutput` bytes.
    Wal(WalChunk),
    /// Transactional logical decoding message.
    Message(LogicalMessage),
}

impl TransactionItem {
    pub(crate) fn payload_len(&self) -> usize {
        match self {
            Self::Wal(chunk) => chunk.data.len(),
            Self::Message(message) => message.prefix.len().saturating_add(message.content.len()),
        }
    }
}

/// A complete `PostgreSQL` transaction. It is query-invisible until `Commit`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TransactionBatch {
    xid: u32,
    begin_final_lsn: LogSequenceNumber,
    commit_lsn: LogSequenceNumber,
    end_lsn: LogSequenceNumber,
    commit_time_micros: i64,
    items: Vec<TransactionItem>,
}

impl TransactionBatch {
    /// Builds a complete, bounded transaction batch.
    pub fn try_new(
        xid: u32,
        begin_final_lsn: LogSequenceNumber,
        commit_lsn: LogSequenceNumber,
        end_lsn: LogSequenceNumber,
        commit_time_micros: i64,
        items: Vec<TransactionItem>,
        limits: BatchLimits,
    ) -> Result<Self, BatchValidationError> {
        if begin_final_lsn != commit_lsn {
            return Err(BatchValidationError::CommitLsnMismatch {
                begin_final_lsn,
                commit_lsn,
            });
        }
        if end_lsn < commit_lsn {
            return Err(BatchValidationError::EndBeforeCommit);
        }
        if items.len() > limits.max_items {
            return Err(BatchValidationError::TooManyItems {
                actual: items.len(),
                maximum: limits.max_items,
            });
        }
        let payload_bytes = items.iter().try_fold(0usize, |sum, item| {
            sum.checked_add(item.payload_len())
                .ok_or(BatchValidationError::EncodedLengthOverflow)
        })?;
        if payload_bytes > limits.max_encoded_bytes {
            return Err(BatchValidationError::EncodedTooLarge {
                actual: payload_bytes,
                maximum: limits.max_encoded_bytes,
            });
        }
        Ok(Self {
            xid,
            begin_final_lsn,
            commit_lsn,
            end_lsn,
            commit_time_micros,
            items,
        })
    }

    /// `PostgreSQL` transaction ID.
    #[must_use]
    pub const fn xid(&self) -> u32 {
        self.xid
    }

    /// Final LSN declared by `BEGIN`.
    #[must_use]
    pub const fn begin_final_lsn(&self) -> LogSequenceNumber {
        self.begin_final_lsn
    }

    /// Commit LSN.
    #[must_use]
    pub const fn commit_lsn(&self) -> LogSequenceNumber {
        self.commit_lsn
    }

    /// End LSN; this is the durable replay checkpoint used by Veyra.
    #[must_use]
    pub const fn end_lsn(&self) -> LogSequenceNumber {
        self.end_lsn
    }

    /// Commit timestamp supplied by `PostgreSQL`.
    #[must_use]
    pub const fn commit_time_micros(&self) -> i64 {
        self.commit_time_micros
    }

    /// Ordered transaction items.
    #[must_use]
    pub fn items(&self) -> &[TransactionItem] {
        &self.items
    }
}

/// Invalid or unbounded transaction content.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BatchValidationError {
    WalEndBeforeStart,
    ItemTooLarge {
        actual: usize,
        maximum: usize,
    },
    PrefixTooLarge {
        actual: usize,
        maximum: usize,
    },
    TooManyItems {
        actual: usize,
        maximum: usize,
    },
    EncodedLengthOverflow,
    EncodedTooLarge {
        actual: usize,
        maximum: usize,
    },
    CommitLsnMismatch {
        begin_final_lsn: LogSequenceNumber,
        commit_lsn: LogSequenceNumber,
    },
    EndBeforeCommit,
}

impl fmt::Display for BatchValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::WalEndBeforeStart => {
                formatter.write_str("WAL chunk end LSN precedes its start LSN")
            }
            Self::ItemTooLarge { actual, maximum } => {
                write!(
                    formatter,
                    "transaction item is {actual} bytes; maximum is {maximum}"
                )
            }
            Self::PrefixTooLarge { actual, maximum } => {
                write!(
                    formatter,
                    "logical message prefix is {actual} bytes; maximum is {maximum}"
                )
            }
            Self::TooManyItems { actual, maximum } => {
                write!(
                    formatter,
                    "transaction has {actual} items; maximum is {maximum}"
                )
            }
            Self::EncodedLengthOverflow => {
                formatter.write_str("transaction encoded length overflow")
            }
            Self::EncodedTooLarge { actual, maximum } => {
                write!(
                    formatter,
                    "transaction payload is {actual} bytes; maximum is {maximum}"
                )
            }
            Self::CommitLsnMismatch {
                begin_final_lsn,
                commit_lsn,
            } => write!(
                formatter,
                "BEGIN final LSN {} differs from COMMIT LSN {}",
                begin_final_lsn.get(),
                commit_lsn.get()
            ),
            Self::EndBeforeCommit => formatter.write_str("transaction end LSN precedes commit LSN"),
        }
    }
}

impl std::error::Error for BatchValidationError {}

#[cfg(test)]
pub(crate) fn sample_batch() -> TransactionBatch {
    let limits = BatchLimits::default();
    let wal = WalChunk::try_new(
        LogSequenceNumber::new(90),
        LogSequenceNumber::new(100),
        11,
        vec![1, 2, 3],
        limits,
    )
    .unwrap_or_else(|error| unreachable!("static sample WAL must be valid: {error}"));
    let message = LogicalMessage::try_new(
        LogSequenceNumber::new(95),
        "veyra".to_owned(),
        vec![8, 9],
        limits,
    )
    .unwrap_or_else(|error| unreachable!("static sample message must be valid: {error}"));
    TransactionBatch::try_new(
        7,
        LogSequenceNumber::new(100),
        LogSequenceNumber::new(100),
        LogSequenceNumber::new(101),
        11,
        vec![TransactionItem::Wal(wal), TransactionItem::Message(message)],
        limits,
    )
    .unwrap_or_else(|error| unreachable!("static sample batch must be valid: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn batch_exposes_exact_values() {
        let batch = sample_batch();
        assert_eq!(batch.xid(), 7);
        assert_eq!(batch.begin_final_lsn(), LogSequenceNumber::new(100));
        assert_eq!(batch.commit_lsn(), LogSequenceNumber::new(100));
        assert_eq!(batch.end_lsn(), LogSequenceNumber::new(101));
        assert_eq!(batch.commit_time_micros(), 11);
        assert_eq!(batch.items().len(), 2);
        match &batch.items()[0] {
            TransactionItem::Wal(wal) => {
                assert_eq!(wal.wal_start(), LogSequenceNumber::new(90));
                assert_eq!(wal.wal_end(), LogSequenceNumber::new(100));
                assert_eq!(wal.server_time_micros(), 11);
                assert_eq!(wal.data(), [1, 2, 3]);
            }
            TransactionItem::Message(_) => unreachable!("first sample item is WAL"),
        }
        match &batch.items()[1] {
            TransactionItem::Message(message) => {
                assert_eq!(message.lsn(), LogSequenceNumber::new(95));
                assert_eq!(message.prefix(), "veyra");
                assert_eq!(message.content(), [8, 9]);
            }
            TransactionItem::Wal(_) => unreachable!("second sample item is message"),
        }
    }

    #[test]
    fn invalid_wal_and_message_limits_fail_closed() {
        let tiny = BatchLimits {
            max_items: 1,
            max_item_bytes: 1,
            max_prefix_bytes: 1,
            max_encoded_bytes: 1,
        };
        assert!(matches!(
            WalChunk::try_new(
                LogSequenceNumber::new(2),
                LogSequenceNumber::new(1),
                0,
                Vec::new(),
                tiny,
            ),
            Err(BatchValidationError::WalEndBeforeStart)
        ));
        assert!(matches!(
            WalChunk::try_new(
                LogSequenceNumber::ZERO,
                LogSequenceNumber::ZERO,
                0,
                vec![1, 2],
                tiny,
            ),
            Err(BatchValidationError::ItemTooLarge { .. })
        ));
        assert!(matches!(
            LogicalMessage::try_new(LogSequenceNumber::ZERO, "xx".into(), vec![], tiny),
            Err(BatchValidationError::PrefixTooLarge { .. })
        ));
        assert!(matches!(
            LogicalMessage::try_new(LogSequenceNumber::ZERO, "x".into(), vec![1, 2], tiny),
            Err(BatchValidationError::ItemTooLarge { .. })
        ));
    }

    #[test]
    fn invalid_batch_constraints_fail_closed() {
        let limits = BatchLimits::default();
        assert!(matches!(
            TransactionBatch::try_new(
                1,
                LogSequenceNumber::new(2),
                LogSequenceNumber::new(1),
                LogSequenceNumber::new(2),
                0,
                Vec::new(),
                limits,
            ),
            Err(BatchValidationError::CommitLsnMismatch { .. })
        ));
        assert!(matches!(
            TransactionBatch::try_new(
                1,
                LogSequenceNumber::new(2),
                LogSequenceNumber::new(2),
                LogSequenceNumber::new(1),
                0,
                Vec::new(),
                limits,
            ),
            Err(BatchValidationError::EndBeforeCommit)
        ));
        let tiny = BatchLimits {
            max_items: 0,
            ..limits
        };
        assert!(matches!(
            TransactionBatch::try_new(
                1,
                LogSequenceNumber::ZERO,
                LogSequenceNumber::ZERO,
                LogSequenceNumber::ZERO,
                0,
                vec![TransactionItem::Wal(
                    WalChunk::try_new(
                        LogSequenceNumber::ZERO,
                        LogSequenceNumber::ZERO,
                        0,
                        Vec::new(),
                        limits,
                    )
                    .unwrap_or_else(|error| unreachable!("static WAL must be valid: {error}"))
                )],
                tiny,
            ),
            Err(BatchValidationError::TooManyItems { .. })
        ));
    }

    #[test]
    fn payload_limit_is_enforced() {
        let limits = BatchLimits {
            max_encoded_bytes: 1,
            ..BatchLimits::default()
        };
        let wal = WalChunk::try_new(
            LogSequenceNumber::ZERO,
            LogSequenceNumber::ZERO,
            0,
            vec![1, 2],
            BatchLimits::default(),
        )
        .unwrap_or_else(|error| unreachable!("static WAL must be valid: {error}"));
        assert!(matches!(
            TransactionBatch::try_new(
                1,
                LogSequenceNumber::ZERO,
                LogSequenceNumber::ZERO,
                LogSequenceNumber::ZERO,
                0,
                vec![TransactionItem::Wal(wal)],
                limits,
            ),
            Err(BatchValidationError::EncodedTooLarge { .. })
        ));
    }

    #[test]
    fn diagnostics_are_stable() {
        assert_eq!(
            BatchValidationError::WalEndBeforeStart.to_string(),
            "WAL chunk end LSN precedes its start LSN"
        );
        assert!(
            BatchValidationError::ItemTooLarge {
                actual: 2,
                maximum: 1
            }
            .to_string()
            .contains("2 bytes")
        );
        assert!(
            BatchValidationError::PrefixTooLarge {
                actual: 2,
                maximum: 1
            }
            .to_string()
            .contains("prefix is 2 bytes")
        );
        assert!(
            BatchValidationError::TooManyItems {
                actual: 2,
                maximum: 1
            }
            .to_string()
            .contains("2 items")
        );
        assert_eq!(
            BatchValidationError::EncodedLengthOverflow.to_string(),
            "transaction encoded length overflow"
        );
        assert!(
            BatchValidationError::EncodedTooLarge {
                actual: 2,
                maximum: 1
            }
            .to_string()
            .contains("payload is 2 bytes")
        );
        assert!(
            BatchValidationError::CommitLsnMismatch {
                begin_final_lsn: LogSequenceNumber::new(2),
                commit_lsn: LogSequenceNumber::new(1),
            }
            .to_string()
            .contains("2 differs")
        );
        assert_eq!(
            BatchValidationError::EndBeforeCommit.to_string(),
            "transaction end LSN precedes commit LSN"
        );
    }
}
