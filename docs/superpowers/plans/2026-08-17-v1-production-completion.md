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
- [ ] Run the final complete CI matrix and record the clean qualification SHA.

### Task 2: Occupancy-dependent solver pricing

**Files:**
- Modify: `crates/veyra-solver/src/lib.rs`
- Test: `crates/veyra-solver/tests/priced_solver.rs`

**Interfaces:**
- Consumes: `PriceVector::quote(check_in_day, check_out_day, adults, children, OccupancyAdjustment)` and `OccupancyReport` from `validate_room`.
- Produces: `PricedRoomOffer` and `solve_priced(...)`, while preserving existing `RoomOffer` and `solve(...)`.

- [x] Add a failing regression where static base-room prices choose a different allocation than exact occupancy-dependent prices.
- [x] Verify the new test fails because `solve_priced` does not yet exist.
- [x] Add `PricedRoomOffer { room_id, prices, occupancy_adjustment, floor, building, adult_age, occupancy_rule }`.
- [x] Add `solve_priced(party, check_in_date, check_in_day, check_out_day, offers, config)`.
- [x] At each valid leaf, price each used room from its actual adult/child counts returned by occupancy validation.
- [x] Reject invalid stay ranges, duplicate room IDs, invalid/non-representable pricing, and price-vector horizon misses through explicit errors.
- [x] Run targeted solver tests and strict Clippy. Final repository coverage remains part of Task 7.

### Task 3: Multi-room public query orchestration

**Files:**
- Modify: `crates/veyra-query/Cargo.toml`
- Modify: `crates/veyra-query/src/lib.rs`
- Test: `crates/veyra-query/tests/multi_room.rs`

**Interfaces:**
- Consumes: availability, restrictions, `RoomDocument`, `solve_priced`.
- Produces: `MultiRoomStayQuery`, `MultiRoomSearchHit`, `MultiRoomQueryExplain`, `MultiRoomSearchResult`, and `SearchEngine::search_multi_room`.

- [x] Add a failing family regression where no single room is valid but one property has a valid multi-room solution.
- [x] Verify the test fails because multi-room query orchestration is absent.
- [x] Group candidate rooms by `property_id`; never allow a solver allocation to span properties.
- [x] Apply availability, destination, and stay restrictions before invoking the solver.
- [x] Use `solve_priced` so price reflects the actual party split per room.
- [x] Apply budget after exact allocation price is known.
- [x] Return deterministic ordering and bounded `limit` results.
- [x] Return explicit explain counters and propagate solver proof-budget exhaustion as an error instead of silently skipping a property.
- [x] Fail closed when a property's candidate room set exceeds the exact solver's provable room bound; never silently truncate candidates.
- [x] Run targeted query tests and strict Clippy. Full workspace/coverage qualification remains Task 7.

### Task 4: Solver search pruning without semantic shortcuts

**Files:**
- Modify: `crates/veyra-solver/src/lib.rs`
- Test: `crates/veyra-solver/tests/pruning.rs`
- Test: `crates/veyra-solver/tests/pruning_differential.rs`

**Interfaces:**
- Consumes: rooming intents and partial assignments.
- Produces: the same exact solution set with fewer explored states.

- [x] Add a regression where a hard same-room constraint makes partial branches provably impossible.
- [x] Verify the pre-pruning solver explores 14 states where exact early pruning needs 10.
- [x] Check hard `SameRoom`, `SeparateRoom`, `SameFloor`, `SameBuilding`, and topology-backed relations as soon as both endpoints are assigned.
- [x] Preserve deterministic enumeration and exact optimum proof.
- [ ] Differential/property test against an independent exhaustive reference is added and awaiting its targeted gate.
- [x] Run targeted solver tests and strict Clippy; project-wide Miri remains the dedicated `veyra-types` CI gate.

### Task 5: Explicit room topology relations

**Files:**
- Modify: `crates/veyra-solver/src/lib.rs`
- Create: `crates/veyra-solver/src/topology.rs`
- Modify: `crates/veyra-query/src/lib.rs`
- Test: `crates/veyra-solver/tests/topology.rs`
- Test: `crates/veyra-query/tests/spatial_multi_room.rs`

**Interfaces:**
- Consumes: explicit projected room placements and symmetric room-relation edges.
- Produces: provable `SameFloor`, `SameBuilding`, `Near`, `AdjacentRooms`, and `ConnectedRooms` semantics.

- [x] Add failing tests showing topology relations are accepted only when an explicit complete projection can prove them.
- [x] Add a bounded `RoomRelationIndex` keyed by room IDs with near/adjacent/connected edges.
- [x] Validate duplicate rooms, unknown endpoints, self-edges, and duplicate normalized edges at construction time.
- [x] Evaluate hard and soft topology intents through the relation index.
- [x] Preserve fail-closed behavior when required topology data is absent.
- [x] Add `RoomSpatialProjection` with dense floor/building placements and wire it through multi-room query candidate projection.
- [x] Run targeted solver/query tests and strict Clippy. Final coverage remains Task 7.

### Task 6: Runtime query generation and service boundary

**Files:**
- Modify: `crates/veyra-runtime/src/lib.rs`
- Modify: `crates/veyra-server/Cargo.toml`
- Modify: `crates/veyra-server/src/lib.rs`
- Test: `crates/veyra-runtime/tests/generation_state.rs`
- Test: `crates/veyra-server/tests/query_boundary.rs`

**Interfaces:**
- Consumes: immutable `SearchEngine` projection plus runtime health/progress metadata.
- Produces: atomically published query-ready generations and a typed request boundary that refuses queries unless the generation is provably queryable.

- [x] Add a failing test proving a not-ready runtime cannot execute a query.
- [x] Publish query payload + generation metadata atomically through `ArcSwap` as one `PublishedGeneration<T>`.
- [x] Reject `Ready` without payload and payload attached to a non-`Ready` phase.
- [x] Add `GenerationState::admit(minimum_lsn)` so a caller receives only the exact payload generation whose status proved safe/current enough.
- [x] Add a typed server `QueryService` for single-room and multi-room search with stable machine-readable failure codes.
- [x] Enforce party/limit/stay/solver bounds in typed query validation before expensive exact solve work; no public raw-body query protocol is frozen in V1.
- [x] Keep health endpoints independent of business-query success.
- [x] Run targeted runtime/server tests and strict Clippy. Full CI remains Task 7.

### Task 7: Truthful documentation and release qualification

**Files:**
- Modify: `README.md`
- Modify: `docs/invariants/core.md`
- Create: `docs/ci-checkpoints/2026-08-17-v1-production-completion.md`

**Interfaces:**
- Consumes: verified CI evidence.
- Produces: documentation that matches executable behavior and an auditable qualification record.

- [x] Remove stale statements claiming implemented crates do not exist.
- [x] Replace milestone placeholders in the invariant register with executable enforcement references.
- [x] Document exact solver bounds, topology proof semantics, authority boundaries, and unsupported/fail-closed cases.
- [ ] Correct README/default-budget wording after the source-of-truth check (`50,000` states / `2,000` solutions).
- [ ] Run the final full CI matrix on an otherwise frozen implementation/documentation head.
- [ ] Repair every real CI failure without lowering coverage/security/lint gates.
- [ ] Record the qualified implementation SHA and every required CI job conclusion in the checkpoint.
- [ ] Run CI once more for the documentation-only checkpoint head.
- [ ] Mark PR ready for review only after every required final job is green.
