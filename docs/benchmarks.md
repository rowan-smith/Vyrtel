# Benchmarks

Benchmarks are run by hand on fixed hardware, following the [benchmark method](benchmark-method.md)
so that anyone can repeat them. This page compares the most recent measured version with the
previous release; each version has its own page with every metric of every test:

- [0.1.2-rc.4](benchmarks/0.1.2-rc.4.md)
- [0.1.2-rc.3](benchmarks/0.1.2-rc.3.md)
- [0.1.2-rc.2](benchmarks/0.1.2-rc.2.md)
- [0.1.2-rc.1](benchmarks/0.1.2-rc.1.md)
- [0.1.1](benchmarks/0.1.1.md)

<!--
To publish a run: add docs/benchmarks/<version>.md with every test's metrics (write "not run" for
anything not measured; never estimate), list it in docs/build.mjs under Benchmarks, then update the
comparison below to the newest version and add a row to the history table.
-->

## Setup

| | Primary (runs Vyrtel) | Secondary (load only) |
|---|---|---|
| Hardware | Intel Xeon E3-1245 v2, 4 cores / 8 threads (AVX, no AVX2), 32 GB RAM, 2 × 480 GB Intel SATA SSD in md RAID 1, ext4 | not run (no secondary) |
| Operating system | Ubuntu 26.04.1 LTS, kernel 7.0.0-42 | not run |
| Build | 0.1.2-rc.4: the published release asset `vyrtel-linux-x86_64` (smoke tests on the published image). 0.1.1: the published release asset. See each version's page for hashes | not run |
| Network | Loopback only | not run |
| Configuration | Baseline: `baseline.toml` without three keys 0.1.1 rejects, the same file for both versions. Transport: defaults, and the constrained configuration for one flood run | |
| Workload | Baseline: seed 42; 100,000 logs, 1,000 traces, 100 metric points; 13 queries cold and warm; 20 s mixed load with 4 workers; 5 runs per version. Transport: 60 s steady, 180 s flood (normal and constrained), 30 min with waves; 1 run each | |

## Results: 0.1.2-rc.4 against 0.1.1

Both versions were measured in the same session on 2026-11-15, with the same harness and
configuration. Local results are measured on the primary alone; remote results have load sent from
the secondary. They are never mixed.

| Test | Metric | 0.1.1 | 0.1.2-rc.4 | Change | Verdict |
|---|---|---:|---:|---:|---|
| Published artifacts | Checksum, glibc ≤ 2.34, starts on baseline x86-64 and on this non-AVX2 CPU | not run | all pass | | PASS |
| Smoke | API checks on a fresh install | not run | 20 / 20 | | PASS |
| Local baseline | Ingest under mixed load (events/s) | 16,734 | 17,054 | +1.9% | PASS (budget 15%) |
| Local baseline | Seeding throughput (items/s) | 38,642 | 33,724 | −12.7% | not gated |
| Local baseline | Query p95, `level = Error` under mixed load (ms) | 74.4 | 72.5 | −2.6% | PASS (budget 15%) |
| Local baseline | Peak memory (MiB) | 84.5 | 112.2 | +32.8% | **FAIL** (budget 15%); acceptance open |
| Local baseline | Restart to healthy (ms) | 52.3 | 103.1 | +97.0% | accepted in 0.1.2-rc.2 |
| Local baseline | Compression ratio | 4.00× | 4.00× | 0% | PASS |
| Local baseline | Stored bytes per event | 73.2 | 73.1 | −0.1% | PASS (budget 2%) |
| Local baseline | Gated metrics within budget or better | | 40 of 42 | | |
| Same-host transport | Steady ingest (events/s) / ack p99 (ms) | not run | 14,937 / 2.67 | | PASS |
| Same-host transport | File descriptors: peak / segment files / others after load | not run | 401 / 64 / back to idle | | PASS (448 / 64 / idle + 4) |
| Same-host transport | 503s, transport errors and unexpected statuses for steady clients | not run | 0 / 0 / 0 | | PASS |
| Same-host transport | Flood wave: accept to end / end to end / establishment p99 | not run | 60.0 s / 60.1 s / 92.5 ms | | PASS (82 s / 82 s / 5 s) |
| Same-host transport | Acknowledged vs stored events (4 runs, 16.3 million events) | not run | equal | | PASS |
| Same-host transport | Constrained configuration (256 MB, 64 connections, 1,024 FDs) | not run | every soak budget met (heap and task gates are in-process, at 200 slots) | | PASS |
| Same-host transport | Transport heap, retained heap and runtime tasks (`tests/overload`, release workflow) | not run | assertions held, values not recorded | | PASS |
| Same-host transport | Peak admitted / waiting connections, sampled | not run | 256 / 64 (constrained 64 / 64) | | PASS (`max_connections` / 64) |
| Same-host transport | Regression budgets, three steady runs against the baseline build | not run | not run | | INCOMPLETE |
| Remote | Ingest throughput, p95 / p99 | not run | not run | | INCOMPLETE |
| Soak | 48–72 hours, data loss, memory trend | not run | not run | | INCOMPLETE |

