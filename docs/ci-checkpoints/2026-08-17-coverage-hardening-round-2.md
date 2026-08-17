# Production coverage hardening checkpoint — 2026-08-17

This checkpoint records the second production-core coverage pass without changing any CI threshold.

- Availability arithmetic now relies only on already-proven positive/bounded stay invariants.
- CDC transaction Begin/Commit boundaries have typed paths; impossible boundary outcomes were removed.
- Checkpoint and journal recovery no longer perform redundant flush/seek operations on unbuffered, freshly opened file handles.
- Real Linux durability failures are exercised through `/dev/full` and `/dev/null`.
- Journal encoding limits and transaction-stream propagation paths have dedicated regression matrices.
- Validated rule execution is infallible after validation; public validation errors remain fail-closed.
- Pricing removes redundant overflow branches only after the 90-night hard bound is proven.

The production CI gate remains >=99% independently for lines, functions, and LLVM regions. This checkpoint does not declare that gate passed; the exact-SHA CI run is authoritative.
