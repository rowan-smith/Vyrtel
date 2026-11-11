# Benchmarks

Benchmarks are run by hand on fixed hardware, following the [benchmark method](benchmark-method.md)
so that anyone can repeat them. Results are added to this page once a run is complete and reviewed.

<!--
To publish a run, fill in each section below and remove the notice above. Write "not run" for
anything that was not measured; never estimate. Keep earlier releases in the history table so
changes between versions stay visible.
-->

## Setup

| | Primary (runs Vyrtel) | Secondary (load only) |
|---|---|---|
| Hardware | Intel Xeon E3-1245 v2, 4 cores / 8 threads (AVX, no AVX2), 32 GB RAM, 2 × 480 GB Intel SATA SSD in md RAID 1, ext4 | not run (no secondary) |
| Operating system | Ubuntu 26.04.1 LTS, kernel 7.0.0-42 | not run |
| Build | v0.1.2-rc.1 (`3f18168`): image `ghcr.io/rowan-smith/vyrtel@sha256:5a60c431d46aca5a…` and the binary inside it, SHA-256 `8eb4a78383db2206…`. Previous: v0.1.1 release asset `vyrtel-linux-x86_64`, SHA-256 `21e1f5fe88bd08e4…`. Both verified against their `SHA256SUMS` or digest | not run |
| Network | Loopback only | not run |
| Configuration | Baseline: `dotnet/Vyrtel.Baseline/baseline.toml` without `server.shutdown_timeout`, `ingest.max_concurrent` and `query.max_concurrent`, which v0.1.1 rejects; the same file for both versions. Transport: defaults, normal configuration | |
| Workload | Baseline: seed 42; 100,000 logs, 1,000 traces and 100 metric points in batches of 500; 13 queries, each cold then 20× warm; 20 s mixed load with 4 workers; 5 runs per version. Transport (rc.1 only): 60 s steady, 180 s flood of 1,000-connection waves, 30 min with a wave every 60 s; 1 run each | |

## Results

Local results are measured on the primary alone; remote results have load sent from the secondary.
They are never mixed.

| Metric | Previous | This release | Verdict |
|---|---:|---:|---|
| Local ingest throughput (events/s) | 16,847 (v0.1.1) | 17,054 | PASS (−1.2%, budget 15%) |
| Local query p95 (ms) | 73.6 (v0.1.1) | 71.1 | PASS (`level = Error` under mixed load; −3.4%, budget 15%) |
| Local peak memory (MiB) | 82.3 (v0.1.1) | 124.8 | **FAIL** (+51.7%, budget 25%) |
| Local compression ratio | 4.00× (v0.1.1) | 4.00× | PASS (73.1 bytes per event) |
| Remote ingest throughput (events/s) | not run | not run | INCOMPLETE |
| Remote ingest p95 / p99 (ms) | not run | not run | INCOMPLETE |
| Soak (hours, data loss, memory trend) | not run | not run | INCOMPLETE |

**Verdict: FAIL.** Of the 42 gated baseline metrics, 2 regressed beyond their budget: peak memory
(above) and restart to healthy, 52.3 → 103.2 ms (+97.3%, budget 15%). The other 40 are within
budget or better (NDJSON ingest p50 −15.2%). Same-host transport for rc.1: ingest acknowledgements
p50 / p95 / p99 of 1.72 / 2.06 / 2.61 ms at 14,613 events/s steady, no transport errors, every
server shutdown clean (exit 0, under 1 s) and RSS growth of 0.22 MiB/min over the last 20 minutes
of the 30-minute run (limit 1.0); it misses two [transport budgets](../scripts/bench/transport-budgets.md),
peak server file descriptors (453, limit 448) and descriptors left open after load (25 and 141,
limit 21).

**Integrity:** offered 100,000 logs, 3,000 spans and 100 metric points in each of 10 baseline runs;
acknowledged and stored matched exactly in every run, and no mixed-load request was rejected.
Transport acknowledged 907,800, 905,600 and 13,614,800 events; stored counts were not read (see
limitations). Smoke tests on the rc.1 image: 20 of 20 API checks passed, including events
persisting across a restart and ingestion staying protected after the last key was revoked.
Unexplained errors or data loss: none.
**Limitations and untested areas:** the published rc.1 `vyrtel-linux-x86_64` does not start on
this CPU: it exits with `SIGILL` on an AVX2 instruction and also needs glibc 2.38, so the local
baseline measures the binary from the rc.1 image instead. There is no secondary, so remote ingest,
mixed query load and the 48–72 hour soak were not run. The transport load tool does not sign in,
so its queries were refused and stored events were not reconciled with acknowledged ones; the
constrained configuration and repeat runs were not run. The baseline harness keeps its data in
`/tmp`, which is tmpfs on this host, so storage was measured in memory rather than on the SSDs, for
both versions alike. The UI was checked only for being served. The baseline configuration leaves
query concurrency at each version's default.

## History

| Version | Date | Verdict | Local ingest | Remote ingest | Local query p95 | Notes |
|---|---|---|---|---|---|---|
| 0.1.2-rc.2 candidate (`1332e29`) | 2026-11-11 | FAIL (transport); baseline PASS with one accepted regression | 17,073 events/s | not run | 72.0 ms | Pre-tag, built as the release workflow builds it (glibc 2.34, starts on a baseline x86-64 CPU). Baseline only, 5 runs against v0.1.1: peak memory +30.2% (budget 40%) after password checks were limited to one at a time; restart to healthy 52.3 → 103.1 ms (+97.2%, budget 15%), **accepted**: the Argon2id check of the configured admin password at startup, the cost of always-on sign-in. Transport was not re-run after the fix; at `bd25064` it still missed the file-descriptor budgets |
| v0.1.2-rc.1 | 2026-11-10 | FAIL | 17,054 events/s | not run | 71.1 ms | Peak memory +52% and restart to healthy +97% against v0.1.1; release asset needs AVX2 and glibc 2.38; transport file-descriptor budgets missed |
| v0.1.1 | 2026-11-10 | not judged | 16,847 events/s | not run | 73.6 ms | Measured on this host as the previous release for v0.1.2-rc.1; no earlier release to compare with |

**Accepted regressions** are measured, reviewed and kept on purpose; they are not budget passes. The
next release compares against the version that accepted them.

| Accepted in | Metric | Change | Reason |
|---|---|---|---|
| 0.1.2-rc.2 | Restart to healthy | 52.3 → 103.1 ms (+97.2%) | Each start checks the configured `auth.admin_password` with Argon2id before it reports ready, so a changed password signs out the admin's sessions before any request is served |

Numbers depend on hardware and data shape. Compare them between versions, not with your own
machine.
