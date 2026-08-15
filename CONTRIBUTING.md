# Contributing to Veyra

Veyra optimizes for correctness before speed. A benchmark improvement that weakens a
semantic invariant is a regression.

## Change discipline

1. Keep changes vertically scoped and reviewable.
2. Add or update the failing correctness test with behavior changes.
3. Preserve deterministic ordering and bounded work.
4. Do not add arbitrary runtime scripting.
5. Do not add unsafe code outside a future dedicated `veyra-unsafe` crate.
6. Do not add a dependency merely for convenience; justify its operational value.
7. Keep architecture and invariant documentation synchronized with code.
8. Keep `Cargo.lock` committed.
9. Never reduce coverage or disable a failing gate to make CI green.
10. Never describe an unmeasured performance claim as fact.

## Required checks

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features --locked
cargo nextest run --workspace --all-features
cargo llvm-cov --workspace --all-features --lib --fail-under-lines 99
cargo deny check
cargo audit
```

Miri and fuzz smoke are executed by CI on the crates/targets where they are meaningful.

## Pull requests

Describe:

- the invariant or capability changed;
- failure behavior;
- tests added;
- benchmark evidence for performance-sensitive work;
- platform impact;
- compatibility-version impact.
