# ADR-0001: Foundation and process boundary

Status: Accepted  
Date: 2026-08-15

## Decision

Veyra starts as a stable-Rust Cargo workspace with independently testable crates.
Milestone 0 intentionally implements only three runtime responsibilities:

- `veyra-types`: allocation-free compatibility and replication value types;
- `veyra-runtime`: immutable fail-closed runtime publication state;
- `veyra-server`: the external process/admin HTTP boundary.

We do **not** create empty crates for future subsystems. A crate is introduced when a
milestone gives it real behavior, tests, invariants, metrics, and ownership.

## Process isolation

The Veyra engine is an independent process. It is not embedded as a giant NIF. This
keeps a native crash or allocator failure from directly taking down the BEAM.

```text
Phoenix / veyra_ex
       |
       | typed RPC (future milestone)
       v
+-------------------+
|   Veyra process   |
|                   |
| runtime snapshot  |
| query engine      |
+-------------------+
```

Milestone 0 exposes only administrative HTTP health endpoints. gRPC is introduced with
the real query protocol rather than as an empty façade.

## Publication model

Readers load one immutable `RuntimeSnapshot` through `ArcSwap`. Writers replace the
entire snapshot atomically. Runtime code does not maintain a globally mutated object
graph behind a query mutex.

A snapshot carries:

- service phase;
- immutable generation ID;
- `received/durable/applied/published` LSN progress;
- explicit reason why a result cannot be proven.

A state is queryable only when its phase is `ready`, no cannot-prove reason exists, and
its applied LSN satisfies the caller's minimum LSN.

## Fail-closed bootstrap

The process starts alive but not ready:

```text
/health/live  -> 200
/health/ready -> 503
```

Milestone 0 has no projection and therefore never fabricates readiness.

## Portability

Portable code must not require `io_uring`, `epoll`, huge pages, NUMA, AVX2, AVX-512, or
other single-platform facilities. Performance specializations may later exist behind
runtime detection or optional features after measurement.
