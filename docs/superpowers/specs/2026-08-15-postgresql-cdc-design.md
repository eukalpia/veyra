# PostgreSQL CDC — Milestone 1 design

## Goal

Consume PostgreSQL logical replication without gaps or partial transaction visibility, make committed transactions locally durable before feedback, and recover idempotently after crashes.

## Architecture

One CDC writer owns the logical replication stream. Transport events feed a bounded transaction assembler. Only COMMIT creates a `TransactionBatch`; the batch is deterministically encoded into a Veyra-owned append-only log and fsynced before the durable LSN or PostgreSQL feedback may advance.

Initial bootstrap uses a conservative overlap boundary: capture the logical slot restart/confirmed-flush LSN before opening a repeatable-read read-only snapshot, build from that snapshot, then replay from the earlier slot position. Overlap is acceptable; a gap is not.

Raw `pgoutput` bytes remain opaque in this milestone. Projection-specific relation/schema interpretation is intentionally deferred until a versioned projection contract exists.

## Failure behavior

Unknown format versions, malformed lengths, CRC mismatch, transaction-boundary violations, conflicting duplicate replay, missing historical checkpoints, and progress-order violations all fail closed. Only an incomplete final record is automatically repaired, by truncating to the last completely validated record.

## Validation

The milestone requires unit tests, bounds/corruption tests, live PostgreSQL snapshot/catch-up/restart tests, fuzzing of the transaction decoder, Miri on pure deterministic components, five-platform CI, dependency audit, and at least 99% lines/functions/regions for library code.
