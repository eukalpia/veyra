# Core invariant register

This file is the source-of-truth register for semantic correctness invariants. An
invariant is not considered implemented merely because it appears in this document.

| ID | Invariant | Current executable enforcement |
|---|---|---|
| I1 | Returned stay is available for every required night. | Milestone 3 |
| I2 | No returned room violates occupancy constraints. | Milestone 4 |
| I3 | Every minor satisfies applicable guardian/adult requirements. | Milestone 4/7 |
| I4 | Pricing uses exactly the projected pricing model version. | Milestone 5 |
| I5 | CTA/CTD/min/max-stay restrictions are satisfied. | Milestone 5 |
| I6 | Published generation passed all integrity validation. | Milestone 2 |
| I7 | `published_lsn <= applied_lsn <= durable_lsn <= received_lsn`. | `ProjectionProgress::try_new` + unit/property tests |
| I8 | Partially applied PostgreSQL transactions are never query-visible. | Milestone 1 |
| I9 | Replaying one CDC transaction twice does not change final state twice. | Milestone 1 |
| I10 | Corrupt data is never served. | Runtime fail-closed state now; storage proof in Milestone 2 |
| I11 | Veyra cannot mutate authoritative Booking Asia state. | Architectural API boundary; mutation API is absent |
| I12 | Unknown semantics fail closed. | `CannotProveReason::UnsupportedSemantics`; rule/query proof in later milestones |
| I13 | Hard constraints always dominate ranking/preferences. | Milestone 9 |
| I14 | Query limits and memory use are bounded. | Protocol/query milestones |

## Executable foundation invariant

The currently executable LSN invariant is:

```text
published <= applied <= durable <= received
```

Construction of invalid `ProjectionProgress` values is rejected. Query admission also
rejects a `min_lsn` newer than the applied LSN instead of pretending read-your-writes.

## Review rule

Every milestone that moves a row from “future” to “implemented” must add at least one
runtime assertion, unit test, property test, differential test, fuzz target, or
integration test that would fail if the invariant were violated.
