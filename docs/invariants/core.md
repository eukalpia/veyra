# Core invariant register

This file is the source-of-truth register for semantic correctness invariants. An invariant is considered enforced only when executable code/tests can fail if the invariant is violated. Documentation alone is never proof.

| ID | Invariant | Current executable enforcement |
|---|---|---|
| I1 | A returned stay is available for every required night. | `veyra-availability` stay intersection + `veyra-query` tests |
| I2 | No returned room violates compiled occupancy constraints. | `veyra-occupancy::validate_room`; single/multi-room query + solver tests |
| I3 | Applicable minor/guardian/adult requirements are evaluated from the actual room assignment. | party guardian graph + occupancy validation + solver tests |
| I4 | Money uses fixed-point integer micros; multi-room pricing uses the actual adult/child occupancy selected by the solver. | `MoneyMicros`, `PriceVector::quote`, `solve_priced`; `priced_solver.rs` |
| I5 | CTA/CTD/min/max-stay restrictions are checked before a room participates in search. | `CompiledRestrictions::validate_stay` + query tests |
| I6 | Query-visible runtime status and its immutable payload belong to the same atomic publication. | `PublishedGeneration<T>` + `GenerationState<T>` + runtime generation tests |
| I7 | `published_lsn <= applied_lsn <= durable_lsn <= received_lsn`. | `ProjectionProgress::try_new`, validated serde conversion, unit/property/serde tests |
| I8 | Partially applied PostgreSQL transactions are not projected as committed query state. | CDC transaction/projection boundary tests |
| I9 | Replayed CDC/projection progress cannot move logical progress backwards. | typed LSN/projection ordering + CDC/projection tests |
| I10 | Corrupt/unready/stale generations are never admitted as queryable. | runtime fail-closed states + `prove_queryable` / `GenerationState::admit` tests |
| I11 | Veyra cannot mutate authoritative Booking Asia booking/inventory/payment state. | architectural API boundary; no mutation API exists in query/runtime/server surfaces |
| I12 | Unknown or unprovable semantics fail closed. | query topology validation, solver unsupported-semantics errors, runtime `CannotProveReason` |
| I13 | Hard constraints dominate ranking/preferences. | exact solver validates hard rooming/occupancy constraints before selecting solution profiles; regression tests |
| I14 | Exact multi-room search is bounded and never silently truncates a property's candidate rooms. | `HARD_MAX_TRAVELERS = 16`, `HARD_MAX_ROOMS = 8`, `SolverConfig`, `QueryError::SolverRoomLimit` |
| I15 | A multi-room allocation never spans different properties. | `SearchEngine::search_multi_room` groups solver inputs by `property_id`; query regression tests |
| I16 | A used room's final projected price is computed after assignment from its real adult/child counts. | `PricedRoomOffer` + `solve_priced`; occupancy-pricing regression |
| I17 | Already-provably-impossible hard partial assignments are pruned without changing exact semantics. | `partial_hard_constraints_hold`; pruning regression verifies reduced explored-state count and same valid optimum |
| I18 | `Near`, `AdjacentRooms`, and `ConnectedRooms` are never guessed from room IDs/numbers. | `RoomRelationIndex` + topology tests |
| I19 | Spatial relations use a complete explicit room projection when required. | `RoomSpatialProjection` density/count checks + spatial multi-room tests |
| I20 | Read-your-writes admission requires `applied_lsn >= minimum_lsn`. | `RuntimeSnapshot::prove_queryable` + `GenerationState::admit` tests |
| I21 | The typed server query boundary can obtain a `SearchEngine` only from an admitted atomic runtime generation. | `QueryService` + server query-boundary tests |
| I22 | Operations health remains independent of business-query success. | Axum liveness/readiness handlers remain separate from `QueryService` |
| I23 | Untrusted serialized values cannot bypass constructor invariants for projection progress, CDC transaction boundaries, or runtime phase/reason coherence. | serde `try_from` wire types for `ProjectionProgress`, `TransactionBatch`, and `RuntimeSnapshot`; positive and negative serde regressions |

