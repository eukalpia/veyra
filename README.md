# Veyra

**Veyra** is a deterministic semantic availability and travel-search engine for Booking Asia. Its internal codename is **BARE — Booking Asia Realtime Engine**.

Veyra is deliberately **not** the system of record. PostgreSQL/Booking Asia owns bookings, holds, inventory mutations, amendments, cancellations, payments, room assignments, and authoritative checkout validation. Veyra owns derived, read-optimized projection state used to prove, solve, price, and rank search results.

The core operating rule is:

> Flexible at the semantic/compiler layer. Bounded and deterministic at runtime. PostgreSQL remains the final authority. Unknown state fails closed instead of becoming a guessed answer.

## Current status

The V1 core is implemented as a production-candidate workspace and is undergoing final repository-wide qualification. Do not interpret this README as a production-release claim until the required CI matrix is green for the exact release SHA.

Implemented behavior includes:

- PostgreSQL logical-replication/CDC primitives and ordered projection progress;
- immutable segment/storage generation primitives;
- dense availability indexes;
- typed traveler/party graphs with guardians and rooming intents;
- rule AST + compiler, occupancy validation, and stay restrictions;
- fixed-point monetary pricing with occupancy-dependent adjustments;
- bounded exact multi-room solving with deterministic profiles;
- early pruning of already-provably-impossible hard constraints;
- deterministic single-room ranking/search;
- exact property-scoped multi-room search with occupancy-dependent pricing;
- explicit room spatial projection for floor/building and `Near` / `AdjacentRooms` / `ConnectedRooms` semantics;
- fail-closed handling when required topology or projection state is missing;
- atomic publication of runtime status and immutable query payload through one generation object;
- read-your-writes admission using minimum LSN proofs;
- typed internal query service with stable machine-readable rejection codes;
- independent Axum liveness/readiness endpoints;
- portability, coverage, dependency-policy, Rustdoc, Miri, and fuzz-smoke CI gates.

## Workspace

| Crate | Responsibility |
|---|---|
| `veyra-types` | generation IDs, LSNs, projection progress, shared invariants |
| `veyra-cdc` | logical-replication decoding/projection transaction boundaries |
| `veyra-segment` | immutable segment representation |
| `veyra-storage` | durable immutable generation/storage primitives |
| `veyra-availability` | dense bounded room/night availability index |
| `veyra-party` | travelers, age evidence, guardians, relationships, rooming intents |
| `veyra-rules` | semantic rule AST |
| `veyra-rule-compiler` | validated compiled occupancy rules |
| `veyra-occupancy` | room occupancy proof/evaluation |
| `veyra-restrictions` | CTA/CTD/min/max-stay restrictions |
| `veyra-pricing` | fixed-point nightly and occupancy-dependent pricing |
| `veyra-solver` | exact bounded room allocation, topology proof, solution profiles |
| `veyra-ranking` | deterministic single-room ranking/top-k |
| `veyra-query` | single-room and property-scoped multi-room search orchestration |
| `veyra-runtime` | fail-closed state, LSN admission, atomic generation publication |
| `veyra-server` | ops health process + typed internal query boundary |

## Multi-room semantics

Veyra can solve family/group allocations where the correct answer depends on **who is placed in which room**, not just on aggregate guest counts.

For multi-room search:

1. availability, destination, and stay restrictions are applied first;
2. candidate rooms are grouped by `property_id` — one solution never spans properties;
3. the exact solver assigns travelers to rooms under occupancy and rooming constraints;
4. every used room is priced from the **actual adult/child occupancy chosen by the solver**;
5. hard constraints dominate preferences/ranking;
6. budget is checked after exact allocation price is known;
7. results are deterministically ordered by the requested solution profile.

Current exact solver bounds are intentionally explicit:

- maximum travelers in an exact multi-room solve: **16**;
- maximum candidate rooms for one property: **8**;
- bounded state budget via `SolverConfig` (default currently **200,000** explored states);
- a property exceeding the exact room bound is rejected fail-closed — candidate rooms are never silently truncated.

