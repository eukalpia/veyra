# CDC, Storage, and Availability Implementation Plan

**Goal:** Extend the verified Veyra foundation vertically through PostgreSQL `pgoutput` transaction decoding, crash-safe durable replay, immutable segment storage, and baseline interval availability.

**Architecture:** Keep the single-writer/many-reader model. CDC decodes typed `pgoutput` messages into complete `TransactionBatch` values, persists complete batches before projection visibility, and fails closed on gaps or conflicting replay. Storage remains immutable and versioned; availability consumes only validated published state.

**Tech Stack:** stable Rust 1.97.1, std I/O, serde, ArcSwap, property tests, nextest, Miri, cargo-fuzz, cargo-llvm-cov.

## Global Constraints

- PostgreSQL remains authoritative; Veyra exposes no booking mutations.
- Preserve transaction boundaries and never expose partial transactions.
- Enforce `published_lsn <= applied_lsn <= durable_lsn <= received_lsn`.
- Unknown/corrupt semantics fail closed.
- All external input is length-bounded and validated.
- Production crates forbid unsafe code.
- Windows x86_64, Linux x86_64/ARM64, macOS x86_64/ARM64 remain first-class.
- Library coverage gate remains >=99% lines/functions/regions.

### Task 1: CDC domain and pgoutput decoder

Create `veyra-cdc` with typed relation metadata, tuples, row changes, transaction batches, strict `pgoutput` message parsing, and a transaction assembler that rejects nested transactions, row changes outside a transaction, regressing commit LSNs, and unsupported message tags.

### Task 2: Durable transaction journal and replay guard

Add a little-endian V1 journal record owned by Veyra, bounded payloads, CRC32C, `sync_data` durability, crash-tail truncation only for incomplete final records, corruption rejection, and duplicate replay detection keyed by commit LSN plus deterministic transaction fingerprint.

### Task 3: Immutable segment/generation storage

Add an owned segment header/manifest format with checksums, bounds validation, atomic temp-write/fsync/rename, immutable generation publication, and old-reader safety.

### Task 4: Baseline availability engine

Add dense date bitmaps, deterministic interval intersection, bounded queries, reference linear evaluation, and differential/property tests.

### Task 5: Verification

Run format, clippy, all native platform tests, nextest, Miri targets, fuzz smoke, dependency policy, and >=99% library coverage before calling the slice complete.
