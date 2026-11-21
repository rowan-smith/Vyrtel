---
name: performance-check
description: Check Vyrtel changes for memory, throughput, latency or overload regressions; use the existing benchmarks and record reproducible evidence.
---

# Performance check

1. Identify the affected hot path and the resource at risk: CPU, memory, disk, connections or latency.
2. Check for unbounded queues, allocations, scans, blocking work and missing cancellation.
3. Choose the smallest representative test from `docs/testing.md` and `docs/development.md`.
4. When measuring, use a release build, a defined workload and a comparable baseline.
5. Record machine, configuration, commands, result and noise/limitations.

Distinguish **measured regression**, **plausible risk** and **not tested**. Never fabricate benchmark results or treat one noisy run as proof.
For release sign-off, follow the profiling requirements in `docs/releasing.md`.
