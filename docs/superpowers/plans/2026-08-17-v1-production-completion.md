# Veyra V1 Production Completion Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make Veyra's deterministic core correctly solve and price complex multi-room parties through the public query pipeline, then expose only verified behavior through the runtime boundary.

**Architecture:** Keep PostgreSQL/Booking Asia authoritative and Veyra fail-closed. Pricing is evaluated from the actual room occupancy chosen by the solver, multi-room search is grouped by property, and any topology relation that cannot be proven from projected catalog data remains unsupported rather than guessed. Existing single-room search stays compatible while multi-room orchestration is added beside it.

**Tech Stack:** Rust 1.97.1, workspace crates `veyra-party`, `veyra-occupancy`, `veyra-pricing`, `veyra-solver`, `veyra-query`, `veyra-runtime`, `veyra-server`; GitHub Actions; cargo-nextest; cargo-llvm-cov.

## Global Constraints

- No floating-point money arithmetic.
- No `unsafe`, `unwrap`, `expect`, `panic`, `todo`, or `unimplemented` in production code.
- Missing or unprovable semantics fail closed.
- Hard constraints are evaluated before ranking.
- Search projections are advisory; authoritative checkout remains outside Veyra.
- Production coverage gate remains >= 99% for lines, functions, and regions.
- Existing single-room query behavior remains source-compatible.
- Do not infer `Near`, `AdjacentRooms`, or `ConnectedRooms` from room IDs or room numbers.

---

### Task 1: Reproducible dependency baseline

**Files:**
- Modify: `Cargo.lock`
- Modify: `.github/workflows/ci.yml`

**Interfaces:**
- Consumes: workspace manifests.
- Produces: a lockfile that resolves with `cargo metadata --locked --format-version 1` including full dependencies.

- [x] Reproduce the stale-lockfile failure on the integration head.
- [x] Regenerate `Cargo.lock` with pinned Rust 1.97.1.
- [x] Change root and fuzz lockfile gates to resolve the full dependency graph instead of `--no-deps`.
- [ ] Run the complete CI matrix and record the first clean baseline SHA.

### Task 2: Occupancy-dependent solver pricing

**Files:**
- Modify: `crates/veyra-solver/Cargo.toml`
- Modify: `crates/veyra-solver/src/lib.rs`
- Test: `crates/veyra-solver/tests/priced_solver.rs`

**Interfaces:**
- Consumes: `PriceVector::quote(check_in_day, check_out_day, adults, children, OccupancyAdjustment)` and `OccupancyReport` from `validate_room`.
- Produces: `PricedRoomOffer` and `solve_priced(...)`, while preserving existing `RoomOffer` and `solve(...)`.

- [ ] Add a failing regression where static base-room prices choose a different allocation than exact occupancy-dependent prices.
- [ ] Verify the new test fails because `solve_priced` does not yet exist.
- [ ] Add `PricedRoomOffer { room_id, prices, occupancy_adjustment, floor, building, adult_age, occupancy_rule }`.
- [ ] Add `solve_priced(party, check_in_date, check_in_day, check_out_day, offers, config)`.
- [ ] At each valid leaf, price each used room from its actual adult/child counts returned by occupancy validation.
- [ ] Reject invalid stay ranges, negative projected totals, duplicate room IDs, and price-vector horizon misses through explicit `SolverError` variants.
- [ ] Run solver tests, clippy, and coverage gates.

### Task 3: Multi-room public query orchestration

**Files:**
- Modify: `crates/veyra-query/Cargo.toml`
- Modify: `crates/veyra-query/src/lib.rs`
- Test: `crates/veyra-query/tests/multi_room.rs`

**Interfaces:**
- Consumes: availability, restrictions, `RoomDocument`, `solve_priced`.
- Produces: `MultiRoomStayQuery`, `MultiRoomSearchHit`, `MultiRoomQueryExplain`, `MultiRoomSearchResult`, and `SearchEngine::search_multi_room`.

