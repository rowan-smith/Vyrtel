# Benchmarks

Benchmarks are run by hand on fixed hardware, following the [benchmark method](benchmark-method.md)
so that anyone can repeat them. This page compares the most recent measured version with the
previous release; each version has its own page with every metric of every test:

- [0.1.2-rc.3](benchmarks/0.1.2-rc.3.md)
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
| Build | 0.1.2-rc.3: the published release asset `vyrtel-linux-x86_64` (smoke tests on the published image). 0.1.1: the published release asset. See each version's page for hashes | not run |
| Network | Loopback only | not run |
| Configuration | Baseline: `baseline.toml` without three keys 0.1.1 rejects, the same file for both versions. Transport: defaults | |
| Workload | Baseline: seed 42; 100,000 logs, 1,000 traces, 100 metric points; 13 queries cold and warm; 20 s mixed load with 4 workers; 5 runs per version. Transport: 60 s steady, 180 s flood, 30 min with waves; 1 run | |

## Results: 0.1.2-rc.3 against 0.1.1

Both versions were measured in the same session on 2026-11-15, with the same harness and
configuration. Local results are measured on the primary alone; remote results have load sent from
the secondary. They are never mixed.

| Test | Metric | 0.1.1 | 0.1.2-rc.3 | Change | Verdict |
|---|---|---:|---:|---:|---|
| Published artifacts | Checksum, glibc ≤ 2.34, starts on baseline x86-64 and on this non-AVX2 CPU | not run | all pass | | PASS |
| Smoke | API checks on a fresh install | not run | 20 / 20 | | PASS |
| Local baseline | Ingest under mixed load (events/s) | 16,866 | 17,064 | +1.2% | PASS (budget 15%) |
| Local baseline | Seeding throughput (items/s) | 35,307 | 32,354 | −8.4% | not gated |
| Local baseline | Query p95, `level = Error` under mixed load (ms) | 75.3 | 72.0 | −4.5% | PASS (budget 20%) |
| Local baseline | Peak memory (MiB) | 79.6 | 114.7 | +44.0% | **FAIL** (budget 30%); acceptance open |
| Local baseline | Restart to healthy (ms) | 52.3 | 103.2 | +97.3% | accepted in 0.1.2-rc.2 |
| Local baseline | Compression ratio | 4.01× | 4.00× | −0.2% | PASS |
| Local baseline | Stored bytes per event | 73.2 | 73.1 | −0.1% | PASS (budget 2%) |
| Local baseline | Gated metrics within budget or better | | 39 of 41 | | |
| Same-host transport | Steady ingest (events/s) / ack p99 (ms) | not run | 14,600 / 2.79 | | PASS |
| Same-host transport | File descriptors: peak / segment files / others after load | not run | 401 / 64 / back to idle | | PASS (448 / 64 / idle + 4) |
| Same-host transport | 503s and transport errors for steady clients | not run | 0 / 0 | | PASS |
| Same-host transport | Longest flood wave | not run | 91.1 s | | **FAIL** (82 s); measurement open |
| Same-host transport | Acknowledged vs stored events | not run | not read | | INCOMPLETE |
| Remote | Ingest throughput, p95 / p99 | not run | not run | | INCOMPLETE |
| Soak | 48–72 hours, data loss, memory trend | not run | not run | | INCOMPLETE |

**Verdict for 0.1.2-rc.3: FAIL, with two decisions open.** Everything 0.1.2-rc.1 failed on is fixed
except as below: the release asset runs on baseline x86-64 CPUs, file descriptors are bounded, and
there were no 503s or transport errors.

- **Peak memory, +44%.** The remaining cost of sign-in being always on: each login checks the
  password with Argon2id, which allocates 19 MiB (one check at a time since 0.1.2-rc.2). Open: accept
  it as part of that decision, like restart to healthy, or reduce it.
- **Longest flood wave, 91.1 s.** 60 s of body timeout plus 31 s of the kernel's SYN retransmission
  backoff (1 + 2 + 4 + 8 + 16 s): the listen queue overflowed during the wave, and the budget counts
  from the client's first attempt, while the server's bounds start at accept. Open: measure it from
  accept, enlarge the listen backlog, or both.

Details, and the full metric tables, are on the [0.1.2-rc.3 page](benchmarks/0.1.2-rc.3.md).

**Integrity:** in every one of the 10 baseline runs, 100,000 logs, 3,000 spans and 100 metric
points were offered, acknowledged and stored exactly; no mixed-load request was rejected.
Unexplained errors or data loss: none.
**Limitations and untested areas:** no secondary machine, so no remote load or soak. The transport
load tool does not sign in, so stored events were not reconciled and query latency was not
measured. The baseline's data directories were on tmpfs. 0.1.1 had no smoke or transport run.

## History

| Version | Date | Verdict | Local ingest | Remote ingest | Local query p95 | Notes |
|---|---|---|---|---|---|---|
| [0.1.2-rc.3](benchmarks/0.1.2-rc.3.md) | 2026-11-15 | FAIL, two decisions open | 17,064 events/s | not run | 72.0 ms | Published asset fixed (glibc 2.34, no AVX2 needed); file descriptors bounded; peak memory +44% (budget 30%); longest flood wave 91.1 s (budget 82 s) |
| 0.1.2-rc.2 candidate (`1332e29`) | 2026-11-11 | FAIL (transport); baseline PASS with one accepted regression | 17,073 events/s | not run | 72.0 ms | Pre-tag, baseline only, against 0.1.1. Peak memory +30.2% (budget 40%) after password checks were limited to one at a time; restart to healthy +97.2%, **accepted** (below) |
| [0.1.2-rc.1](benchmarks/0.1.2-rc.1.md) | 2026-11-10 | FAIL | 17,054 events/s | not run | 71.1 ms | Peak memory +52% and restart to healthy +97% against 0.1.1; release asset needs AVX2 and glibc 2.38; transport file-descriptor budgets missed |
| [0.1.1](benchmarks/0.1.1.md) | 2026-11-10 | not judged | 16,847 events/s | not run | 73.6 ms | Measured as the previous release for 0.1.2-rc.1; no earlier release to compare with. Measured again for 0.1.2-rc.3 on 2026-11-15 (16,866 events/s, 75.3 ms) |

**Accepted regressions** are measured, reviewed and kept on purpose; they are not budget passes. The
next release compares against the version that accepted them.

| Accepted in | Metric | Change | Reason |
|---|---|---|---|
| 0.1.2-rc.2 | Restart to healthy | 52.3 → 103.1 ms (+97.2%) | Each start checks the configured `auth.admin_password` with Argon2id before it reports ready, so a changed password signs out the admin's sessions before any request is served |

Numbers depend on hardware and data shape. Compare them between versions, not with your own
machine.
