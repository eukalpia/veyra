#![forbid(unsafe_code)]

//! Correctness-first PostgreSQL logical replication ingestion for Veyra.
//!
//! This crate owns transaction assembly, durable replay, snapshot boundaries,
//! and PostgreSQL feedback ordering. It deliberately does not interpret Booking
//! Asia business rows yet: raw `pgoutput` bytes remain opaque until a versioned
//! projection schema exists.

mod assembler;
mod codec;
mod durable;
mod model;
mod postgres;
mod progress;
mod pump;
mod snapshot;

pub use assembler::{AssemblerAction, AssemblerError, CdcAssembler, CdcEvent};
pub use codec::{BatchCodecError, decode_transaction_batch, encode_transaction_batch};
pub use durable::{AppendOutcome, DurableCdcLog, DurableLogError, OpenOutcome};
pub use model::{
    BatchLimits, BatchValidationError, LogicalMessage, TransactionBatch, TransactionItem, WalChunk,
};
pub use postgres::{PostgresCdcError, PostgresReplicationStream};
pub use progress::{CdcProgressError, CdcProgressTracker};
pub use pump::{CdcPump, CdcPumpError, CdcPumpEvent};
pub use snapshot::{
    SnapshotBoundary, SnapshotError, SnapshotSession, begin_consistent_snapshot, parse_pg_lsn,
};