## Ordering and read-your-writes proof

The executable projection ordering invariant is:

```text
published <= applied <= durable <= received
```

Construction and deserialization of invalid `ProjectionProgress` values are rejected. Query admission also rejects a caller's `minimum_lsn` when it is newer than the applied projection instead of pretending read-your-writes consistency.

A complete decoded CDC transaction additionally satisfies:

```text
final_lsn <= commit_lsn <= end_lsn
```

The same validation path is used for constructor calls and deserialization, so a JSON or persisted representation cannot create a transaction state that normal code could not construct.

## Serialized trust boundary

Serde input is untrusted input. Types with cross-field invariants deserialize into private wire structures first and then enter the same validated construction path used by normal code.

The enforced runtime phase/reason combinations are:

```text
Ready      => cannot_prove is None
Starting   => cannot_prove is Some(...)
Degraded   => cannot_prove is Some(...)
Failed     => cannot_prove is Some(...)
```

This prevents a serialized `Ready` snapshot from carrying a hidden refusal reason and prevents a non-ready snapshot from accidentally appearing queryable because its fail-closed reason is absent.

## Exact multi-room bounds

The exact solver intentionally has hard proof bounds:

```text
max travelers:           16
max candidate rooms:      8
SolverConfig default:
  max_states:         50_000
  max_solutions:       2_000
```

These bounds are correctness boundaries, not performance hints. If one property's post-filter candidate set exceeds the exact room bound, the public multi-room query returns an explicit error. It does **not** take the first eight rooms, rank a subset, or otherwise hide an unknown optimum.

A custom `SolverConfig` can reduce or adjust state/solution budgets within the hard structural bounds. Exhausting the configured proof budget is a query failure, not an empty-result success.

## Pricing invariant

Single-room and multi-room pricing share fixed-point monetary primitives, but multi-room pricing has an additional ordering requirement:

```text
assign travelers -> validate occupancy -> obtain adult/child counts -> quote room -> sum allocation
```

The solver does not choose an allocation using a static room price and then retrofit child/adult surcharges afterward. This matters for families because moving one child between rooms can change which allocation is actually cheapest.

Projected prices remain advisory search values. Booking Asia/PostgreSQL must revalidate price and inventory during authoritative checkout.

## Spatial/topology proof model

`SameRoom` and `SeparateRoom` do not require spatial metadata. Other room-location semantics require `RoomSpatialProjection`.

The projection contains dense room placements (`floor`, `building`) plus explicit symmetric edges. The semantics are:

```text
SameFloor       => projected floor values are equal
SameBuilding    => projected building values are equal
Near            => same room OR explicit Near edge
AdjacentRooms   => distinct rooms AND explicit Adjacent edge
ConnectedRooms  => distinct rooms AND explicit Connected edge
```

For a supplied complete topology index, a missing edge means that relation is false. If the topology projection itself is unavailable, Veyra cannot prove the relation and fails closed.

Room numbers, room IDs, lexical names, or numeric closeness are never interpreted as topology.

## Atomic generation invariant

A runtime publication is one `PublishedGeneration<T>` containing both:

- `RuntimeSnapshot` metadata;
- the immutable `Arc<T>` payload for that exact generation when `Ready`.

Construction rejects:

- `Ready` metadata without a payload;
- a payload attached to a non-`Ready` phase.

`GenerationState<T>` swaps the whole publication through `ArcSwap`. Readers therefore cannot observe status from one generation and query payload from another.

## Authority boundary

Veyra is a search/proof projection, not a transactional authority. A successful Veyra result must never be interpreted as permission to mutate inventory or as a checkout guarantee. Booking Asia/PostgreSQL remains responsible for authoritative inventory, booking, payment, and final price validation.

## Review rule

Any change that weakens, expands, or adds an invariant must add or update at least one runtime assertion, unit test, property test, differential test, fuzz target, or integration test that would fail if the invariant were violated. CI/documentation must never be used as a substitute for executable proof.
