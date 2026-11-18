# Benchmarks

Benchmarks are run by hand on fixed hardware, following the [benchmark method](benchmark-method.md)
so that anyone can repeat them. This page compares the most recent measured version with the
previous release; each version has its own page with every metric of every test:

- [0.1.2](benchmarks/0.1.2.md)
- [0.1.2-rc.5](benchmarks/0.1.2-rc.5.md)
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
| Build | 0.1.2: the published release asset `vyrtel-linux-x86_64` (smoke tests on the published image). 0.1.1: the published release asset. See each version's page for hashes | not run |
| Network | Loopback only | not run |
| Configuration | Baseline: `baseline.toml` without three keys 0.1.1 rejects, the same file for both versions. Transport: defaults, and the constrained configuration for one flood run | |
| Workload | Baseline: seed 42; 100,000 logs, 1,000 traces, 100 metric points; 13 queries cold and warm; 20 s mixed load with 4 workers; 5 runs per version. Transport: 60 s steady, 180 s flood (normal and constrained), 30 min with waves; 1 run each, and the 30 min run 2 more times. Transport regression: 60 s steady, 3 runs each of 0.1.2 and the declared baseline build (0.1.1 + REL-17), interleaved. Sign-in memory: `signin-memory.sh`, 1 run | |

## Results: 0.1.2 against 0.1.1

Both versions were measured in the same session on 2026-11-18, with the same harness and
configuration, on the published 0.1.2 release. Local results are measured on the primary alone;
remote results have load sent from the secondary. They are never mixed.

| Test | Metric | 0.1.1 | 0.1.2 | Change | Verdict |
|---|---|---:|---:|---:|---|
| Published artifacts | Checksum, glibc ≤ 2.34, starts on baseline x86-64 and on this non-AVX2 CPU | not run | all pass | | PASS |
| Smoke | API checks on a fresh install | not run | 20 / 20 | | PASS |
| Local baseline | Ingest under mixed load (events/s) | 16,781 | 17,016 | +1.4% | PASS (budget 15%) |
| Local baseline | Seeding throughput (items/s) | 34,859 | 34,361 | −1.4% | PASS (budget 40%) |
| Local baseline | Query p95, `level = Error` under mixed load (ms) | 73.9 | 72.4 | −2.0% | PASS (budget 15%) |
| Local baseline | Peak memory (MiB) | 83.0 | 80.9 | −2.5% | PASS (budget 35%) |
| Local baseline | Restart to healthy (ms) | 52.3 | 103.1 | +97.1% | accepted in 0.1.2-rc.2 |
| Local baseline | Stored bytes per event | 73.2 | 73.1 | −0.2% | PASS (budget 2%) |
| Local baseline | Gated metrics within budget or better | | 42 of 43 | | |
| Sign-in memory | AUTH-PERF-04: peak / retained above idle, 429 p99 | not run | 21.0 / 2.1 MiB, 8.5 ms | | PASS (30 / 8 MiB, 100 ms) |
| Same-host transport | Steady ingest (events/s) / ack p99 (ms) | not run | 14,953 / 2.81 | | PASS |
| Same-host transport | File descriptors: peak / segment files / others after load | not run | 401 / 64 / back to idle | | PASS (448 / 64 / idle + 4) |
| Same-host transport | 503s, transport errors and unexpected statuses for steady clients | not run | 0 / 0 / 0 | | PASS |
| Same-host transport | Flood wave: accept to end / end to end / establishment p99 | not run | 60.0 s / 60.0 s / 67.1 ms | | PASS (82 s / 82 s / 5 s) |
| Same-host transport | Acknowledged vs stored events (4 runs, 16.2 million events) | not run | equal | | PASS |
| Same-host transport | RSS growth, last 20 min of the 30 min run | not run | 1.24 MiB/min | | **FAIL** (budget 1.0); repeats 0.39 and 1.55 |
| Same-host transport | Shutdown after load | not run | 147–192 ms | | PASS (8 s) |
| Same-host transport | Peak admitted / waiting connections, sampled | not run | 256 / 64 (constrained 64 / 64) | | PASS (`max_connections` / 64) |
| Same-host transport | Regression budgets, three steady runs each (baseline: 0.1.1 + REL-17): events/s, ack p95 / p99 (ms), query p95 (ms), peak RSS (MiB), CPU (s) | 14,874, 2.55 / 2.99, 79.4, 139, 14.5 | 14,949, 2.37 / 2.99, 80.1, 125, 13.1 | +0.5%, −7% / 0%, +0.9%, −10%, −10% | PASS (all 6; 0 unexpected failures) |
| Remote | Ingest throughput, p95 / p99 | not run | not run | | INCOMPLETE |
| Soak | 48–72 hours, data loss, memory trend | not run | not run | | INCOMPLETE |

**Verdict for 0.1.2: FAIL on the 30 minute run's RSS growth, in two of three runs.** Every other
measured budget passes, including the transport regression comparison and the sign-in memory
budgets, and every shutdown reading is valid. Remote load and the sustained soak remain
unvalidated.

- **RSS growth in the 30 minute run, 1.24 MiB/min (budget 1.0); repeats 0.39 and 1.55.** Unlike
  0.1.2-rc.5 (one run in three), the miss repeats. Each run over budget is the one where the
  runtime started a 15th thread inside the measured 20 minutes, and RSS rose by 11–12 MiB in the
  30 s after it; on either side of that step the slope is 0.29–0.68 MiB/min. All three runs end
  between 157 and 171 MiB, as 0.1.2-rc.5's did (158–173 MiB). The step, not a steady climb, is
  what fails the budget; whether it is acceptable, or needs a fix or a different measurement, is
  open.
