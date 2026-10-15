# Vyrtel performance baseline

- Generated: 2026-10-15 12:03:05 UTC
- Label: 0.1.1 same-commit check
- Vyrtel: 0.1.1 at `b61cd28761aa` (uncommitted changes)
- Duration: 3.3 min for 3 run(s) (66 s, 65 s, 65 s)
- Workload: seed 42; 100,000 log events, 1,000 traces, 100 metric points in batches of 500; each query once cold then 20× warm; mixed load 20s (3s warmup discarded) with 4 workers

## Host and build

| | |
| --- | --- |
| CPU | Intel(R) Core(TM) Ultra 7 155H, 22 logical cores |
| Memory | 31.5 GB available to the harness |
| OS | Microsoft Windows 10.0.26200 (X64) |
| Disk | C:\ NTFS (Fixed), 22.2 GB free of 474.7 GB |
| Harness runtime | .NET 10.0.8 |
| Notes | Laptop, NVMe SSD, AC power, desktop apps open |
| Binary | 11.8 MB (release), SHA-256 `cb7f5384782d8be7…` |
| Compiler | rustc 1.96.1 (31fca3adb 2026-06-26) |
| Configuration | `dotnet/Vyrtel.Baseline/baseline.toml`, SHA-256 `3593a029c80e2d51…` |
| Ingest admission | 3 concurrent (explicit), 1.0 MB max request |
| Compression | zstd level 3, independently compressed column chunks: 4.01× ratio, 7.6% index overhead, 73.1 bytes/event, 22 segments, 7.2 MB data directory |

## Memory

| Run | Peak RSS | Idle after start | Seeding peak | Query phase peak | Mixed load peak | Source |
| ---: | ---: | ---: | ---: | ---: | ---: | --- |
| 1 | 56.2 MB | 12.4 MB | 30.1 MB | 20.8 MB | 56.2 MB | os-peak |
| 2 | 54.1 MB | 12.4 MB | 31.5 MB | 20.9 MB | 54.1 MB | os-peak |
| 3 | 53.6 MB | 12.3 MB | 32.3 MB | 21.0 MB | 53.6 MB | os-peak |

## Accepted and rejected requests

Backpressure is 429/503 (retryable); seeding retries it, so the dataset always arrives complete.

| Run | Phase | Requests | Accepted | Backpressure | Too large (413) | Other rejected | Transport errors | Items accepted | Throughput | p50 | p95 | p99 |
| ---: | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 1 | seeding | 1,201 | 1,201 | 0 | 0 | 0 | 0 | 101,100 | 51,142 items/s | 0.195 ms | 4.16 ms | 5.82 ms |
| 1 | mixed | 3,603 | 3,603 | 0 | 0 | 0 | 0 | 315,200 | 212 req/s | | | |
| 2 | seeding | 1,201 | 1,201 | 0 | 0 | 0 | 0 | 101,100 | 71,896 items/s | 0.167 ms | 3.68 ms | 5.22 ms |
| 2 | mixed | 3,664 | 3,664 | 0 | 0 | 0 | 0 | 319,200 | 216 req/s | | | |
| 3 | seeding | 1,201 | 1,201 | 0 | 0 | 0 | 0 | 101,100 | 73,592 items/s | 0.150 ms | 3.45 ms | 4.58 ms |
| 3 | mixed | 3,800 | 3,800 | 0 | 0 | 0 | 0 | 330,400 | 224 req/s | | | |

## Mixed load by operation

Mean across runs; ± is the run-to-run coefficient of variation.

| Operation | Requests (run 1) | Rejected (all runs) | p50 | p95 | p99 |
| --- | ---: | ---: | ---: | ---: | ---: |
| ingest NDJSON (800 events) | 244 | 0 | 5.68 ms ±5% | 16.00 ms ±23% | 23.17 ms ±18% |
| ingest JSON array (800 events) | 150 | 0 | 5.94 ms ±2% | 16.66 ms ±18% | 26.58 ms ±16% |
| latest logs (limit 200) | 542 | 0 | 18.55 ms ±3% | 25.59 ms ±6% | 29.18 ms ±4% |
| count: level = Error | 462 | 0 | 18.78 ms ±3% | 27.95 ms ±0% | 31.86 ms ±1% |
| level = Error | 359 | 0 | 43.79 ms ±3% | 55.45 ms ±6% | 63.32 ms ±6% |
| service = payments | 328 | 0 | 19.44 ms ±2% | 26.83 ms ±7% | 30.54 ms ±7% |
| traceId (Bloom filter) | 312 | 0 | 1.04 ms ±3% | 1.66 ms ±2% | 2.41 ms ±14% |
| message contains (scan) | 161 | 0 | 72.31 ms ±5% | 96.07 ms ±2% | 102.97 ms ±4% |
| histogram, 60 buckets | 159 | 0 | 18.35 ms ±1% | 27.59 ms ±1% | 32.20 ms ±5% |
| facets | 147 | 0 | 47.64 ms ±3% | 68.05 ms ±7% | 79.84 ms ±1% |
| trace search (limit 50) | 305 | 0 | 3.64 ms ±5% | 6.72 ms ±3% | 8.12 ms ±5% |
| trace by id | 161 | 0 | 0.493 ms ±8% | 0.864 ms ±7% | 1.59 ms ±59% |
| list metric names | 152 | 0 | 0.277 ms ±6% | 0.565 ms ±10% | 0.914 ms ±17% |
| metric series | 121 | 0 | 0.429 ms ±3% | 0.759 ms ±9% | 1.18 ms ±17% |

## Queries: cold and warm

Cold is the first request after a server restart (no index cache, nothing decoded; the OS page cache is not dropped). Warm is the following repetitions. Diagnostics are the server's own, from run 1.

