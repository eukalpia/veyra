# Benchmark policy

Benchmarks are evidence, not marketing.

Milestone 0 establishes Criterion as the microbenchmark harness and measures the
immutable runtime-snapshot load path. No latency claim for availability/search is made
before those engines exist.

## Rules

- Benchmark code and corpus versions are committed.
- Warm/cold measurements are labeled explicitly.
- P50/P95/P99 are reported separately.
- CPU, RAM, dataset cardinality, compiler version, target triple, and engine revision
  accompany published numbers.
- Portable builds must not use `-C target-cpu=native`.
- Comparisons against PostgreSQL, ClickHouse, Materialize, or RisingWave must use the
  same semantic workload and comparable durability/freshness settings.
- A benchmark regression never justifies weakening correctness.

Future macro workloads live under `tests/performance/` and reproduce the Simple,
Medium, Complex, and Pathological-bounded scenarios in the Veyra specification.
