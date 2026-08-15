# CDC benchmark policy

Milestone 1 intentionally makes no throughput or freshness claim before reproducible measurements exist.

The first CDC benchmark corpus must measure:

- single-row and multi-row transaction ingest;
- 1 KiB / 64 KiB / 1 MiB transaction payloads;
- synchronous fsync latency and throughput;
- restart replay with 1k / 100k durable transactions;
- duplicate replay lookup cost;
- logical replication freshness P50/P95/P99;
- CPU and allocations per committed transaction.

Record PostgreSQL version, storage medium, filesystem, OS, CPU architecture, Rust version, Veyra commit SHA and exact command line with every result.

Performance optimization is rejected if it weakens C1–C10 in `docs/invariants/cdc.md`.
