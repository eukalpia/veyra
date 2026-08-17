use veyra_types::LogSequenceNumber;

use crate::TransactionBatch;

impl TransactionBatch {
    /// Compatibility accessor for the LSN captured by the `pgoutput` BEGIN message.
    ///
    /// PostgreSQL names this field `final_lsn`: it is the final LSN of the transaction
    /// being opened, not the physical location of the BEGIN record itself. Older Veyra
    /// journal code called the same value `begin_lsn`; keeping the accessor here avoids
    /// changing the durable journal layout while preserving the correct PostgreSQL semantics.
    #[must_use]
    pub const fn begin_lsn(&self) -> LogSequenceNumber {
        self.final_lsn()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn begin_lsn_aliases_pgoutput_final_lsn() {
        let batch = TransactionBatch::try_new(
            7,
            LogSequenceNumber::new(11),
            LogSequenceNumber::new(12),
            LogSequenceNumber::new(13),
            Vec::new(),
        )
        .unwrap_or_else(|_| unreachable!());

        assert_eq!(batch.begin_lsn(), LogSequenceNumber::new(11));
        assert_eq!(batch.begin_lsn(), batch.final_lsn());
    }
}
