#![forbid(unsafe_code)]

//! `PostgreSQL` logical replication primitives for Veyra.
//!
//! This crate owns transaction-boundary preservation, a strict subset-complete decoder for
//! `PostgreSQL` `pgoutput` row-change messages, and crash-safe durable transaction replay state.
//! It deliberately does not own booking mutations or query projections.

mod journal;
mod pgoutput;
mod transaction;

pub use journal::{Journal, JournalError, ReplayDecision, ReplayGuard};
pub use pgoutput::{
    ColumnMetadata, PgOutputDecoder, PgOutputError, PgOutputMessage, RelationMetadata,
    ReplicaIdentity, TupleColumn, TupleData,
};
pub use transaction::{
    ChangeKind, RowChange, TransactionBatch, TransactionBuildError, TransactionBuilder,
    TransactionValidationError,
};
