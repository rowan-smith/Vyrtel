# REL-18 transport budgets

Declared 2026-10-17T07:13:00Z, before any candidate measurement of the
transport runs in [`2026-10-17-wsl2/`](2026-10-17-wsl2/README.md). Baseline:
`main` at `6c8a4a230eb76a8c6a35c24e992bebeced3a4287` (0.1.1 + REL-17). These
limits are not changed after the candidate is measured; a miss is a FAIL and
is fixed, not re-budgeted.

Configurations:

* **normal**: defaults (`max_memory = 512MB`, `max_connections = 256`,
  `connection_buffer = 64KB`), server container 2 CPUs, 1 GiB, 4096 FDs.
* **constrained**: `max_memory = 256MB`, `max_connections = 64`,
  `connection_buffer = 16KB`, server container 2 CPUs, 384 MiB, 1024 FDs.

## Absolute budgets (new behaviour, no baseline)

| Metric                                                | Unit    | Limit                                                                                       |
|-------------------------------------------------------|---------|---------------------------------------------------------------------------------------------|
| Peak admitted connections (sampled)                   | count   | ≤ `max_connections`                                                                         |
| Peak waiting connections (sampled)                    | count   | ≤ 64                                                                                        |
| Transport heap at the limits (`tests/overload`)       | bytes   | ≤ `TransportLimits::estimated_peak`                                                        |
| Heap retained after every connection closed           | KiB     | ≤ 512                                                                                       |
| Runtime tasks during a flood (`tests/overload`)       | count   | ≤ `max_connections` + 64 + 16                                                               |
| Peak server FDs                                       | count   | ≤ `max_connections` + 64 + 128                                                              |
| Server FDs and sockets after load (idle)              | count   | FDs ≤ idle-before + 4; sockets ≤ 8                                                          |
| Server OOM kills / exit status after `docker stop`    | —       | none / 0                                                                                    |
| Peak RSS                                              | MiB     | normal ≤ 1024; constrained ≤ 384 (container limits)                                         |
| Post-load RSS                                         | MiB     | ≤ `max_memory`                                                                              |
| RSS growth slope, last 20 min of each 30 min soak     | MiB/min | ≤ 1.0                                                                                       |
| Steady clients' transport errors and `server_busy` during floods | count | 0 (ingest, query and sampler clients hold admitted keep-alive connections)        |
| Acknowledged vs persisted events                      | events  | equal                                                                                       |
| Shutdown after load                                   | s       | ≤ 8 (`shutdown_timeout`)                                                                    |
| Time for a flood wave's connections to end            | s       | ≤ `header_timeout` + `body_timeout` + `PENDING_TIMEOUT` + 10                                |

## Regression budgets (steady mixed load, no waves, normal configuration)

Three 60 s runs each of baseline and candidate. Tolerance `t = max(10%, 3 ×
CV)`, where CV is the coefficient of variation of the baseline's three runs
for that metric (the 3 × CV rule of [reproducible baselines](../../benchmarks.md#regression-budgets),
with a 10% floor because three runs on a shared WSL2 VM are noisy).

| Metric                         | Unit     | Limit                                    |
|--------------------------------|----------|------------------------------------------|
| Acknowledged events/s (mean)   | events/s | ≥ baseline mean × (1 − t)                |
| Ingest ACK latency p95 (mean)  | ms       | ≤ baseline mean × (1 + t) + 2 ms         |
| Ingest ACK latency p99 (mean)  | ms       | ≤ baseline mean × (1 + t) + 5 ms         |
| Query latency p95 (mean)       | ms       | ≤ baseline mean × (1 + t) + 2 ms         |
| Peak RSS (mean)                | MiB      | ≤ baseline mean × (1 + t)                |
| CPU seconds (mean)             | s        | ≤ baseline mean × (1 + t)                |
| Unexpected failures            | count    | 0 (non-200 other than 429, transport errors) |
