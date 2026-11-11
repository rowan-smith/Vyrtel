# Benchmarks

Benchmarks are run by hand on fixed hardware, following the [benchmark method](benchmark-method.md)
so that anyone can repeat them. This page compares the two most recent measured versions; each
version has its own page with every metric of every test:

- [0.1.2-rc.1](benchmarks/0.1.2-rc.1.md)
- [0.1.1](benchmarks/0.1.1.md)

<!--
To publish a run: add docs/benchmarks/<version>.md with every test's metrics (write "not run" for
anything not measured; never estimate), list it in docs/build.mjs under Benchmarks, then update the
comparison below to the two newest versions and add a row to the history table.
-->

## Setup

| | Primary (runs Vyrtel) | Secondary (load only) |
|---|---|---|
| Hardware | Intel Xeon E3-1245 v2, 4 cores / 8 threads (AVX, no AVX2), 32 GB RAM, 2 × 480 GB Intel SATA SSD in md RAID 1, ext4 | not run (no secondary) |
| Operating system | Ubuntu 26.04.1 LTS, kernel 7.0.0-42 | not run |
| Build | 0.1.2-rc.1: the binary in its published image. 0.1.1: the published release asset. See each version's page for hashes | not run |
| Network | Loopback only | not run |
| Configuration | Baseline: `baseline.toml` without three keys 0.1.1 rejects, the same file for both versions. Transport: defaults | |
| Workload | Baseline: seed 42; 100,000 logs, 1,000 traces, 100 metric points; 13 queries cold and warm; 20 s mixed load with 4 workers; 5 runs per version. Transport: 60 s steady, 180 s flood, 30 min with waves; 1 run | |

## Results: 0.1.2-rc.1 against 0.1.1

Both versions were measured in the same session on 2026-11-10, with the same harness and
configuration. Local results are measured on the primary alone; remote results have load sent from
the secondary. They are never mixed.

| Test | Metric | 0.1.1 | 0.1.2-rc.1 | Change | Verdict |
|---|---|---:|---:|---:|---|
| Smoke | API checks on a fresh install | not run | 20 / 20 | | PASS |
| Local baseline | Ingest under mixed load (events/s) | 16,847 | 17,054 | +1.2% | PASS (budget 15%) |
| Local baseline | Seeding throughput (items/s) | 35,108 | 32,604 | −7.1% | not gated |
| Local baseline | Query p95, `level = Error` under mixed load (ms) | 73.6 | 71.1 | −3.4% | PASS (budget 15%) |
| Local baseline | Peak memory (MiB) | 82.3 | 124.8 | +51.7% | **FAIL** (budget 25%) |
| Local baseline | Restart to healthy (ms) | 52.3 | 103.2 | +97.3% | **FAIL** (budget 15%) |
| Local baseline | Compression ratio | 4.00× | 4.00× | 0% | PASS |
| Local baseline | Stored bytes per event | 73.2 | 73.1 | −0.2% | PASS (budget 2%) |
| Local baseline | Gated metrics within budget or better | | 39 of 41 | | |
| Same-host transport | Steady ingest (events/s) / ack p99 (ms) | not run | 14,613 / 2.61 | | PASS |
| Same-host transport | File descriptors: peak / after load | not run | 453 / 141 | | **FAIL** (448 / 21) |
| Same-host transport | Acknowledged vs stored events | not run | not read | | INCOMPLETE |
| Remote | Ingest throughput, p95 / p99 | not run | not run | | INCOMPLETE |
| Soak | 48–72 hours, data loss, memory trend | not run | not run | | INCOMPLETE |

**Verdict for 0.1.2-rc.1: FAIL.** Peak memory and restart to healthy regressed beyond budget, and
two transport file-descriptor budgets were missed. Its published `vyrtel-linux-x86_64` also does not
start on x86-64 CPUs without AVX2, so the image binary was measured. Details, and the full metric
tables, are on the [0.1.2-rc.1 page](benchmarks/0.1.2-rc.1.md).

**Integrity:** in every one of the 10 baseline runs, 100,000 logs, 3,000 spans and 100 metric
points were offered, acknowledged and stored exactly; no mixed-load request was rejected.
Unexplained errors or data loss: none.
**Limitations and untested areas:** no secondary machine, so no remote load or soak. The transport
load tool does not sign in, so stored events were not reconciled and query latency was not
measured. The baseline's data directories were on tmpfs. 0.1.1 had no smoke or transport run.

## History

| Version | Date | Verdict | Local ingest | Remote ingest | Local query p95 | Notes |
|---|---|---|---|---|---|---|
| 0.1.2-rc.2 candidate (`1332e29`) | 2026-11-11 | FAIL (transport); baseline PASS with one accepted regression | 17,073 events/s | not run | 72.0 ms | Pre-tag, baseline only, against 0.1.1. Peak memory +30.2% (budget 40%) after password checks were limited to one at a time; restart to healthy +97.2%, **accepted** (below). Its own page follows the check of the published release |
| [0.1.2-rc.1](benchmarks/0.1.2-rc.1.md) | 2026-11-10 | FAIL | 17,054 events/s | not run | 71.1 ms | Peak memory +52% and restart to healthy +97% against 0.1.1; release asset needs AVX2 and glibc 2.38; transport file-descriptor budgets missed |
| [0.1.1](benchmarks/0.1.1.md) | 2026-11-10 | not judged | 16,847 events/s | not run | 73.6 ms | Measured as the previous release for 0.1.2-rc.1; no earlier release to compare with |

**Accepted regressions** are measured, reviewed and kept on purpose; they are not budget passes. The
next release compares against the version that accepted them.

| Accepted in | Metric | Change | Reason |
|---|---|---|---|
| 0.1.2-rc.2 | Restart to healthy | 52.3 → 103.1 ms (+97.2%) | Each start checks the configured `auth.admin_password` with Argon2id before it reports ready, so a changed password signs out the admin's sessions before any request is served |

Numbers depend on hardware and data shape. Compare them between versions, not with your own
machine.
