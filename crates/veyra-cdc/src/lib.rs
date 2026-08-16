#![forbid(unsafe_code)]

//! `PostgreSQL` logical replication primitives for Veyra.
//!
//! This crate owns transaction-boundary preservation, strict `pgoutput` decoding,
//! crash-safe durable transaction replay, and the journal-before-apply acknowledgement boundary.
//! It deliberately does not own booking mutations or query projections.

mod checkpoint;
#[path = "journal_v2.rs"]
mod journal;
mod live;
#[path = "pgoutput_v1.rs"]
mod pgoutput;
mod processor;
mod stream;
#[path = "transaction_v1.rs"]
mod transaction;

pub use checkpoint::{AppliedCheckpoint, AppliedState, CheckpointAdvance, CheckpointError};
pub use journal::{Journal, JournalError, ReplayDecision, ReplayGuard};
pub use live::{
    CdcProgress, CheckpointApplyError, LiveEventOutcome, LiveReplicationError,
    LiveReplicationState, LiveRunSummary, process_replication_event, recover_checkpointed,
    run_pgwire,
};
pub use pgoutput::{
    ColumnMetadata, PgOutputDecoder, PgOutputError, PgOutputMessage, RelationMetadata,
    ReplicaIdentity, TupleColumn, TupleData,
};
pub use processor::{DurableTransactionProcessor, ProcessingOutcome, ProcessorError};
pub use stream::{StreamError, TransactionStream};
pub use transaction::{
    ChangeKind, RowChange, TransactionBatch, TransactionBuildError, TransactionBuilder,
    TransactionValidationError,
};

#[cfg(test)]
impl PartialEq for JournalError {
    fn eq(&self, other: &Self) -> bool {
        core::mem::discriminant(self) == core::mem::discriminant(other)
    }
}

#[cfg(test)]
impl Eq for JournalError {}