The supported solution profiles are:

- `Cheapest`;
- `FewestRooms`;
- `BestFamilyLayout`.

Projected price remains advisory until Booking Asia performs authoritative checkout/revalidation.

## Spatial and relationship constraints

`SameRoom` and `SeparateRoom` can be proven without spatial metadata. Spatial semantics require an explicit, complete room projection.

`RoomSpatialProjection` provides:

- dense room placements with `floor` and `building`;
- explicit symmetric relation edges for `Near`, `Adjacent`, and `Connected`.

Veyra does **not** infer adjacency or connectedness from room IDs, room numbers, naming conventions, or numerical distance guesses.

Semantics are deliberately strict:

- `SameFloor` and `SameBuilding` use projected placement fields;
- `Near` is true for the same room or for an explicit `Near` edge;
- `AdjacentRooms` requires two distinct rooms and an explicit `Adjacent` edge;
- `ConnectedRooms` requires two distinct rooms and an explicit `Connected` edge;
- within a supplied complete topology index, absence of an edge means the relation is false;
- without the required spatial projection, Veyra returns an unsupported-semantics failure rather than guessing.

## Runtime publication and query admission

A queryable generation is published as one coherent object containing:

- runtime/projection metadata;
- the immutable query payload for that exact generation.

`GenerationState<T>` publishes this object atomically with `ArcSwap`. A reader cannot observe `Ready` metadata from generation N while still using payload N-1.

Before a query is admitted, runtime proves that:

- the service is `Ready`;
- projection integrity state is acceptable;
- `applied_lsn >= minimum_lsn` requested by the caller.

If that proof cannot be made, the query is rejected. Stable service error codes include:

- `not_ready`;
- `stale_projection`;
- `cdc_gap`;
- `corrupt_generation`;
- `version_mismatch`;
- `unsupported_semantics`;
- `overloaded`;
- `internal_invariant_failure`;
- `query_rejected`.

The typed query boundary is intentionally separate from the public HTTP contract. The Axum process currently exposes operations health endpoints; Booking Asia/Elixir remains responsible for the external BFF/API contract.

## Health process

```bash
cargo run -p veyra-server
```

Default bind address: `127.0.0.1:8080`. Override with `VEYRA_BIND`.

- `GET /health/live` — process liveness;
- `GET /health/ready` — `503 Service Unavailable` until runtime can prove a validated queryable state.

Health remains available independently of whether a business query is accepted.

## Supported CI portability targets

- `x86_64-pc-windows-msvc`;
- `x86_64-unknown-linux-gnu`;
- `aarch64-unknown-linux-gnu`;
- `x86_64-apple-darwin`;
- `aarch64-apple-darwin`.

Portable code must not rely on Linux-only syscalls or a fixed SIMD ISA.

## Development and qualification

The workspace pins Rust **1.97.1** in CI.

```bash
cargo metadata --locked --format-version 1
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-features --locked
cargo doc --workspace --all-features --no-deps --locked
```

CI additionally runs:

- `cargo-nextest`;
- production-core coverage with **>=99% lines, functions, and regions**;
- `cargo deny` and `cargo audit --deny warnings`;
- Miri on `veyra-types`;
- bounded fuzz smoke for projection progress;
- the five-target portability matrix above.

The root workspace forbids `unsafe` and denies `unwrap`, `expect`, `panic`, `todo`, `unimplemented`, and `dbg!` through repository lint policy/CI.

## Non-goals and authority boundary

Veyra does not:

- mutate Booking Asia inventory or booking state;
- confirm a booking or payment;
- replace authoritative PostgreSQL transactions;
- invent missing topology, policy, or pricing semantics;
- treat a projected search price as a checkout guarantee;
- expose payment-card data or identity-document data as search state.

See [`docs/invariants/core.md`](docs/invariants/core.md) for the executable invariant register.

## Security

Please read [SECURITY.md](SECURITY.md). Veyra should store only search-required traveler attributes and never payment-card data, identity documents, or unnecessary PII.

## License

Apache-2.0.
