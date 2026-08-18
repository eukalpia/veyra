# Veyra V1 Threat Model

## Scope

This threat model covers the Veyra V1 projection and query path: PostgreSQL logical replication input, CDC transaction assembly, durable journal/checkpoint state, immutable segment publication, atomic runtime generations, deterministic single-room and multi-room query execution, and the HTTP service boundary.

Veyra is an advisory search projection. PostgreSQL and the Booking Asia checkout path remain authoritative for inventory, restrictions, and final price. Veyra must never upgrade uncertain projection state into an authoritative booking decision.

## Assets and security properties

- **Projection correctness:** a published generation represents a prefix of committed PostgreSQL transactions and never a torn or reordered transaction.
- **Monotonic progress:** `published <= applied <= durable <= received` for all accepted projection progress values.
- **Transaction ordering:** `begin <= commit <= end` for all accepted projected transactions.
- **Fail-closed query admission:** only a runtime generation in phase `Ready` may serve queries.
- **Tenant and property isolation:** a multi-room solution may not span properties; tenant filtering remains an upstream authoritative responsibility and must be preserved in projected identifiers.
- **Bounded resource use:** request size, party size, candidate rooms, solver states, valid solutions, price horizon, and result count have hard limits.
- **Determinism:** equal projection state and equal request input produce the same validation, allocation, price, ranking, and explanation output.
- **Money integrity:** projected prices use checked fixed-point integer arithmetic; no floating-point money path exists.

## Trust boundaries

### PostgreSQL replication stream

Replication bytes, relation metadata, transaction ordering, and WAL positions are treated as untrusted input until decoded and validated. Unknown message kinds, truncated frames, relation mismatches, oversized values, and invalid LSN order must be rejected without advancing an acknowledged checkpoint.

### Durable local state

Journal, checkpoint, and segment files may be truncated, stale, corrupted, or replaced after a crash. Readers validate framing, lengths, checksums or version markers where available, monotonic positions, and complete transaction boundaries before publication.

### Serialized runtime state

Serde is a trust boundary. Derived deserialization must not bypass constructors or state-machine invariants. Runtime admission, projection progress, and projected transaction ordering are validated after deserialization.

### Query and HTTP input

JSON bodies, service-day values, party graphs, budgets, limits, rooming relations, and topology edges are untrusted. Validation occurs before allocation or ranking. Unsupported or unprovable hard constraints are explicit errors rather than ignored preferences.

### NIF and external integration boundary

V1 does not require unsafe code or a native extension in the deterministic core. Any future Rust NIF must remain a narrow, versioned boundary with dirty-scheduler use for blocking work, panic containment, input limits, and a pure Elixir fallback or circuit breaker.

## Principal threats and mitigations

| Threat | Consequence | Mitigation |
|---|---|---|
| WAL frame truncation or malformed pgoutput | corrupted projection or process crash | bounded decoder, explicit parse errors, truncation matrices, no checkpoint advancement on failure |
| acknowledgement before durable checkpoint | data loss after restart | durable progress is persisted before acknowledgement; recovery tests exercise failure ordering |
| forged serde state | non-ready query admission or LSN regression | validated deserialization and explicit `Ready` admission guard |
| cross-property room allocation | invalid group offer | candidates are grouped by property before exact solving |
| topology guessed from identifiers | false adjacency/connection guarantee | only explicit projected topology edges satisfy topology relations |
| combinatorial party input | CPU exhaustion | hard traveler/room/state/solution bounds; state-budget exhaustion is an error |
| price overflow or negative projected total | incorrect ordering or budget bypass | checked `MoneyMicros` arithmetic and explicit pricing errors |
| stale generation served as healthy | silent correctness drift | runtime phase/progress health, atomic generation publication, fail-closed readiness |
| dependency compromise | build or runtime compromise | locked graph, cargo-deny, cargo-audit, minimal dependencies, CI permissions restricted to read except one-shot maintenance jobs |
| oversized or malformed HTTP body | memory/CPU pressure or 500 responses | body and semantic bounds, typed rejection mapping, malformed/method/route tests |

## Residual risks

- Search remains stale between the authoritative commit and projection publication. Callers must revalidate at checkout.
- A valid but adversarial request can consume the configured solver budget. Capacity planning must treat the maximum budget as a per-request cost ceiling and enforce upstream concurrency limits.
- Host filesystem, kernel, CI runner, and PostgreSQL credentials are outside the Rust type system. Deployment must use least privilege, read-only secrets, process isolation, and monitored disk capacity.
- Disaster recovery depends on retaining or rebuilding the authoritative projection source. Journal and segment backups are useful accelerators, not substitutes for PostgreSQL backups.

## Security regression requirements

A release is not qualified unless the locked build, clippy with warnings denied, workspace tests, nextest, >=99% production coverage, cargo-deny, cargo-audit, rustdoc, Miri core check, fuzz smoke, cross-platform matrix, and serialized-invariant regressions all pass on the exact release commit.
