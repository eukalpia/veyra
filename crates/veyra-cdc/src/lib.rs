#![forbid(unsafe_code)]

//! `PostgreSQL` logical replication primitives for Veyra.
//!
//! This crate owns transaction-boundary preservation, strict `pgoutput` decoding,
//! and crash-safe durable transaction replay state. It deliberately does not own
//! booking mutations or query projections.

#[path = "journal_v2.rs"]
mod journal;
#[path = "pgoutput_v1.rs"]
mod pgoutput;
mod stream;
#[path = "transaction_v1.rs"]
mod transaction;

pub use journal::{Journal, JournalError, ReplayDecision, ReplayGuard};
pub use pgoutput::{
    ColumnMetadata, PgOutputDecoder, PgOutputError, PgOutputMessage, RelationMetadata,
    ReplicaIdentity, TupleColumn, TupleData,
};
pub use stream::{StreamError, TransactionStream};
pub use transaction::{
    ChangeKind, RowChange, TransactionBatch, TransactionBuildError, TransactionBuilder,
    TransactionValidationError,
};
