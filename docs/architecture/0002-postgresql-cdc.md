# ADR 0002: PostgreSQL CDC durability pipeline

## Status

Accepted for Milestone 1.

## Context

PostgreSQL remains the authoritative command-side database. Veyra consumes logical replication only to build derived query projections. A CDC fault may cause search fallback, but it must never make a partial transaction query-visible or acknowledge WAL that Veyra has not made durable.

## Decision

The ingestion pipeline is deliberately single-writer:

```text
PostgreSQL pgoutput
    -> pgwire-replication transport
    -> CdcEvent
    -> CdcAssembler
    -> complete TransactionBatch
    -> Veyra durable CDC log
    -> fsync
    -> durable_lsn
    -> PostgreSQL standby feedback
    -> later projection apply/publish
```

`pgoutput` tuple bytes remain opaque in Milestone 1. Interpreting them without a versioned Booking Asia projection schema would couple transport durability to guessed business semantics. A later projection crate will decode known relation/schema versions and fail closed on unknown ones.

### Initial snapshot boundary

Veyra captures the logical slot restart/confirmed-flush LSN before opening a `REPEATABLE READ READ ONLY` snapshot. The snapshot records `pg_current_snapshot()` and a diagnostic WAL LSN. Catch-up always replays from the earlier slot point.

This deliberately permits overlap. Duplicate replay is safe and detectable; an unobserved WAL gap is not. The projection/apply layer must therefore be idempotent.

### Durable format

The CDC log is Veyra-owned and explicitly versioned. All integers are little-endian. Both the file header and each record header carry CRC32C; each transaction payload carries a second CRC32C. Records are length-bounded before allocation and decoded using Veyra's own transaction format.

A final incomplete append is treated as a crash/torn write and truncated to the previous fully validated record. Any checksum mismatch or semantic corruption in a complete record fails closed.

### Duplicate delivery

A transaction with `end_lsn` newer than the durable tail is appended and fsynced. A transaction at an existing historical LSN is accepted only when its deterministic encoded bytes exactly equal the stored transaction. A different transaction at the same LSN, or an old LSN missing from the durable history, is an invariant failure.

### Progress

Veyra tracks four independent positions:

```text
published_lsn <= applied_lsn <= durable_lsn <= received_lsn
```

PostgreSQL feedback advances only after the corresponding transaction is durably fsynced. Query publication is outside this crate and may lag intentionally.

## Consequences

- No partial transaction is exposed by the CDC API.
- Crash recovery may repeat work but does not silently skip WAL.
- Current durability favors correctness over fsync throughput; group commit may be introduced later only with equivalent durability proof.
- CDC log growth is bounded operationally by later checkpoint/compaction retention; unbounded in-process queues are not introduced here.
