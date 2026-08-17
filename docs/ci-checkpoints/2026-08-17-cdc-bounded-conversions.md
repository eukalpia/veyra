# CDC bounded conversion checkpoint

This checkpoint records the proof obligations behind the three journal integer conversions hardened on 2026-08-17.

- Durable record payloads are rejected above `MAX_RECORD_BYTES = 64 MiB` before converting the encoded `u64` payload length to `usize`.
- Transaction change counts are rejected above `MAX_CHANGES = 1,000,000` before converting the count to the on-disk `u32` representation.
- Optional tuple payloads are rejected above `MAX_RECORD_BYTES = 64 MiB` before converting their encoded length to `u32`.

These are local proof annotations only. Workspace Clippy remains `-D warnings`; no global lint suppression or CI threshold was weakened.

The source change was verified by the dedicated helper with:

```text
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-features --locked
```

The normal PR CI remains the source of truth for the full five-target matrix and the 99/99/99 production coverage gate.
