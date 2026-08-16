# Veyra production qualification and CDC bootstrap plan

**Date:** 2026-08-17  
**Branch:** `agent/m0-foundation-ci`  
**Scope:** finish the current correctness/coverage gate, then implement the next vertical CDC bootstrap slice without weakening fail-closed semantics.

## Goal

Produce a reviewable Veyra slice that:

1. keeps the full five-target portable CI matrix green;
2. enforces at least 99% production line and function coverage and closes compiler-region gaps with real tests or simpler proven code rather than hidden exclusions;
3. preserves transaction-boundary durability and duplicate replay safety;
4. introduces a typed initial-snapshot/bootstrap state machine that cannot publish partial or unverifiable projections;
5. is ready for a real PostgreSQL logical-replication integration suite.

## Invariants

- PostgreSQL remains authoritative.
- No WAL acknowledgement before durable journal, successful projection apply and durable applied checkpoint.
- No partially applied PostgreSQL transaction is query-visible.
- Unknown snapshot, schema, LSN or recovery state fails closed.
- Bootstrap generation publication is atomic.
- Snapshot rows and WAL catch-up are associated with one explicit snapshot LSN.
- Duplicate WAL delivery is idempotent.
- Every queue, batch and state transition is bounded.
- The external PostgreSQL transport remains isolated from the deterministic state machine.

## Task 1 — qualify the current deterministic core

- Read the LLVM JSON artifact from the exact branch SHA.
- Add behavioral tests for currently uncovered public success and rejection paths.
- Add corruption fixtures for journal, checkpoint and segment formats.
- Remove redundant impossible fallible conversions only when an earlier bound proves infallibility.
- Keep I/O and transport error paths covered by explicit fault tests; do not disguise them as semantic-core coverage.
- Run format, strict Clippy, full tests, Nextest, Rustdoc, Miri, fuzz smoke, dependency audit and the five-platform matrix.

## Task 2 — CDC bootstrap types and transition model

Add a small typed bootstrap module to `veyra-cdc`:

- `BootstrapPhase`: `Empty`, `Snapshotting`, `ReplayingWal`, `Validating`, `Ready`, `Failed`.
- `SnapshotDescriptor`: snapshot identifier, consistent point LSN, schema/projection versions and bounded table set.
- `BootstrapProgress`: rows read, durable/applied/replayed LSN and generation candidate.
- validated transitions that reject regression, publication before validation, mismatched snapshot identity and WAL gaps;
- deterministic error surface suitable for Phoenix fallback and observability.

Write tests first for every accepted and rejected transition.

## Task 3 — snapshot sink contract

Define a transport-independent snapshot sink contract:

- begin one snapshot at one consistent LSN;
- append bounded typed rows in deterministic table/key order;
- finish only after all declared tables complete;
- abort leaves no publishable generation;
- replay begins strictly after the snapshot LSN;
- duplicate snapshot rows are either proven identical or rejected.

Keep PostgreSQL networking outside the contract.

## Task 4 — real PostgreSQL integration harness

Add an opt-in CI/integration workflow using PostgreSQL logical replication:

- publication and slot setup;
- exported snapshot capture;
- concurrent writes during snapshot;
- WAL catch-up from the recorded LSN;
- random process termination before/after journal fsync and checkpoint fsync;
- restart, duplicate delivery and gap detection;
- exact comparison against a reference projection.

The suite must not be required on unsupported local environments, but it must be required before production release qualification.

## Task 5 — documentation and PR truthfulness

- update CDC durability and bootstrap ADRs;
- map executable tests to invariants I6–I10 and I12–I14;
- update the PR body to describe what exists and what remains;
- do not claim production readiness until the exact production acceptance workflow is green.

## Verification commands

```text
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-features --locked
cargo nextest run --workspace --all-features --locked
cargo doc --workspace --all-features --no-deps --locked
cargo +nightly miri test -p veyra-types --lib --locked
cargo +nightly fuzz run projection_progress --manifest-path fuzz/Cargo.toml -- -runs=512
cargo deny check
cargo audit --deny warnings
cargo +nightly llvm-cov --workspace --all-features --lib --tests --locked
```

Completion means the exact branch SHA has a fully green required matrix and the bootstrap state machine has executable fail-closed transition tests. It does not mean the entire future Veyra roadmap is complete.
