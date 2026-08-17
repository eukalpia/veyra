# Production region matrix checkpoint

This checkpoint records the behavioral coverage expansion verified before the next normal PR CI run.

The added integration matrices cover:

- pricing overflow independently at base accumulation, adult and child multipliers, occupancy adjustment addition, stay multiplication, final total addition, and negative projected totals;
- each compiled stay-bound rejection predicate and CTA/CTD boundaries;
- segment typed I/O conversion, error sources, missing-file handling, and the full public semantic error surface;
- CDC recovery ordering around older, exact, future and absent durable checkpoints, fingerprint mismatch, monotonic live progress, and typed error provenance.

Before these tests were committed, a dedicated isolated job ran:

```text
cargo fmt --all
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-features --locked
```

and completed successfully. Temporary verification workflows were removed in the same commit. The normal pull-request CI remains the source of truth for the five portable targets, security gates, Miri, fuzz smoke, Nextest, Rustdoc and the hard 99/99/99 production coverage threshold.
