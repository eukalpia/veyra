# Veyra V1 Production Runbook

## Deployment contract

Veyra is deployed as a rebuildable, read-optimized projection service. PostgreSQL remains authoritative. A deployment must not route search traffic until the new process has restored its durable checkpoint, caught up its projection, published an immutable generation, and entered runtime phase `Ready`.

## Required configuration

- PostgreSQL endpoint with logical replication enabled and a dedicated least-privilege replication role.
- Explicit publication and replication slot names; no implicit creation in the query-serving process.
- Durable journal/checkpoint directory on a filesystem with monitored free space and atomic rename semantics.
- Segment directory separated from ephemeral build artifacts.
- Bounded values for HTTP body size, concurrent requests, solver states, valid solutions, party size, candidate rooms, stay length, and result limit.
- Structured logs with generation, received/durable/applied/published LSNs, request correlation ID, rejection code, solver states, and query latency.

Secrets must be supplied by the deployment platform, never committed to the repository or embedded in images.

## Startup sequence

1. Validate configuration and filesystem permissions.
2. Read and validate checkpoint and journal state.
3. Recover only complete, ordered transactions.
4. Rebuild or open immutable segments.
5. Connect to PostgreSQL and resume from the durable position.
6. Catch up while readiness remains false.
7. Atomically publish one complete query generation.
8. Enter `Ready` only when all runtime invariants hold.
9. Enable traffic through the load balancer.

Any failure before step 8 leaves liveness available for diagnostics but readiness false.

## Health semantics

- **Liveness:** the process and event loop are running. Liveness must not depend on PostgreSQL being reachable for a short interval.
- **Readiness:** a complete query generation is published, runtime phase is `Ready`, and no refusal reason is active.
- **Degraded:** the process can expose diagnostics while refusing search. Degraded is never reported as ready.

Recommended alert signals:

- replication lag by `received - published` LSN distance and wall-clock age;
- duration outside `Ready`;
- checkpoint/journal write failures;
- segment publication failures;
- disk free-space and inode exhaustion;
- query rejection rate by stable code;
- solver state-budget exhaustion rate;
- p50/p95/p99 query latency and queue time;
- process restarts and recovery duration.

## Rollout

Use a canary or blue/green rollout.

1. Start the new version without traffic.
2. Wait for `Ready` and compare generation/progress telemetry.
3. Replay a fixed qualification corpus against old and new versions.
4. Compare deterministic results and explanation counters.
5. Shift a small traffic percentage.
6. Monitor error, latency, lag, CPU, memory, and disk.
7. Complete rollout only when the canary remains within thresholds.

Do not perform an in-place segment-format migration without a versioned reader or an offline rebuild plan.

## Rollback

- Stop routing new traffic to the failing version.
- Keep PostgreSQL authoritative operations unaffected.
- Start the previous binary against its compatible durable state, or rebuild its projection from the authoritative source.
- Never force a newer checkpoint into an older reader unless the format compatibility contract explicitly allows it.
- Preserve failing journal/segment files and logs for analysis before cleanup.

## Failure procedures

### PostgreSQL unavailable

Remain live but not ready if the published generation is outside the configured staleness policy. Reconnect with bounded exponential backoff and jitter. Do not advance progress locally.

### Journal or checkpoint write failure

Stop acknowledgement and publication. Report refusal reason and readiness false. Protect the filesystem evidence; do not silently recreate state over a failing path.

### Corrupt or truncated journal

Recover the last complete validated boundary. If corruption is not an expected torn tail, quarantine the file and rebuild from PostgreSQL rather than guessing.

### State-budget exhaustion

Return the stable bounded-computation error. Record party size, candidate count, configured budget, explored states, and property identifier without logging personal identity data. Investigate upstream candidate pruning or adjust capacity only after benchmarks.

### Disk pressure

Stop compaction/publication before the filesystem reaches an unsafe threshold. Alert early, retain the current query generation, and refuse operations that require additional durable space.

## Backup and disaster recovery

- PostgreSQL backup and WAL retention are authoritative.
- Checkpoint, journal, and immutable segments may be backed up to shorten recovery, with checksums and format version retained.
- Regularly rehearse restore into an isolated environment.
- Verify that a clean projection rebuild produces equivalent deterministic query outputs for a fixed corpus.

## Release gate

The exact release commit must have green results for formatting, locked dependency resolution, all-target clippy, workspace tests, nextest, >=99% lines/functions/regions on the production denominator, cargo-deny, cargo-audit, rustdoc, Miri, fuzz smoke, cross-platform builds/tests, serialized-invariant regressions, and release-mode build. A report from another SHA is not transferable evidence.
