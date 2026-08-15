# Veyra

**Veyra** is a specialized semantic availability and travel-search engine. Its internal
Booking Asia codename is **BARE — Booking Asia Realtime Engine**.

Veyra is deliberately **not** a system of record. PostgreSQL owns bookings, holds,
inventory mutations, amendments, cancellations, payments, room assignments, and every
transactional invariant. Veyra owns only derived, read-optimized state used to prove and
rank travel search results.

The core operating rule is:

> Flexible at the semantic/compiler layer. Simple and deterministic at runtime.
> PostgreSQL remains the final authority. Failure never becomes corruption.

## Milestone status

The repository is currently implementing **Milestone 0 — Foundation**.

Implemented in the foundation branch:

- stable Rust Cargo workspace;
- explicit LSN/projection ordering invariants;
- immutable runtime status publication with `ArcSwap`;
- fail-closed query admission semantics;
- independent Axum administrative process boundary;
- `/health/live` and `/health/ready`;
- property-based tests;
- Criterion benchmark harness;
- five-target GitHub Actions portability matrix;
- 99% library coverage gate;
- dependency audit / license policy / Miri / fuzz-smoke CI.

Not implemented yet, and therefore not claimed:

- PostgreSQL logical replication;
- immutable storage generations;
- availability indexes;
- rules/compiler;
- party graph;
- pricing;
- solver;
- geo;
- ranking;
- Elixir SDK.

Those arrive as vertical milestones only when they have real behavior and tests.

## Supported production targets

- `x86_64-pc-windows-msvc`
- `x86_64-unknown-linux-gnu`
- `aarch64-unknown-linux-gnu`
- `x86_64-apple-darwin`
- `aarch64-apple-darwin`

Portable releases must not depend on Linux-only syscalls or a fixed SIMD ISA.

## Local development

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features --locked
cargo bench -p veyra-runtime --bench snapshot
```

The administrative server is fail-closed until a validated projection exists:

```bash
cargo run -p veyra-server
```

Default bind address: `127.0.0.1:8080`. Override with `VEYRA_BIND`.

- `GET /health/live` — process liveness.
- `GET /health/ready` — `503 Service Unavailable` until a validated queryable state is
  atomically published.

## Security

Please read [SECURITY.md](SECURITY.md). Veyra should store only search-required
traveler attributes and never payment-card data, identity documents, or unnecessary PII.

## License

Apache-2.0.
