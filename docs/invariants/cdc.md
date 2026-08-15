# CDC invariants

## C1 — transaction atomicity
`CdcAssembler` emits a transaction only after COMMIT.

## C2 — partial transactions are never durable
Only complete `TransactionBatch` values can enter the durable journal.

## C3 — feedback never outruns local durability
Commit ordering is assemble -> append -> write -> `sync_all` -> durable LSN -> PostgreSQL feedback.

## C4 — duplicate delivery is idempotent
An existing LSN is accepted only when canonical bytes exactly match durable history.

## C5 — torn final append is recoverable
Only an incomplete final record may be truncated automatically. Complete corruption fails closed.

## C6 — durable journal order is strictly monotonic
Complete on-disk records have strictly increasing end LSNs.

## C7 — progress order is always valid
`published_lsn <= applied_lsn <= durable_lsn <= received_lsn` is revalidated after transitions.

## C8 — bootstrap chooses overlap over a WAL gap
Snapshot catch-up starts from the slot boundary captured before the repeatable-read snapshot.

## C9 — corrupt CDC data is never served
Unknown formats, invalid bounds, CRC failures and invalid transaction semantics return errors.

## C10 — CDC work is bounded
Transaction item count, item bytes, message prefix bytes and encoded bytes have explicit limits.

## C11 — replication resume must be proven
Before `START_REPLICATION`, slot plugin, activity, WAL status, restart LSN and confirmed-flush LSN are checked. If the server has discarded or confirmed beyond Veyra's local checkpoint, Veyra returns a fail-closed CDC gap instead of starting from an ambiguous position.

## C12 — a newly created durable journal survives metadata flush boundaries
The journal file is `sync_all`'d before use; Unix additionally fsyncs the parent directory after first creation. PostgreSQL feedback can only happen after journal initialization and transaction durability complete.

Each invariant is covered by unit, corruption, fuzz, Miri or live PostgreSQL tests where applicable. No invariant is weakened to improve benchmark numbers.