| Query | Kind | Cold | Warm p50 | Warm p95 | Cold ÷ warm | Segments (skipped/considered) | Blocks read (skipped) | Events examined → matched | Bytes read cold → warm | Indexes |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | --- |
| level = Error (bitmap) | selective | 23.08 ms ±22% | 20.14 ms ±3% | 22.71 ms ±6% | 1.1× | 0/22 | 114 (0) | 31,597 → 611 | 3.0 MB → 3.0 MB | timestamp:time, level:bitmap |
| service = payments | selective | 15.42 ms ±89% | 7.46 ms ±9% | 8.88 ms ±11% | 2.1× | 0/22 | 50 (0) | 6,429 → 817 | 823.3 KB → 823.3 KB | timestamp:time, service:bitmap |
| traceId = … (Bloom filter) | selective | 1.96 ms ±33% | 0.508 ms ±10% | 1.04 ms ±16% | 3.9× | 20/22 | 3 (108) | 1,808 → 1 | 44.4 KB → 44.4 KB | timestamp:time, traceId:bloom |
| durationMs > 1990 (zone map) | selective | 102.45 ms ±4% | 105.13 ms ±2% | 108.28 ms ±2% | 1.0× | 0/22 | 88 (22) | 82,432 → 182 | 4.9 MB → 4.9 MB | timestamp:time, durationMs:zonemap |
| count: level = Error | selective | 5.32 ms ±32% | 3.63 ms ±4% | 4.77 ms ±9% | 1.5× | 0/22 | 110 (0) | 100,000 → 1,981 | 480.2 KB → 480.2 KB | timestamp:time, level:bitmap |
| message contains "timed out" (scan) | scan | 50.69 ms ±27% | 38.86 ms ±2% | 40.96 ms ±1% | 1.3× | 0/22 | 178 (0) | 75,788 → 372 | 5.4 MB → 5.4 MB | timestamp:time, message:none |
| message contains "error" (scan) | scan | 16.86 ms ±4% | 15.99 ms ±2% | 18.45 ms ±2% | 1.1× | 0/22 | 110 (0) | 100,000 → 0 | 1,022.0 KB → 1,022.0 KB | timestamp:time, message:none |
| latest logs (limit 200) | recent | 7.22 ms ±9% | 6.30 ms ±3% | 7.99 ms ±4% | 1.1× | 0/22 | 44 (0) | 1,206 → 1,206 | 604.6 KB → 604.6 KB | timestamp:time |
| histogram, 60 buckets | aggregate | 4.99 ms ±12% | 3.64 ms ±4% | 4.37 ms ±2% | 1.4× | 0/22 | 110 (0) | 100,000 → 100,000 | 480.2 KB → 480.2 KB | timestamp:time |
| facets, 2000-event sample | aggregate | 21.61 ms ±19% | 19.48 ms ±1% | 21.71 ms ±6% | 1.1× | 0/22 | 54 (0) | 7,800 → 7,800 | 947.0 KB → 947.0 KB | timestamp:time |
| trace search (limit 50) | trace | 4.38 ms ±29% | 2.48 ms ±10% | 3.24 ms ±30% | 1.8× | 0/0 | 0 (0) | 4,530 → 1,680 | 0 B → 0 B | timestamp:time |
| trace by id | trace | 1.04 ms ±41% | 0.393 ms ±33% | 0.634 ms ±7% | 2.6× | 0/0 | 0 (0) | 3,000 → 3 | 0 B → 0 B | timestamp:time |
| metric series | metric | 1.10 ms ±51% | 0.372 ms ±56% | 0.550 ms ±31% | 2.9× | 0/0 | 0 (0) | 100 → 100 | 0 B → 0 B | timestamp:time, message:none |

## Run-to-run variance and proposed regression budgets

budget = max(floor, 3 × CV × k(n) rounded up), where k(n) turns the CV of n runs into a 90 % upper confidence bound; floor 15% for timings and memory, 2% for sizes; not gated with fewer than 3 runs or a budget over 50%. See docs/benchmarks.md#regression-budgets.