**Verdict for 0.1.2-rc.4: FAIL on peak memory alone.** Every measured transport budget passes,
the constrained configuration included, and the flood-wave deadline that 0.1.2-rc.3 failed is
met: with a listen backlog of 2,048 the kernel dropped no handshakes, and the slowest connection
of a wave is the server's own 60 s body timeout. Stored events now match acknowledged events in
every transport run. In the local baseline, restart to healthy is the accepted regression from
0.1.2-rc.2, and peak memory is over its budget. The transport regression comparison, remote load
and the sustained soak remain unvalidated.

- **Peak memory, +32.8% (84.5 → 112.2 MiB).** Nothing in 0.1.2-rc.4 changed memory: its peak is
  within run-to-run noise of 0.1.2-rc.3's (114.7 MiB). The percentage differs from 0.1.2-rc.3's
  +44.0% because 0.1.1 peaked higher in this session (84.5 against 79.6 MiB), and the budget
  differs (15% against 30%) because each session derives it from 0.1.1's run-to-run variance.
  Argon2id is a known contributor: each password check allocates 19 MiB, and checks run one at a
  time since 0.1.2-rc.2. The increase that remains has not been profiled. Open: accept it as a
  documented trade-off once profiled, or reduce it.

Details, and the full metric tables, are on the [0.1.2-rc.4 page](benchmarks/0.1.2-rc.4.md).

**Integrity:** in every one of the 10 baseline runs, the seeded dataset (100,000 logs, 3,000 spans
and 100 metric points) was offered, acknowledged and stored exactly, and every mixed-load request
was accepted; mixed-load ingest is counted, not reconciled against storage. In transport, the
stored count of `connload`'s events equalled the acknowledged count in all four runs.
Unexplained errors or data loss: none found.
**Limitations and untested areas:** no secondary machine, so no remote load or soak. The
transport regression budgets need three steady runs of the baseline build, which were not run, so
query latency under transport load is recorded without a verdict. The baseline's data directories
were on tmpfs. 0.1.1 had no smoke or transport run.

## History

| Version | Date | Verdict | Local ingest | Remote ingest | Local query p95 | Notes |
|---|---|---|---|---|---|---|
| [0.1.2-rc.4](benchmarks/0.1.2-rc.4.md) | 2026-11-15 | FAIL: peak memory open, validation incomplete | 17,054 events/s | not run | 72.5 ms | Flood waves end in 60.1 s (listen backlog 2,048, no dropped handshakes); stored events reconciled in transport; constrained configuration passes; peak memory +32.8% (budget 15%) |
| [0.1.2-rc.3](benchmarks/0.1.2-rc.3.md) | 2026-11-15 | FAIL: peak memory open, flood deadline failed, validation incomplete | 17,064 events/s | not run | 72.0 ms | Published asset fixed (glibc 2.34, no AVX2 needed); file descriptors bounded; peak memory +44% (budget 30%); longest flood wave 91.1 s (budget 82 s) |
| [0.1.2-rc.2](benchmarks/0.1.2-rc.2.md) | 2026-11-11 | FAIL (transport); baseline PASS with one accepted regression | 17,073 events/s | not run | 72.0 ms | Published asset fixed (glibc 2.34, no AVX2 needed). Peak memory +30.2% (budget 40%) after password checks were limited to one at a time; restart to healthy +97.2%, **accepted** (below); file-descriptor budgets and one 503 missed in transport. Measured while it was prepared: baseline of its exact code, smoke and transport of `bd25064` |
| [0.1.2-rc.1](benchmarks/0.1.2-rc.1.md) | 2026-11-10 | FAIL | 17,054 events/s | not run | 71.1 ms | Peak memory +52% and restart to healthy +97% against 0.1.1; release asset needs AVX2 and glibc 2.38; transport file-descriptor budgets missed |
| [0.1.1](benchmarks/0.1.1.md) | 2026-11-10 | not judged | 16,847 events/s | not run | 73.6 ms | Measured as the previous release for 0.1.2-rc.1; no earlier release to compare with. Measured again on 2026-11-15 for 0.1.2-rc.3 (16,866 events/s, 75.3 ms) and for 0.1.2-rc.4 (16,734 events/s, 74.4 ms) |

**Accepted regressions** are measured, reviewed and kept on purpose; they are not budget passes. The
next release compares against the version that accepted them.

| Accepted in | Metric | Change | Reason |
|---|---|---|---|
| 0.1.2-rc.2 | Restart to healthy | 52.3 → 103.1 ms (+97.2%) | Each start checks the configured `auth.admin_password` with Argon2id before it reports ready, so a changed password signs out the admin's sessions before any request is served |

Numbers depend on hardware and data shape. Compare them between versions, not with your own
machine.