- [ ] Add a failing two-family regression where no single room is valid but one property has a valid two-room solution.
- [ ] Verify the test fails because multi-room query orchestration is absent.
- [ ] Group candidate rooms by `property_id`; never allow a solver allocation to span properties.
- [ ] Apply availability, destination, and stay restrictions before invoking the solver.
- [ ] Use `solve_priced` so price reflects the actual party split per room.
- [ ] Apply budget after exact allocation price is known.
- [ ] Return deterministic ordering and bounded `limit` results.
- [ ] Return explicit counters for properties considered, properties solved, solver state budget failures, and returned results.
- [ ] Fail closed when a property's candidate room set exceeds the exact solver's provable room bound; never silently truncate candidates.
- [ ] Run query tests, full workspace tests, clippy, and coverage gates.

### Task 4: Solver search pruning without semantic shortcuts

**Files:**
- Modify: `crates/veyra-solver/src/lib.rs`
- Test: `crates/veyra-solver/tests/pruning.rs`

**Interfaces:**
- Consumes: rooming intents and partial assignments.
- Produces: the same exact solution set with fewer explored states.

- [ ] Add a regression asserting a hard-separate partial assignment is rejected before a complete leaf.
- [ ] Verify existing exhaustive search explores the larger state count.
- [ ] Check hard SameRoom/SeparateRoom/SameFloor/SameBuilding constraints as soon as both endpoints are assigned.
- [ ] Preserve deterministic enumeration and exact optimum proof.
- [ ] Add property tests comparing pruned vs exhaustive reference solutions for small generated cases.
- [ ] Run solver property tests and Miri-compatible core checks.

### Task 5: Explicit room topology relations

**Files:**
- Modify: `crates/veyra-solver/src/lib.rs`
- Modify: `crates/veyra-query/src/lib.rs`
- Test: `crates/veyra-solver/tests/topology.rs`

**Interfaces:**
- Consumes: explicit projected room-relation edges.
- Produces: provable `Near`, `AdjacentRooms`, and `ConnectedRooms` semantics.

- [ ] Add failing tests showing topology relations are accepted only when an explicit symmetric relation edge exists.
- [ ] Add a bounded `RoomRelationIndex` keyed by room IDs with separate near/adjacent/connected edges.
- [ ] Validate duplicate/asymmetric/unknown room edges at construction time.
- [ ] Evaluate hard and soft topology intents through the relation index.
- [ ] Preserve fail-closed behavior when topology data is absent.
- [ ] Wire the relation projection through multi-room query documents.
- [ ] Run solver/query tests and coverage.

### Task 6: Runtime query snapshot and service boundary

**Files:**
- Modify: `crates/veyra-runtime/Cargo.toml`
- Modify: `crates/veyra-runtime/src/lib.rs`
- Modify: `crates/veyra-server/Cargo.toml`
- Modify: `crates/veyra-server/src/lib.rs`
- Test: `crates/veyra-runtime/tests/query_snapshot.rs`
- Test: `crates/veyra-server/tests/query_boundary.rs`

**Interfaces:**
- Consumes: immutable `SearchEngine` projection plus runtime health metadata.
- Produces: atomically published query-ready snapshots and a bounded typed request handler that refuses queries unless the snapshot is Ready.

- [ ] Add a failing test proving a not-ready runtime cannot execute a query.
- [ ] Publish search engine + generation metadata atomically through `ArcSwap`.
- [ ] Add a typed server handler that maps runtime/query errors to stable machine-readable error codes.
- [ ] Enforce request/body/party/limit bounds before expensive solver work.
- [ ] Keep health endpoints independent of query success.
- [ ] Run runtime/server tests and full CI.

### Task 7: Truthful documentation and release qualification

**Files:**
- Modify: `README.md`
- Modify: `docs/invariants/core.md`
- Create: `docs/ci-checkpoints/2026-08-17-v1-production-completion.md`

**Interfaces:**
- Consumes: verified CI evidence.
- Produces: documentation that matches executable behavior and an auditable qualification record.

- [ ] Remove stale statements claiming implemented crates do not exist.
- [ ] Mark only invariants proven by executable tests as enforced.
- [ ] Document exact unsupported semantics and bounds.
- [ ] Record final SHA and each required CI job conclusion.
- [ ] Mark PR ready for review only after every required job is green.