| Metric | Better | Runs | Mean | Std dev | CV | Min | Max | Budget | Justification |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | --- |
| Seeding throughput | higher | 3 | 65,543 items/s | 12,501 items/s | 19.1% | 51,142 items/s | 73,592 items/s | – | not gated: 3 runs, CV 19.1% → 90% upper bound 58.8% (×3.08) → 3× = 176.3%, rounded up to 180%; floor 15% → 180% exceeds 50%; too noisy to gate with this many runs on this host |
| Seeding request p95 | lower | 3 | 3.76 ms | 0.363 ms | 9.6% | 3.45 ms | 4.16 ms | – | not gated: 3 runs, CV 9.6% → 90% upper bound 29.7% (×3.08) → 3× = 89.2%, rounded up to 90%; floor 15% → 90% exceeds 50%; too noisy to gate with this many runs on this host |
| level = Error (bitmap): cold | lower | 3 | 23.08 ms | 5.10 ms | 22.1% | 19.90 ms | 28.96 ms | – | not gated: 3 runs, CV 22.1% → 90% upper bound 68.0% (×3.08) → 3× = 204.1%, rounded up to 205%; floor 15% → 205% exceeds 50%; too noisy to gate with this many runs on this host |
| level = Error (bitmap): warm p50 | lower | 3 | 20.14 ms | 0.571 ms | 2.8% | 19.49 ms | 20.56 ms | 30% | 3 runs, CV 2.8% → 90% upper bound 8.7% (×3.08) → 3× = 26.2%, rounded up to 30%; floor 15% → 30% |
| level = Error (bitmap): warm p95 | lower | 3 | 22.71 ms | 1.25 ms | 5.5% | 21.45 ms | 23.95 ms | – | not gated: 3 runs, CV 5.5% → 90% upper bound 17.0% (×3.08) → 3× = 51.1%, rounded up to 55%; floor 15% → 55% exceeds 50%; too noisy to gate with this many runs on this host |
| service = payments: cold | lower | 3 | 15.42 ms | 13.70 ms | 88.8% | 7.29 ms | 31.24 ms | – | not gated: 3 runs, CV 88.8% → 90% upper bound 273.7% (×3.08) → 3× = 821.0%, rounded up to 825%; floor 15% → 825% exceeds 50%; too noisy to gate with this many runs on this host |
| service = payments: warm p50 | lower | 3 | 7.46 ms | 0.653 ms | 8.7% | 6.97 ms | 8.20 ms | – | not gated: 3 runs, CV 8.7% → 90% upper bound 26.9% (×3.08) → 3× = 80.8%, rounded up to 85%; floor 15% → 85% exceeds 50%; too noisy to gate with this many runs on this host |
| service = payments: warm p95 | lower | 3 | 8.88 ms | 1.01 ms | 11.4% | 7.80 ms | 9.81 ms | – | not gated: 3 runs, CV 11.4% → 90% upper bound 35.1% (×3.08) → 3× = 105.3%, rounded up to 110%; floor 15% → 110% exceeds 50%; too noisy to gate with this many runs on this host |
| traceId = … (Bloom filter): cold | lower | 3 | 1.96 ms | 0.646 ms | 32.9% | 1.54 ms | 2.71 ms | – | not gated: 3 runs, CV 32.9% → 90% upper bound 101.3% (×3.08) → 3× = 303.9%, rounded up to 305%; floor 15% → 305% exceeds 50%; too noisy to gate with this many runs on this host |
| traceId = … (Bloom filter): warm p50 | lower | 3 | 0.508 ms | 0.052 ms | 10.3% | 0.458 ms | 0.563 ms | – | not gated: 3 runs, CV 10.3% → 90% upper bound 31.8% (×3.08) → 3× = 95.3%, rounded up to 100%; floor 15% → 100% exceeds 50%; too noisy to gate with this many runs on this host |
| traceId = … (Bloom filter): warm p95 | lower | 3 | 1.04 ms | 0.163 ms | 15.7% | 0.857 ms | 1.17 ms | – | not gated: 3 runs, CV 15.7% → 90% upper bound 48.4% (×3.08) → 3× = 145.2%, rounded up to 150%; floor 15% → 150% exceeds 50%; too noisy to gate with this many runs on this host |
| durationMs > 1990 (zone map): cold | lower | 3 | 102.45 ms | 4.47 ms | 4.4% | 99.41 ms | 107.59 ms | 45% | 3 runs, CV 4.4% → 90% upper bound 13.5% (×3.08) → 3× = 40.4%, rounded up to 45%; floor 15% → 45% |
| durationMs > 1990 (zone map): warm p50 | lower | 3 | 105.13 ms | 1.98 ms | 1.9% | 103.33 ms | 107.25 ms | 20% | 3 runs, CV 1.9% → 90% upper bound 5.8% (×3.08) → 3× = 17.4%, rounded up to 20%; floor 15% → 20% |
| durationMs > 1990 (zone map): warm p95 | lower | 3 | 108.28 ms | 2.10 ms | 1.9% | 106.06 ms | 110.23 ms | – | not gated: p95 from 20 samples in one run; at least 200 are needed for 10 beyond it |
| count: level = Error: cold | lower | 3 | 5.32 ms | 1.71 ms | 32.1% | 4.30 ms | 7.29 ms | – | not gated: 3 runs, CV 32.1% → 90% upper bound 98.8% (×3.08) → 3× = 296.5%, rounded up to 300%; floor 15% → 300% exceeds 50%; too noisy to gate with this many runs on this host |
| count: level = Error: warm p50 | lower | 3 | 3.63 ms | 0.129 ms | 3.6% | 3.55 ms | 3.78 ms | 35% | 3 runs, CV 3.6% → 90% upper bound 11.0% (×3.08) → 3× = 32.9%, rounded up to 35%; floor 15% → 35% |
| count: level = Error: warm p95 | lower | 3 | 4.77 ms | 0.425 ms | 8.9% | 4.29 ms | 5.11 ms | – | not gated: 3 runs, CV 8.9% → 90% upper bound 27.5% (×3.08) → 3× = 82.4%, rounded up to 85%; floor 15% → 85% exceeds 50%; too noisy to gate with this many runs on this host |
| message contains "timed out" (scan): cold | lower | 3 | 50.69 ms | 13.71 ms | 27.0% | 42.32 ms | 66.51 ms | – | not gated: 3 runs, CV 27.0% → 90% upper bound 83.3% (×3.08) → 3× = 249.9%, rounded up to 250%; floor 15% → 250% exceeds 50%; too noisy to gate with this many runs on this host |
| message contains "timed out" (scan): warm p50 | lower | 3 | 38.86 ms | 0.807 ms | 2.1% | 38.10 ms | 39.71 ms | 20% | 3 runs, CV 2.1% → 90% upper bound 6.4% (×3.08) → 3× = 19.2%, rounded up to 20%; floor 15% → 20% |
| message contains "timed out" (scan): warm p95 | lower | 3 | 40.96 ms | 0.379 ms | 0.9% | 40.61 ms | 41.36 ms | – | not gated: p95 from 20 samples in one run; at least 200 are needed for 10 beyond it |
| message contains "error" (scan): cold | lower | 3 | 16.86 ms | 0.708 ms | 4.2% | 16.08 ms | 17.46 ms | 40% | 3 runs, CV 4.2% → 90% upper bound 12.9% (×3.08) → 3× = 38.8%, rounded up to 40%; floor 15% → 40% |
| message contains "error" (scan): warm p50 | lower | 3 | 15.99 ms | 0.333 ms | 2.1% | 15.61 ms | 16.25 ms | 20% | 3 runs, CV 2.1% → 90% upper bound 6.4% (×3.08) → 3× = 19.2%, rounded up to 20%; floor 15% → 20% |
| message contains "error" (scan): warm p95 | lower | 3 | 18.45 ms | 0.397 ms | 2.2% | 18.07 ms | 18.86 ms | – | not gated: p95 from 20 samples in one run; at least 200 are needed for 10 beyond it |
| latest logs (limit 200): cold | lower | 3 | 7.22 ms | 0.637 ms | 8.8% | 6.55 ms | 7.82 ms | – | not gated: 3 runs, CV 8.8% → 90% upper bound 27.2% (×3.08) → 3× = 81.6%, rounded up to 85%; floor 15% → 85% exceeds 50%; too noisy to gate with this many runs on this host |
| latest logs (limit 200): warm p50 | lower | 3 | 6.30 ms | 0.208 ms | 3.3% | 6.12 ms | 6.53 ms | 35% | 3 runs, CV 3.3% → 90% upper bound 10.2% (×3.08) → 3× = 30.6%, rounded up to 35%; floor 15% → 35% |
| latest logs (limit 200): warm p95 | lower | 3 | 7.99 ms | 0.305 ms | 3.8% | 7.64 ms | 8.20 ms | – | not gated: p95 from 20 samples in one run; at least 200 are needed for 10 beyond it |
| histogram, 60 buckets: cold | lower | 3 | 4.99 ms | 0.582 ms | 11.7% | 4.33 ms | 5.42 ms | – | not gated: 3 runs, CV 11.7% → 90% upper bound 35.9% (×3.08) → 3× = 107.8%, rounded up to 110%; floor 15% → 110% exceeds 50%; too noisy to gate with this many runs on this host |
| histogram, 60 buckets: warm p50 | lower | 3 | 3.64 ms | 0.146 ms | 4.0% | 3.49 ms | 3.78 ms | 40% | 3 runs, CV 4.0% → 90% upper bound 12.3% (×3.08) → 3× = 37.0%, rounded up to 40%; floor 15% → 40% |
| histogram, 60 buckets: warm p95 | lower | 3 | 4.37 ms | 0.090 ms | 2.1% | 4.28 ms | 4.46 ms | – | not gated: p95 from 20 samples in one run; at least 200 are needed for 10 beyond it |
| facets, 2000-event sample: cold | lower | 3 | 21.61 ms | 4.01 ms | 18.6% | 19.26 ms | 26.24 ms | – | not gated: 3 runs, CV 18.6% → 90% upper bound 57.2% (×3.08) → 3× = 171.5%, rounded up to 175%; floor 15% → 175% exceeds 50%; too noisy to gate with this many runs on this host |
| facets, 2000-event sample: warm p50 | lower | 3 | 19.48 ms | 0.261 ms | 1.3% | 19.24 ms | 19.76 ms | 15% | 3 runs, CV 1.3% → 90% upper bound 4.1% (×3.08) → 3× = 12.4%, rounded up to 15%; floor 15% → 15% |
| facets, 2000-event sample: warm p95 | lower | 3 | 21.71 ms | 1.20 ms | 5.5% | 20.94 ms | 23.09 ms | – | not gated: 3 runs, CV 5.5% → 90% upper bound 17.0% (×3.08) → 3× = 50.9%, rounded up to 55%; floor 15% → 55% exceeds 50%; too noisy to gate with this many runs on this host |
| trace search (limit 50): cold | lower | 3 | 4.38 ms | 1.28 ms | 29.1% | 3.46 ms | 5.84 ms | – | not gated: 3 runs, CV 29.1% → 90% upper bound 89.7% (×3.08) → 3× = 269.2%, rounded up to 270%; floor 15% → 270% exceeds 50%; too noisy to gate with this many runs on this host |
| trace search (limit 50): warm p50 | lower | 3 | 2.48 ms | 0.244 ms | 9.8% | 2.25 ms | 2.74 ms | – | not gated: 3 runs, CV 9.8% → 90% upper bound 30.3% (×3.08) → 3× = 90.8%, rounded up to 95%; floor 15% → 95% exceeds 50%; too noisy to gate with this many runs on this host |
| trace search (limit 50): warm p95 | lower | 3 | 3.24 ms | 0.959 ms | 29.6% | 2.64 ms | 4.34 ms | – | not gated: 3 runs, CV 29.6% → 90% upper bound 91.2% (×3.08) → 3× = 273.5%, rounded up to 275%; floor 15% → 275% exceeds 50%; too noisy to gate with this many runs on this host |
| trace by id: cold | lower | 3 | 1.04 ms | 0.421 ms | 40.5% | 0.563 ms | 1.36 ms | – | not gated: 3 runs, CV 40.5% → 90% upper bound 124.8% (×3.08) → 3× = 374.5%, rounded up to 375%; floor 15% → 375% exceeds 50%; too noisy to gate with this many runs on this host |
| trace by id: warm p50 | lower | 3 | 0.393 ms | 0.128 ms | 32.6% | 0.254 ms | 0.507 ms | – | not gated: 3 runs, CV 32.6% → 90% upper bound 100.3% (×3.08) → 3× = 300.9%, rounded up to 305%; floor 15% → 305% exceeds 50%; too noisy to gate with this many runs on this host |
| trace by id: warm p95 | lower | 3 | 0.634 ms | 0.046 ms | 7.2% | 0.588 ms | 0.679 ms | – | not gated: 3 runs, CV 7.2% → 90% upper bound 22.1% (×3.08) → 3× = 66.4%, rounded up to 70%; floor 15% → 70% exceeds 50%; too noisy to gate with this many runs on this host |
| metric series: cold | lower | 3 | 1.10 ms | 0.563 ms | 51.3% | 0.587 ms | 1.70 ms | – | not gated: 3 runs, CV 51.3% → 90% upper bound 158.0% (×3.08) → 3× = 474.1%, rounded up to 475%; floor 15% → 475% exceeds 50%; too noisy to gate with this many runs on this host |
| metric series: warm p50 | lower | 3 | 0.372 ms | 0.207 ms | 55.6% | 0.203 ms | 0.603 ms | – | not gated: 3 runs, CV 55.6% → 90% upper bound 171.4% (×3.08) → 3× = 514.1%, rounded up to 515%; floor 15% → 515% exceeds 50%; too noisy to gate with this many runs on this host |
| metric series: warm p95 | lower | 3 | 0.550 ms | 0.171 ms | 31.1% | 0.371 ms | 0.712 ms | – | not gated: 3 runs, CV 31.1% → 90% upper bound 95.9% (×3.08) → 3× = 287.7%, rounded up to 290%; floor 15% → 290% exceeds 50%; too noisy to gate with this many runs on this host |
| Mixed load throughput | higher | 3 | 217 req/s | 6 req/s | 2.7% | 212 req/s | 224 req/s | 30% | 3 runs, CV 2.7% → 90% upper bound 8.4% (×3.08) → 3× = 25.3%, rounded up to 30%; floor 15% → 30% |
| Mixed ingest NDJSON (800 events): p50 | lower | 3 | 5.68 ms | 0.273 ms | 4.8% | 5.38 ms | 5.90 ms | 45% | 3 runs, CV 4.8% → 90% upper bound 14.8% (×3.08) → 3× = 44.4%, rounded up to 45%; floor 15% → 45% |
| Mixed ingest NDJSON (800 events): p95 | lower | 3 | 16.00 ms | 3.65 ms | 22.8% | 13.01 ms | 20.07 ms | – | not gated: 3 runs, CV 22.8% → 90% upper bound 70.4% (×3.08) → 3× = 211.1%, rounded up to 215%; floor 15% → 215% exceeds 50%; too noisy to gate with this many runs on this host |
| Mixed ingest NDJSON (800 events): p99 | lower | 3 | 23.17 ms | 4.19 ms | 18.1% | 19.97 ms | 27.91 ms | – | not gated: 3 runs, CV 18.1% → 90% upper bound 55.7% (×3.08) → 3× = 167.1%, rounded up to 170%; floor 15% → 170% exceeds 50%; too noisy to gate with this many runs on this host |
| Mixed ingest JSON array (800 events): p50 | lower | 3 | 5.94 ms | 0.147 ms | 2.5% | 5.81 ms | 6.10 ms | 25% | 3 runs, CV 2.5% → 90% upper bound 7.6% (×3.08) → 3× = 22.9%, rounded up to 25%; floor 15% → 25% |
| Mixed ingest JSON array (800 events): p95 | lower | 3 | 16.66 ms | 2.93 ms | 17.6% | 14.53 ms | 19.99 ms | – | not gated: 3 runs, CV 17.6% → 90% upper bound 54.1% (×3.08) → 3× = 162.3%, rounded up to 165%; floor 15% → 165% exceeds 50%; too noisy to gate with this many runs on this host |
| Mixed ingest JSON array (800 events): p99 | lower | 3 | 26.58 ms | 4.20 ms | 15.8% | 23.09 ms | 31.24 ms | – | not gated: 3 runs, CV 15.8% → 90% upper bound 48.7% (×3.08) → 3× = 146.0%, rounded up to 150%; floor 15% → 150% exceeds 50%; too noisy to gate with this many runs on this host |
| Mixed latest logs (limit 200): p50 | lower | 3 | 18.55 ms | 0.481 ms | 2.6% | 18.00 ms | 18.87 ms | 25% | 3 runs, CV 2.6% → 90% upper bound 8.0% (×3.08) → 3× = 24.0%, rounded up to 25%; floor 15% → 25% |
| Mixed latest logs (limit 200): p95 | lower | 3 | 25.59 ms | 1.62 ms | 6.3% | 23.74 ms | 26.78 ms | – | not gated: 3 runs, CV 6.3% → 90% upper bound 19.5% (×3.08) → 3× = 58.6%, rounded up to 60%; floor 15% → 60% exceeds 50%; too noisy to gate with this many runs on this host |
| Mixed latest logs (limit 200): p99 | lower | 3 | 29.18 ms | 1.04 ms | 3.6% | 28.36 ms | 30.35 ms | – | not gated: p99 from 542 samples in one run; at least 1000 are needed for 10 beyond it |
| Mixed count: level = Error: p50 | lower | 3 | 18.78 ms | 0.547 ms | 2.9% | 18.15 ms | 19.17 ms | 30% | 3 runs, CV 2.9% → 90% upper bound 9.0% (×3.08) → 3× = 26.9%, rounded up to 30%; floor 15% → 30% |
| Mixed count: level = Error: p95 | lower | 3 | 27.95 ms | 0.123 ms | 0.4% | 27.81 ms | 28.05 ms | 15% | 3 runs, CV 0.4% → 90% upper bound 1.4% (×3.08) → 3× = 4.1%, rounded up to 5%; floor 15% → 15% |
| Mixed count: level = Error: p99 | lower | 3 | 31.86 ms | 0.321 ms | 1.0% | 31.49 ms | 32.09 ms | – | not gated: p99 from 462 samples in one run; at least 1000 are needed for 10 beyond it |
| Mixed level = Error: p50 | lower | 3 | 43.79 ms | 1.10 ms | 2.5% | 42.54 ms | 44.58 ms | 25% | 3 runs, CV 2.5% → 90% upper bound 7.7% (×3.08) → 3× = 23.2%, rounded up to 25%; floor 15% → 25% |
| Mixed level = Error: p95 | lower | 3 | 55.45 ms | 3.47 ms | 6.3% | 51.95 ms | 58.88 ms | – | not gated: 3 runs, CV 6.3% → 90% upper bound 19.3% (×3.08) → 3× = 57.8%, rounded up to 60%; floor 15% → 60% exceeds 50%; too noisy to gate with this many runs on this host |
| Mixed level = Error: p99 | lower | 3 | 63.32 ms | 3.88 ms | 6.1% | 61.00 ms | 67.79 ms | – | not gated: 3 runs, CV 6.1% → 90% upper bound 18.9% (×3.08) → 3× = 56.6%, rounded up to 60%; floor 15% → 60% exceeds 50%; too noisy to gate with this many runs on this host |
| Mixed service = payments: p50 | lower | 3 | 19.44 ms | 0.470 ms | 2.4% | 18.90 ms | 19.75 ms | 25% | 3 runs, CV 2.4% → 90% upper bound 7.5% (×3.08) → 3× = 22.4%, rounded up to 25%; floor 15% → 25% |
| Mixed service = payments: p95 | lower | 3 | 26.83 ms | 2.00 ms | 7.4% | 24.87 ms | 28.86 ms | – | not gated: 3 runs, CV 7.4% → 90% upper bound 22.9% (×3.08) → 3× = 68.8%, rounded up to 70%; floor 15% → 70% exceeds 50%; too noisy to gate with this many runs on this host |
| Mixed service = payments: p99 | lower | 3 | 30.54 ms | 2.12 ms | 6.9% | 29.02 ms | 32.96 ms | – | not gated: 3 runs, CV 6.9% → 90% upper bound 21.4% (×3.08) → 3× = 64.1%, rounded up to 65%; floor 15% → 65% exceeds 50%; too noisy to gate with this many runs on this host |
| Mixed traceId (Bloom filter): p50 | lower | 3 | 1.04 ms | 0.032 ms | 3.1% | 1.01 ms | 1.07 ms | 30% | 3 runs, CV 3.1% → 90% upper bound 9.5% (×3.08) → 3× = 28.4%, rounded up to 30%; floor 15% → 30% |
| Mixed traceId (Bloom filter): p95 | lower | 3 | 1.66 ms | 0.041 ms | 2.5% | 1.63 ms | 1.70 ms | 25% | 3 runs, CV 2.5% → 90% upper bound 7.6% (×3.08) → 3× = 22.8%, rounded up to 25%; floor 15% → 25% |
| Mixed traceId (Bloom filter): p99 | lower | 3 | 2.41 ms | 0.331 ms | 13.8% | 2.05 ms | 2.70 ms | – | not gated: 3 runs, CV 13.8% → 90% upper bound 42.4% (×3.08) → 3× = 127.1%, rounded up to 130%; floor 15% → 130% exceeds 50%; too noisy to gate with this many runs on this host |
| Mixed message contains (scan): p50 | lower | 3 | 72.31 ms | 3.48 ms | 4.8% | 68.41 ms | 75.13 ms | 45% | 3 runs, CV 4.8% → 90% upper bound 14.8% (×3.08) → 3× = 44.5%, rounded up to 45%; floor 15% → 45% |
| Mixed message contains (scan): p95 | lower | 3 | 96.07 ms | 2.29 ms | 2.4% | 93.51 ms | 97.93 ms | – | not gated: p95 from 159 samples in one run; at least 200 are needed for 10 beyond it |
| Mixed message contains (scan): p99 | lower | 3 | 102.97 ms | 4.21 ms | 4.1% | 98.11 ms | 105.57 ms | – | not gated: p99 from 159 samples in one run; at least 1000 are needed for 10 beyond it |
| Mixed histogram, 60 buckets: p50 | lower | 3 | 18.35 ms | 0.157 ms | 0.9% | 18.21 ms | 18.52 ms | 15% | 3 runs, CV 0.9% → 90% upper bound 2.6% (×3.08) → 3× = 7.9%, rounded up to 10%; floor 15% → 15% |
| Mixed histogram, 60 buckets: p95 | lower | 3 | 27.59 ms | 0.254 ms | 0.9% | 27.30 ms | 27.76 ms | – | not gated: p95 from 159 samples in one run; at least 200 are needed for 10 beyond it |
| Mixed histogram, 60 buckets: p99 | lower | 3 | 32.20 ms | 1.50 ms | 4.7% | 31.20 ms | 33.93 ms | – | not gated: p99 from 159 samples in one run; at least 1000 are needed for 10 beyond it |
| Mixed facets: p50 | lower | 3 | 47.64 ms | 1.36 ms | 2.9% | 46.09 ms | 48.60 ms | 30% | 3 runs, CV 2.9% → 90% upper bound 8.8% (×3.08) → 3× = 26.4%, rounded up to 30%; floor 15% → 30% |
| Mixed facets: p95 | lower | 3 | 68.05 ms | 4.69 ms | 6.9% | 62.72 ms | 71.56 ms | – | not gated: 3 runs, CV 6.9% → 90% upper bound 21.2% (×3.08) → 3× = 63.7%, rounded up to 65%; floor 15% → 65% exceeds 50%; too noisy to gate with this many runs on this host |
| Mixed facets: p99 | lower | 3 | 79.84 ms | 1.06 ms | 1.3% | 78.92 ms | 81.00 ms | – | not gated: p99 from 147 samples in one run; at least 1000 are needed for 10 beyond it |
| Mixed trace search (limit 50): p50 | lower | 3 | 3.64 ms | 0.186 ms | 5.1% | 3.48 ms | 3.84 ms | 50% | 3 runs, CV 5.1% → 90% upper bound 15.7% (×3.08) → 3× = 47.2%, rounded up to 50%; floor 15% → 50% |
| Mixed trace search (limit 50): p95 | lower | 3 | 6.72 ms | 0.169 ms | 2.5% | 6.54 ms | 6.88 ms | 25% | 3 runs, CV 2.5% → 90% upper bound 7.7% (×3.08) → 3× = 23.2%, rounded up to 25%; floor 15% → 25% |
| Mixed trace search (limit 50): p99 | lower | 3 | 8.12 ms | 0.439 ms | 5.4% | 7.63 ms | 8.47 ms | – | not gated: p99 from 305 samples in one run; at least 1000 are needed for 10 beyond it |
| Mixed trace by id: p50 | lower | 3 | 0.493 ms | 0.042 ms | 8.5% | 0.452 ms | 0.535 ms | – | not gated: 3 runs, CV 8.5% → 90% upper bound 26.0% (×3.08) → 3× = 78.1%, rounded up to 80%; floor 15% → 80% exceeds 50%; too noisy to gate with this many runs on this host |
| Mixed trace by id: p95 | lower | 3 | 0.864 ms | 0.059 ms | 6.8% | 0.825 ms | 0.931 ms | – | not gated: 3 runs, CV 6.8% → 90% upper bound 21.0% (×3.08) → 3× = 63.0%, rounded up to 65%; floor 15% → 65% exceeds 50%; too noisy to gate with this many runs on this host |
| Mixed trace by id: p99 | lower | 3 | 1.59 ms | 0.944 ms | 59.3% | 0.965 ms | 2.68 ms | – | not gated: 3 runs, CV 59.3% → 90% upper bound 182.8% (×3.08) → 3× = 548.5%, rounded up to 550%; floor 15% → 550% exceeds 50%; too noisy to gate with this many runs on this host |
| Mixed list metric names: p50 | lower | 3 | 0.277 ms | 0.016 ms | 5.7% | 0.259 ms | 0.290 ms | – | not gated: 3 runs, CV 5.7% → 90% upper bound 17.6% (×3.08) → 3× = 52.9%, rounded up to 55%; floor 15% → 55% exceeds 50%; too noisy to gate with this many runs on this host |
| Mixed list metric names: p95 | lower | 3 | 0.565 ms | 0.059 ms | 10.5% | 0.529 ms | 0.634 ms | – | not gated: 3 runs, CV 10.5% → 90% upper bound 32.3% (×3.08) → 3× = 97.0%, rounded up to 100%; floor 15% → 100% exceeds 50%; too noisy to gate with this many runs on this host |
| Mixed list metric names: p99 | lower | 3 | 0.914 ms | 0.155 ms | 16.9% | 0.769 ms | 1.08 ms | – | not gated: 3 runs, CV 16.9% → 90% upper bound 52.1% (×3.08) → 3× = 156.4%, rounded up to 160%; floor 15% → 160% exceeds 50%; too noisy to gate with this many runs on this host |
| Mixed metric series: p50 | lower | 3 | 0.429 ms | 0.012 ms | 2.9% | 0.415 ms | 0.439 ms | 30% | 3 runs, CV 2.9% → 90% upper bound 8.8% (×3.08) → 3× = 26.5%, rounded up to 30%; floor 15% → 30% |
| Mixed metric series: p95 | lower | 3 | 0.759 ms | 0.065 ms | 8.6% | 0.713 ms | 0.833 ms | – | not gated: 3 runs, CV 8.6% → 90% upper bound 26.4% (×3.08) → 3× = 79.1%, rounded up to 80%; floor 15% → 80% exceeds 50%; too noisy to gate with this many runs on this host |
| Mixed metric series: p99 | lower | 3 | 1.18 ms | 0.198 ms | 16.8% | 0.994 ms | 1.39 ms | – | not gated: 3 runs, CV 16.8% → 90% upper bound 51.6% (×3.08) → 3× = 154.9%, rounded up to 155%; floor 15% → 155% exceeds 50%; too noisy to gate with this many runs on this host |
| Peak RSS | lower | 3 | 54.61 MB | 1.36 MB | 2.5% | 53.61 MB | 56.16 MB | 25% | 3 runs, CV 2.5% → 90% upper bound 7.7% (×3.08) → 3× = 23.1%, rounded up to 25%; floor 15% → 25% |
| Restart to healthy (mean) | lower | 3 | 518.52 ms | 0.945 ms | 0.2% | 517.87 ms | 519.60 ms | 15% | 3 runs, CV 0.2% → 90% upper bound 0.6% (×3.08) → 3× = 1.7%, rounded up to 5%; floor 15% → 15% |
| Data directory after seeding | lower | 3 | 7.18 MB | 9.7 KB | 0.1% | 7.17 MB | 7.19 MB | 2% | 3 runs, CV 0.1% → 90% upper bound 0.4% (×3.08) → 3× = 1.2%, rounded up to 2%; floor 2% → 2% |
| Stored bytes per event | lower | 3 | 73.0 B | 0.1 B | 0.1% | 72.9 B | 73.1 B | 2% | 3 runs, CV 0.1% → 90% upper bound 0.4% (×3.08) → 3× = 1.2%, rounded up to 2%; floor 2% → 2% |
| Binary size | lower | 3 | 11.81 MB | 0.0 B | 0.0% | 11.81 MB | 11.81 MB | 2% | 3 runs, CV 0.0% → 90% upper bound 0.0% (×3.08) → 3× = 0.0%, rounded up to 0%; floor 2% → 2% |

