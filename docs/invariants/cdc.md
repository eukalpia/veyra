# CDC invariants

These invariants are executable contracts for `veyra-cdc`.

## C1 — PostgreSQL transaction atomicity

`CdcAssembler` emits `TransactionBatch` only after a matching `COMMIT`. `BEGIN`, `XLogData`, and transactional logical messages are not returned as committed state.

Verification: unit sequence tests and live PostgreSQL integration test.

## C2 — Partial transactions are never durable

Only a complete `TransactionBatch` reaches `DurableCdcLog::append`.

Verification: assembler type boundary and tests.

## C3 — feedback never outruns local durability

The CDC pump order is fixed: encode -> append -> write -> `sync_all` -> advance `durable_lsn` -> `update_applied_lsn`.

Verification: code ordering plus restart integration tests.

## C4 — duplicate delivery is idempotent

An already-durable LSN is accepted only when deterministic transaction bytes match the stored bytes exactly. A conflict fails closed.

Verification: exact historical duplicate and conflicting replay tests.

## C5 — torn final append is recoverable

An incomplete final record header or payload is truncated to the last complete record. Complete-record checksum corruption is never repaired silently.

Verification: torn-header, torn-payload, and checksum corruption tests.

## C6 — durable log order is strictly monotonic

Complete records on disk must have strictly increasing end LSNs. Any non-monotonic persisted sequence is corrupt.

Verification: scanner runtime assertion and corruption tests.

## C7 — progress order is always valid

`published_lsn <= applied_lsn <= durable_lsn <= received_lsn` is revalidated after every transition.

Verification: `ProjectionProgress`, `CdcProgressTracker`, property/unit tests.

## C8 — snapshot catch-up cannot intentionally leave a WAL gap

Catch-up begins from the logical slot LSN captured before opening the snapshot, accepting overlap rather than a gap.

Verification: live integration transaction written while the repeatable-read snapshot is still open.

## C9 — corrupt CDC data is never served

Unknown format versions, bad CRCs, invalid bounds, unknown item tags and invalid transaction semantics return errors.

Verification: codec/log tests and fuzz target.

## C10 — CDC work is bounded

Per-transaction item count, item bytes, message prefix bytes, and encoded bytes have explicit limits checked before or during allocation.

Verification: limit tests and fuzz target.
