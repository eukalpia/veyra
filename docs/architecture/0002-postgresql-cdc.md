# ADR 0002: PostgreSQL CDC durability pipeline

## Status

Accepted for Milestone 1.

## Context

PostgreSQL remains authoritative. Veyra may accelerate reads, but a CDC fault must never create partial transaction visibility, silently skip WAL, or acknowledge work that is not locally durable.

## Decision

The ingestion pipeline is single-writer:

```text
PostgreSQL pgoutput
  -> checked slot resume proof
  -> replication transport
  -> CdcAssembler
  -> complete TransactionBatch
  -> Veyra CDC log
  -> fsync
  -> durable_lsn
  -> PostgreSQL feedback
  -> later projection apply/publish
```

Raw `pgoutput` tuple bytes remain opaque in Milestone 1. Projection-specific decoding begins only when a versioned relation/schema contract exists.

### Initial snapshot

Veyra captures the logical slot restart/confirmed-flush boundary before opening a `REPEATABLE READ READ ONLY` snapshot. Catch-up starts from the earlier captured slot point. This intentionally permits overlap because duplicate application can be made idempotent while an unobserved WAL gap cannot be repaired by guessing.

### Resume fence

Before every replication connection, Veyra queries `pg_replication_slots` and creates a private `ReplicationStartProof`. Replication cannot start unless:

- the slot exists and is logical;
- the plugin is `pgoutput`;
- the slot is not already active;
- `wal_status` is `reserved` or `extended`;
- `restart_lsn <= requested_lsn`;
- `confirmed_flush_lsn <= requested_lsn`.

`unreserved`, `lost`, missing/unknown fields, a server-confirmed position ahead of Veyra's local checkpoint, or a restart LSN ahead of the requested position all fail closed as a CDC gap/unsupported state. The dedicated slot and single CDC writer are operational invariants.

### Durable format

The CDC journal is Veyra-owned, versioned and little-endian. File headers, record headers and transaction payloads are independently protected by CRC32C and all lengths are bounded before allocation.

A new journal file is `sync_all`'d before use. On Unix, Veyra additionally fsyncs its parent directory after first creation so the directory entry is durable before PostgreSQL feedback can ever depend on the file. On Windows Veyra relies on safe Rust `File::sync_all`; no unsafe Win32 directory-handle workaround is introduced into the production core.

A final incomplete append is treated as a torn crash write and truncated to the previous complete record. A checksum mismatch or semantic corruption in a complete record is never repaired silently.

### Duplicate delivery

A newer transaction is appended and fsynced. An existing historical `end_lsn` is accepted only when deterministic transaction bytes match exactly. Different bytes at the same LSN or an old LSN absent from durable history are invariant failures.

### Progress and feedback

Veyra maintains:

```text
published_lsn <= applied_lsn <= durable_lsn <= received_lsn
```

The pump order is fixed: assemble -> append -> fsync -> mark durable -> feedback. Query publication is a later layer and may intentionally lag.

## Consequences

- No partial transaction is exposed.
- Restart may repeat work but cannot intentionally skip WAL.
- Slot drift or discarded WAL becomes explicit fallback, not silent data loss.
- Current fsync-per-transaction policy favors correctness over throughput; batching requires later proof and benchmarks.