## Comparison with 0.1.1 reference (0.1.1, 2026-10-15)

0 regression(s) beyond budget.

| Metric | Baseline | Current | Worse by | Budget | Verdict |
| --- | ---: | ---: | ---: | ---: | --- |
| Seeding throughput | 70,921 items/s | 65,543 items/s | +7.6% | – | not gated |
| Seeding request p95 | 3.56 ms | 3.76 ms | +5.6% | 20% | within budget |
| level = Error (bitmap): cold | 29.93 ms | 23.08 ms | -22.9% | – | not gated |
| level = Error (bitmap): warm p50 | 19.99 ms | 20.14 ms | +0.7% | 15% | within budget |
| level = Error (bitmap): warm p95 | 24.71 ms | 22.71 ms | -8.1% | – | not gated |
| service = payments: cold | 8.97 ms | 15.42 ms | +72.0% | – | not gated |
| service = payments: warm p50 | 7.15 ms | 7.46 ms | +4.4% | 35% | within budget |
| service = payments: warm p95 | 8.78 ms | 8.88 ms | +1.1% | – | not gated |
| traceId = … (Bloom filter): cold | 2.40 ms | 1.96 ms | -18.1% | – | not gated |
| traceId = … (Bloom filter): warm p50 | 0.611 ms | 0.508 ms | -16.9% | – | not gated |
| traceId = … (Bloom filter): warm p95 | 0.963 ms | 1.04 ms | +7.6% | – | not gated |
| durationMs > 1990 (zone map): cold | 104.31 ms | 102.45 ms | -1.8% | 15% | within budget |
| durationMs > 1990 (zone map): warm p50 | 104.31 ms | 105.13 ms | +0.8% | 15% | within budget |
| durationMs > 1990 (zone map): warm p95 | 111.17 ms | 108.28 ms | -2.6% | – | not gated |
| count: level = Error: cold | 6.24 ms | 5.32 ms | -14.7% | – | not gated |
| count: level = Error: warm p50 | 3.61 ms | 3.63 ms | +0.7% | 15% | within budget |
| count: level = Error: warm p95 | 4.59 ms | 4.77 ms | +3.9% | – | not gated |
| message contains "timed out" (scan): cold | 42.92 ms | 50.69 ms | +18.1% | – | not gated |
| message contains "timed out" (scan): warm p50 | 37.90 ms | 38.86 ms | +2.5% | 15% | within budget |
| message contains "timed out" (scan): warm p95 | 40.52 ms | 40.96 ms | +1.1% | – | not gated |
| message contains "error" (scan): cold | 16.37 ms | 16.86 ms | +3.0% | 45% | within budget |
| message contains "error" (scan): warm p50 | 15.04 ms | 15.99 ms | +6.3% | 30% | within budget |
| message contains "error" (scan): warm p95 | 17.33 ms | 18.45 ms | +6.5% | – | not gated |
| latest logs (limit 200): cold | 7.32 ms | 7.22 ms | -1.4% | – | not gated |
| latest logs (limit 200): warm p50 | 6.16 ms | 6.30 ms | +2.3% | 30% | within budget |
| latest logs (limit 200): warm p95 | 7.44 ms | 7.99 ms | +7.4% | – | not gated |
| histogram, 60 buckets: cold | 5.97 ms | 4.99 ms | -16.3% | – | not gated |
| histogram, 60 buckets: warm p50 | 3.72 ms | 3.64 ms | -2.2% | 15% | within budget |
| histogram, 60 buckets: warm p95 | 4.80 ms | 4.37 ms | -9.1% | – | not gated |
| facets, 2000-event sample: cold | 21.15 ms | 21.61 ms | +2.2% | – | not gated |
| facets, 2000-event sample: warm p50 | 19.49 ms | 19.48 ms | 0.0% | 25% | within budget |
| facets, 2000-event sample: warm p95 | 22.05 ms | 21.71 ms | -1.5% | – | not gated |
| trace search (limit 50): cold | 4.23 ms | 4.38 ms | +3.7% | – | not gated |
| trace search (limit 50): warm p50 | 2.36 ms | 2.48 ms | +4.9% | 45% | within budget |
| trace search (limit 50): warm p95 | 3.41 ms | 3.24 ms | -4.9% | – | not gated |
| trace by id: cold | 0.883 ms | 1.04 ms | +17.6% | – | not gated |
| trace by id: warm p50 | 0.296 ms | 0.393 ms | +32.7% | – | not gated |
| trace by id: warm p95 | 0.554 ms | 0.634 ms | +14.5% | – | not gated |
| metric series: cold | 1.07 ms | 1.10 ms | +3.1% | – | not gated |
| metric series: warm p50 | 0.395 ms | 0.372 ms | -5.7% | – | not gated |
| metric series: warm p95 | 0.641 ms | 0.550 ms | -14.2% | – | not gated |
| Mixed load throughput | 221 req/s | 217 req/s | +1.7% | 20% | within budget |
| Mixed ingest NDJSON (800 events): p50 | 5.24 ms | 5.68 ms | +8.5% | 35% | within budget |
| Mixed ingest NDJSON (800 events): p95 | 15.15 ms | 16.00 ms | +5.6% | – | not gated |
| Mixed ingest NDJSON (800 events): p99 | 26.03 ms | 23.17 ms | -11.0% | – | not gated |
| Mixed ingest JSON array (800 events): p50 | 5.72 ms | 5.94 ms | +3.8% | 40% | within budget |
| Mixed ingest JSON array (800 events): p95 | 18.72 ms | 16.66 ms | -11.0% | – | not gated |
| Mixed ingest JSON array (800 events): p99 | 28.03 ms | 26.58 ms | -5.2% | – | not gated |
| Mixed latest logs (limit 200): p50 | 17.82 ms | 18.55 ms | +4.1% | 30% | within budget |
| Mixed latest logs (limit 200): p95 | 25.87 ms | 25.59 ms | -1.1% | 40% | within budget |
| Mixed latest logs (limit 200): p99 | 29.04 ms | 29.18 ms | +0.5% | – | not gated |
| Mixed count: level = Error: p50 | 18.21 ms | 18.78 ms | +3.1% | 35% | within budget |
| Mixed count: level = Error: p95 | 28.65 ms | 27.95 ms | -2.5% | 35% | within budget |
| Mixed count: level = Error: p99 | 40.16 ms | 31.86 ms | -20.7% | – | not gated |
| Mixed level = Error: p50 | 42.85 ms | 43.79 ms | +2.2% | 20% | within budget |
| Mixed level = Error: p95 | 55.58 ms | 55.45 ms | -0.2% | 30% | within budget |
| Mixed level = Error: p99 | 63.74 ms | 63.32 ms | -0.7% | – | not gated |
| Mixed service = payments: p50 | 18.62 ms | 19.44 ms | +4.4% | 30% | within budget |
| Mixed service = payments: p95 | 27.25 ms | 26.83 ms | -1.5% | 45% | within budget |
| Mixed service = payments: p99 | 31.43 ms | 30.54 ms | -2.8% | – | not gated |
| Mixed traceId (Bloom filter): p50 | 1.04 ms | 1.04 ms | +0.5% | 40% | within budget |
| Mixed traceId (Bloom filter): p95 | 1.69 ms | 1.66 ms | -2.1% | – | not gated |
| Mixed traceId (Bloom filter): p99 | 2.18 ms | 2.41 ms | +10.4% | – | not gated |
| Mixed message contains (scan): p50 | 69.13 ms | 72.31 ms | +4.6% | 20% | within budget |
| Mixed message contains (scan): p95 | 95.18 ms | 96.07 ms | +0.9% | – | not gated |
| Mixed message contains (scan): p99 | 128.79 ms | 102.97 ms | -20.0% | – | not gated |
| Mixed histogram, 60 buckets: p50 | 17.72 ms | 18.35 ms | +3.6% | 30% | within budget |
| Mixed histogram, 60 buckets: p95 | 30.03 ms | 27.59 ms | -8.1% | – | not gated |
| Mixed histogram, 60 buckets: p99 | 51.51 ms | 32.20 ms | -37.5% | – | not gated |
| Mixed facets: p50 | 44.71 ms | 47.64 ms | +6.6% | 25% | within budget |
| Mixed facets: p95 | 66.43 ms | 68.05 ms | +2.4% | – | not gated |
| Mixed facets: p99 | 97.30 ms | 79.84 ms | -18.0% | – | not gated |
| Mixed trace search (limit 50): p50 | 3.48 ms | 3.64 ms | +4.7% | – | not gated |
| Mixed trace search (limit 50): p95 | 6.52 ms | 6.72 ms | +3.0% | 40% | within budget |
| Mixed trace search (limit 50): p99 | 7.55 ms | 8.12 ms | +7.6% | – | not gated |
| Mixed trace by id: p50 | 0.477 ms | 0.493 ms | +3.4% | 30% | within budget |
| Mixed trace by id: p95 | 0.835 ms | 0.864 ms | +3.5% | – | not gated |
| Mixed trace by id: p99 | 2.97 ms | 1.59 ms | -46.5% | – | not gated |
| Mixed list metric names: p50 | 0.281 ms | 0.277 ms | -1.4% | 40% | within budget |
| Mixed list metric names: p95 | 0.575 ms | 0.565 ms | -1.7% | – | not gated |
| Mixed list metric names: p99 | 1.14 ms | 0.914 ms | -19.9% | – | not gated |
| Mixed metric series: p50 | 0.433 ms | 0.429 ms | -1.1% | 25% | within budget |
| Mixed metric series: p95 | 0.716 ms | 0.759 ms | +6.0% | – | not gated |
| Mixed metric series: p99 | 0.943 ms | 1.18 ms | +25.5% | – | not gated |
| Peak RSS | 54.42 MB | 54.61 MB | +0.3% | 20% | within budget |
| Restart to healthy (mean) | 518.71 ms | 518.52 ms | 0.0% | 15% | within budget |
| Data directory after seeding | 7.17 MB | 7.18 MB | +0.1% | 2% | within budget |
| Stored bytes per event | 72.9 B | 73.0 B | +0.1% | 2% | within budget |
| Binary size | 11.81 MB | 11.81 MB | 0.0% | 2% | within budget |

## Raw data

- `2026-10-15-windows-laptop-same-commit-check.json`: every run's measurements, diagnostics and the effective server configuration.
- `check.samples.csv` (not committed): every request's latency and HTTP status.