- **Restart to healthy, +97.1%,** the regression accepted in 0.1.2-rc.2 (the admin password is
  checked with Argon2id before the server reports ready).
- **Transport regression comparison, PASS.** Three interleaved 60 s steady runs each of 0.1.2
  and the declared baseline build (0.1.1 + REL-17): throughput +0.5%, ack p95 −7%, query p95
  +0.9%, peak RSS −10%, CPU −10%, all within budget, with no unexpected failures and every
  acknowledged event stored. The first attempt did not run (the soak script was not executable);
  the re-run used the same command.

Details, and the full metric tables, are on the [0.1.2 page](benchmarks/0.1.2.md).

**Integrity:** in every one of the 10 baseline runs, the seeded dataset was offered, acknowledged
and stored exactly, and every mixed-load request was accepted; mixed-load ingest is counted, not
reconciled against storage. In transport, the stored count of `connload`'s events equalled the
acknowledged count in all four runs, the two repeats and the six regression runs. Unexplained
errors or data loss: none found.
**Limitations and untested areas:** no secondary machine, so no remote load or soak. 0.1.2 was
released without final-candidate profiling; these measurements are of the published release.
The baseline's data directories were on tmpfs. 0.1.1 had no smoke, sign-in or transport run.

## History

| Version | Date | Verdict | Local ingest | Remote ingest | Local query p95 | Notes |
|---|---|---|---|---|---|---|
| [0.1.2](benchmarks/0.1.2.md) | 2026-11-18 | FAIL: 30 min RSS growth 1.24 MiB/min, repeated in 1 of 2 repeats (0.39, 1.55); validation incomplete | 17,016 events/s | not run | 72.4 ms | Measured as published (released without final-candidate profiling). Over-budget runs are the ones with a late 15th runtime thread (an 11–12 MiB step); every other measured budget passes, transport regression budgets included; all shutdown readings valid |
| [0.1.2-rc.5](benchmarks/0.1.2-rc.5.md) | 2026-11-15 | FAIL: 30 min RSS growth 1.09 MiB/min (not reproduced), validation incomplete | 17,111 events/s | not run | 71.0 ms | Peak memory back within budget (−4.4%): Argon2id work areas returned after each check, at most 16 sign-ins at once; AUTH-PERF-04 passes; repeats on 2026-11-16 measured 0.69 and 0.50 MiB/min (0.1.2-rc.4: 0.73); one invalid shutdown reading; transport regression budgets pass (2026-11-18) |
| [0.1.2-rc.4](benchmarks/0.1.2-rc.4.md) | 2026-11-15 | FAIL: peak memory open, validation incomplete | 17,054 events/s | not run | 72.5 ms | Flood waves end in 60.1 s (listen backlog 2,048, no dropped handshakes); stored events reconciled in transport; constrained configuration passes; peak memory +32.8% (budget 15%) |
| [0.1.2-rc.3](benchmarks/0.1.2-rc.3.md) | 2026-11-15 | FAIL: peak memory open, flood deadline failed, validation incomplete | 17,064 events/s | not run | 72.0 ms | Published asset fixed (glibc 2.34, no AVX2 needed); file descriptors bounded; peak memory +44% (budget 30%); longest flood wave 91.1 s (budget 82 s) |
| [0.1.2-rc.2](benchmarks/0.1.2-rc.2.md) | 2026-11-11 | FAIL (transport); baseline PASS with one accepted regression | 17,073 events/s | not run | 72.0 ms | Published asset fixed (glibc 2.34, no AVX2 needed). Peak memory +30.2% (budget 40%) after password checks were limited to one at a time; restart to healthy +97.2%, **accepted** (below); file-descriptor budgets and one 503 missed in transport. Measured while it was prepared: baseline of its exact code, smoke and transport of `bd25064` |
| [0.1.2-rc.1](benchmarks/0.1.2-rc.1.md) | 2026-11-10 | FAIL | 17,054 events/s | not run | 71.1 ms | Peak memory +52% and restart to healthy +97% against 0.1.1; release asset needs AVX2 and glibc 2.38; transport file-descriptor budgets missed |
| [0.1.1](benchmarks/0.1.1.md) | 2026-11-10 | not judged | 16,847 events/s | not run | 73.6 ms | Measured as the previous release for 0.1.2-rc.1; no earlier release to compare with. Measured again on 2026-11-15 for 0.1.2-rc.3 (16,866 events/s, 75.3 ms) for 0.1.2-rc.4 (16,734 events/s, 74.4 ms) for 0.1.2-rc.5 (16,856 events/s, 74.5 ms) and for 0.1.2 on 2026-11-18 (16,781 events/s, 73.9 ms) |

**Accepted regressions** are measured, reviewed and kept on purpose; they are not budget passes. The
next release compares against the version that accepted them.

| Accepted in | Metric | Change | Reason |
|---|---|---|---|
| 0.1.2-rc.2 | Restart to healthy | 52.3 → 103.1 ms (+97.2%) | Each start checks the configured `auth.admin_password` with Argon2id before it reports ready, so a changed password signs out the admin's sessions before any request is served |

Numbers depend on hardware and data shape. Compare them between versions, not with your own
machine.
