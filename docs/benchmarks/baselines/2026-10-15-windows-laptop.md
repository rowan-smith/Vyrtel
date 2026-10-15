# Vyrtel performance baseline

- Generated: 2026-10-15 11:59:47 UTC
- Label: 0.1.1 reference
- Vyrtel: 0.1.1 at `b61cd28761aa` (uncommitted changes)
- Duration: 5.5 min for 5 run(s) (66 s, 65 s, 65 s, 65 s, 66 s)
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
| Compression | zstd level 3, independently compressed column chunks: 4.01× ratio, 7.6% index overhead, 73.0 bytes/event, 22 segments, 7.2 MB data directory |

## Memory

| Run | Peak RSS | Idle after start | Seeding peak | Query phase peak | Mixed load peak | Source |
| ---: | ---: | ---: | ---: | ---: | ---: | --- |
| 1 | 56.0 MB | 12.4 MB | 30.7 MB | 21.0 MB | 56.0 MB | os-peak |
| 2 | 52.8 MB | 12.4 MB | 32.4 MB | 21.4 MB | 52.8 MB | os-peak |
| 3 | 56.0 MB | 12.3 MB | 32.6 MB | 20.9 MB | 56.0 MB | os-peak |
| 4 | 54.0 MB | 12.4 MB | 33.6 MB | 21.0 MB | 54.0 MB | os-peak |
| 5 | 53.2 MB | 12.4 MB | 32.0 MB | 21.5 MB | 53.2 MB | os-peak |

## Accepted and rejected requests

Backpressure is 429/503 (retryable); seeding retries it, so the dataset always arrives complete.

| Run | Phase | Requests | Accepted | Backpressure | Too large (413) | Other rejected | Transport errors | Items accepted | Throughput | p50 | p95 | p99 |
| ---: | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 1 | seeding | 1,201 | 1,201 | 0 | 0 | 0 | 0 | 101,100 | 57,553 items/s | 0.178 ms | 3.41 ms | 4.22 ms |
| 1 | mixed | 3,754 | 3,754 | 0 | 0 | 0 | 0 | 329,600 | 221 req/s | | | |
| 2 | seeding | 1,201 | 1,201 | 0 | 0 | 0 | 0 | 101,100 | 77,520 items/s | 0.156 ms | 3.62 ms | 4.62 ms |
| 2 | mixed | 3,597 | 3,597 | 0 | 0 | 0 | 0 | 312,000 | 212 req/s | | | |
| 3 | seeding | 1,201 | 1,201 | 0 | 0 | 0 | 0 | 101,100 | 73,878 items/s | 0.166 ms | 3.56 ms | 4.98 ms |
| 3 | mixed | 3,876 | 3,876 | 0 | 0 | 0 | 0 | 339,200 | 228 req/s | | | |
| 4 | seeding | 1,201 | 1,201 | 0 | 0 | 0 | 0 | 101,100 | 76,701 items/s | 0.152 ms | 3.53 ms | 5.34 ms |
| 4 | mixed | 3,757 | 3,757 | 0 | 0 | 0 | 0 | 328,000 | 221 req/s | | | |
| 5 | seeding | 1,201 | 1,201 | 0 | 0 | 0 | 0 | 101,100 | 68,951 items/s | 0.248 ms | 3.70 ms | 4.85 ms |
| 5 | mixed | 3,783 | 3,783 | 0 | 0 | 0 | 0 | 329,600 | 223 req/s | | | |

## Mixed load by operation

Mean across runs; ± is the run-to-run coefficient of variation.

| Operation | Requests (run 1) | Rejected (all runs) | p50 | p95 | p99 |
| --- | ---: | ---: | ---: | ---: | ---: |
| ingest NDJSON (800 events) | 257 | 0 | 5.24 ms ±6% | 15.15 ms ±13% | 26.03 ms ±9% |
| ingest JSON array (800 events) | 155 | 0 | 5.72 ms ±6% | 18.72 ms ±17% | 28.03 ms ±6% |
| latest logs (limit 200) | 556 | 0 | 17.82 ms ±4% | 25.87 ms ±7% | 29.04 ms ±8% |
| count: level = Error | 481 | 0 | 18.21 ms ±5% | 28.65 ms ±5% | 40.16 ms ±33% |
| level = Error | 378 | 0 | 42.85 ms ±3% | 55.58 ms ±5% | 63.74 ms ±11% |
| service = payments | 344 | 0 | 18.62 ms ±5% | 27.25 ms ±7% | 31.43 ms ±8% |
| traceId (Bloom filter) | 325 | 0 | 1.04 ms ±6% | 1.69 ms ±10% | 2.18 ms ±10% |
| message contains (scan) | 165 | 0 | 69.13 ms ±3% | 95.18 ms ±7% | 128.79 ms ±30% |
| histogram, 60 buckets | 170 | 0 | 17.72 ms ±5% | 30.03 ms ±12% | 51.51 ms ±53% |
| facets | 153 | 0 | 44.71 ms ±4% | 66.43 ms ±7% | 97.30 ms ±42% |
| trace search (limit 50) | 316 | 0 | 3.48 ms ±10% | 6.52 ms ±6% | 7.55 ms ±9% |
| trace by id | 167 | 0 | 0.477 ms ±5% | 0.835 ms ±5% | 2.97 ms ±137% |
| list metric names | 160 | 0 | 0.281 ms ±7% | 0.575 ms ±8% | 1.14 ms ±74% |
| metric series | 127 | 0 | 0.433 ms ±4% | 0.716 ms ±8% | 0.943 ms ±16% |

## Queries: cold and warm

Cold is the first request after a server restart (no index cache, nothing decoded; the OS page cache is not dropped). Warm is the following repetitions. Diagnostics are the server's own, from run 1.

| Query | Kind | Cold | Warm p50 | Warm p95 | Cold ÷ warm | Segments (skipped/considered) | Blocks read (skipped) | Events examined → matched | Bytes read cold → warm | Indexes |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | --- |
| level = Error (bitmap) | selective | 29.93 ms ±31% | 19.99 ms ±2% | 24.71 ms ±16% | 1.5× | 0/22 | 114 (0) | 31,570 → 611 | 3.0 MB → 3.0 MB | timestamp:time, level:bitmap |
| service = payments | selective | 8.97 ms ±21% | 7.15 ms ±5% | 8.78 ms ±10% | 1.3× | 0/22 | 50 (0) | 6,444 → 815 | 822.1 KB → 822.1 KB | timestamp:time, service:bitmap |
| traceId = … (Bloom filter) | selective | 2.40 ms ±21% | 0.611 ms ±13% | 0.963 ms ±14% | 3.9× | 20/22 | 3 (108) | 1,808 → 1 | 44.3 KB → 44.3 KB | timestamp:time, traceId:bloom |
| durationMs > 1990 (zone map) | selective | 104.31 ms ±3% | 104.31 ms ±2% | 111.17 ms ±3% | 1.0× | 0/22 | 88 (22) | 82,432 → 182 | 4.9 MB → 4.9 MB | timestamp:time, durationMs:zonemap |
| count: level = Error | selective | 6.24 ms ±34% | 3.61 ms ±1% | 4.59 ms ±10% | 1.7× | 0/22 | 110 (0) | 100,000 → 1,981 | 476.7 KB → 476.7 KB | timestamp:time, level:bitmap |
| message contains "timed out" (scan) | scan | 42.92 ms ±13% | 37.90 ms ±2% | 40.52 ms ±2% | 1.1× | 0/22 | 177 (0) | 76,002 → 368 | 5.4 MB → 5.4 MB | timestamp:time, message:none |
| message contains "error" (scan) | scan | 16.37 ms ±7% | 15.04 ms ±5% | 17.33 ms ±8% | 1.1× | 0/22 | 110 (0) | 100,000 → 0 | 1,018.5 KB → 1,018.5 KB | timestamp:time, message:none |
| latest logs (limit 200) | recent | 7.32 ms ±11% | 6.16 ms ±5% | 7.44 ms ±7% | 1.2× | 0/22 | 44 (0) | 1,212 → 1,212 | 603.5 KB → 603.5 KB | timestamp:time |
| histogram, 60 buckets | aggregate | 5.97 ms ±39% | 3.72 ms ±2% | 4.80 ms ±9% | 1.6× | 0/22 | 110 (0) | 100,000 → 100,000 | 476.7 KB → 476.7 KB | timestamp:time |
| facets, 2000-event sample | aggregate | 21.15 ms ±10% | 19.49 ms ±4% | 22.05 ms ±7% | 1.1× | 0/22 | 54 (0) | 7,794 → 7,794 | 945.5 KB → 945.5 KB | timestamp:time |
| trace search (limit 50) | trace | 4.23 ms ±35% | 2.36 ms ±7% | 3.41 ms ±13% | 1.8× | 0/0 | 0 (0) | 4,515 → 1,665 | 0 B → 0 B | timestamp:time |
| trace by id | trace | 0.883 ms ±29% | 0.296 ms ±35% | 0.554 ms ±19% | 3.0× | 0/0 | 0 (0) | 3,000 → 3 | 0 B → 0 B | timestamp:time |
| metric series | metric | 1.07 ms ±55% | 0.395 ms ±48% | 0.641 ms ±28% | 2.7× | 0/0 | 0 (0) | 100 → 100 | 0 B → 0 B | timestamp:time, message:none |

## Run-to-run variance and proposed regression budgets

budget = max(floor, 3 × CV × k(n) rounded up), where k(n) turns the CV of n runs into a 90 % upper confidence bound; floor 15% for timings and memory, 2% for sizes; not gated with fewer than 3 runs or a budget over 50%. See docs/benchmarks.md#regression-budgets.

| Metric | Better | Runs | Mean | Std dev | CV | Min | Max | Budget | Justification |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | --- |
| Seeding throughput | higher | 5 | 70,921 items/s | 8,190 items/s | 11.5% | 57,553 items/s | 77,520 items/s | – | not gated: 5 runs, CV 11.5% → 90% upper bound 22.4% (×1.94) → 3× = 67.2%, rounded up to 70%; floor 15% → 70% exceeds 50%; too noisy to gate with this many runs on this host |
| Seeding request p95 | lower | 5 | 3.56 ms | 0.108 ms | 3.0% | 3.41 ms | 3.70 ms | 20% | 5 runs, CV 3.0% → 90% upper bound 5.9% (×1.94) → 3× = 17.6%, rounded up to 20%; floor 15% → 20% |
| level = Error (bitmap): cold | lower | 5 | 29.93 ms | 9.29 ms | 31.0% | 19.32 ms | 39.36 ms | – | not gated: 5 runs, CV 31.0% → 90% upper bound 60.2% (×1.94) → 3× = 180.5%, rounded up to 185%; floor 15% → 185% exceeds 50%; too noisy to gate with this many runs on this host |
| level = Error (bitmap): warm p50 | lower | 5 | 19.99 ms | 0.360 ms | 1.8% | 19.68 ms | 20.50 ms | 15% | 5 runs, CV 1.8% → 90% upper bound 3.5% (×1.94) → 3× = 10.5%, rounded up to 15%; floor 15% → 15% |
| level = Error (bitmap): warm p95 | lower | 5 | 24.71 ms | 3.95 ms | 16.0% | 22.18 ms | 31.56 ms | – | not gated: 5 runs, CV 16.0% → 90% upper bound 31.0% (×1.94) → 3× = 93.0%, rounded up to 95%; floor 15% → 95% exceeds 50%; too noisy to gate with this many runs on this host |
| service = payments: cold | lower | 5 | 8.97 ms | 1.85 ms | 20.6% | 7.75 ms | 12.21 ms | – | not gated: 5 runs, CV 20.6% → 90% upper bound 40.0% (×1.94) → 3× = 120.0%, rounded up to 120%; floor 15% → 120% exceeds 50%; too noisy to gate with this many runs on this host |
| service = payments: warm p50 | lower | 5 | 7.15 ms | 0.392 ms | 5.5% | 6.67 ms | 7.62 ms | 35% | 5 runs, CV 5.5% → 90% upper bound 10.6% (×1.94) → 3× = 31.9%, rounded up to 35%; floor 15% → 35% |
| service = payments: warm p95 | lower | 5 | 8.78 ms | 0.843 ms | 9.6% | 7.98 ms | 10.05 ms | – | not gated: 5 runs, CV 9.6% → 90% upper bound 18.6% (×1.94) → 3× = 55.8%, rounded up to 60%; floor 15% → 60% exceeds 50%; too noisy to gate with this many runs on this host |
| traceId = … (Bloom filter): cold | lower | 5 | 2.40 ms | 0.506 ms | 21.1% | 1.60 ms | 2.84 ms | – | not gated: 5 runs, CV 21.1% → 90% upper bound 40.9% (×1.94) → 3× = 122.7%, rounded up to 125%; floor 15% → 125% exceeds 50%; too noisy to gate with this many runs on this host |
| traceId = … (Bloom filter): warm p50 | lower | 5 | 0.611 ms | 0.080 ms | 13.0% | 0.541 ms | 0.746 ms | – | not gated: 5 runs, CV 13.0% → 90% upper bound 25.3% (×1.94) → 3× = 75.9%, rounded up to 80%; floor 15% → 80% exceeds 50%; too noisy to gate with this many runs on this host |
| traceId = … (Bloom filter): warm p95 | lower | 5 | 0.963 ms | 0.139 ms | 14.4% | 0.831 ms | 1.18 ms | – | not gated: 5 runs, CV 14.4% → 90% upper bound 28.0% (×1.94) → 3× = 84.0%, rounded up to 85%; floor 15% → 85% exceeds 50%; too noisy to gate with this many runs on this host |
| durationMs > 1990 (zone map): cold | lower | 5 | 104.31 ms | 2.65 ms | 2.5% | 100.77 ms | 107.71 ms | 15% | 5 runs, CV 2.5% → 90% upper bound 4.9% (×1.94) → 3× = 14.8%, rounded up to 15%; floor 15% → 15% |
| durationMs > 1990 (zone map): warm p50 | lower | 5 | 104.31 ms | 2.06 ms | 2.0% | 102.23 ms | 107.30 ms | 15% | 5 runs, CV 2.0% → 90% upper bound 3.8% (×1.94) → 3× = 11.5%, rounded up to 15%; floor 15% → 15% |
| durationMs > 1990 (zone map): warm p95 | lower | 5 | 111.17 ms | 3.41 ms | 3.1% | 106.32 ms | 115.39 ms | – | not gated: p95 from 20 samples in one run; at least 200 are needed for 10 beyond it |
| count: level = Error: cold | lower | 5 | 6.24 ms | 2.14 ms | 34.3% | 4.53 ms | 9.88 ms | – | not gated: 5 runs, CV 34.3% → 90% upper bound 66.5% (×1.94) → 3× = 199.5%, rounded up to 200%; floor 15% → 200% exceeds 50%; too noisy to gate with this many runs on this host |
| count: level = Error: warm p50 | lower | 5 | 3.61 ms | 0.027 ms | 0.7% | 3.58 ms | 3.64 ms | 15% | 5 runs, CV 0.7% → 90% upper bound 1.4% (×1.94) → 3× = 4.3%, rounded up to 5%; floor 15% → 15% |
| count: level = Error: warm p95 | lower | 5 | 4.59 ms | 0.444 ms | 9.7% | 3.84 ms | 5.02 ms | – | not gated: 5 runs, CV 9.7% → 90% upper bound 18.8% (×1.94) → 3× = 56.3%, rounded up to 60%; floor 15% → 60% exceeds 50%; too noisy to gate with this many runs on this host |
| message contains "timed out" (scan): cold | lower | 5 | 42.92 ms | 5.67 ms | 13.2% | 38.08 ms | 51.13 ms | – | not gated: 5 runs, CV 13.2% → 90% upper bound 25.6% (×1.94) → 3× = 76.9%, rounded up to 80%; floor 15% → 80% exceeds 50%; too noisy to gate with this many runs on this host |
| message contains "timed out" (scan): warm p50 | lower | 5 | 37.90 ms | 0.913 ms | 2.4% | 36.46 ms | 38.64 ms | 15% | 5 runs, CV 2.4% → 90% upper bound 4.7% (×1.94) → 3× = 14.0%, rounded up to 15%; floor 15% → 15% |
| message contains "timed out" (scan): warm p95 | lower | 5 | 40.52 ms | 0.751 ms | 1.9% | 39.97 ms | 41.80 ms | – | not gated: p95 from 20 samples in one run; at least 200 are needed for 10 beyond it |
| message contains "error" (scan): cold | lower | 5 | 16.37 ms | 1.19 ms | 7.3% | 14.50 ms | 17.67 ms | 45% | 5 runs, CV 7.3% → 90% upper bound 14.1% (×1.94) → 3× = 42.2%, rounded up to 45%; floor 15% → 45% |
| message contains "error" (scan): warm p50 | lower | 5 | 15.04 ms | 0.747 ms | 5.0% | 14.51 ms | 16.32 ms | 30% | 5 runs, CV 5.0% → 90% upper bound 9.6% (×1.94) → 3× = 28.9%, rounded up to 30%; floor 15% → 30% |
| message contains "error" (scan): warm p95 | lower | 5 | 17.33 ms | 1.32 ms | 7.6% | 15.59 ms | 19.28 ms | – | not gated: p95 from 20 samples in one run; at least 200 are needed for 10 beyond it |
| latest logs (limit 200): cold | lower | 5 | 7.32 ms | 0.834 ms | 11.4% | 6.54 ms | 8.55 ms | – | not gated: 5 runs, CV 11.4% → 90% upper bound 22.1% (×1.94) → 3× = 66.3%, rounded up to 70%; floor 15% → 70% exceeds 50%; too noisy to gate with this many runs on this host |
| latest logs (limit 200): warm p50 | lower | 5 | 6.16 ms | 0.315 ms | 5.1% | 5.85 ms | 6.62 ms | 30% | 5 runs, CV 5.1% → 90% upper bound 9.9% (×1.94) → 3× = 29.8%, rounded up to 30%; floor 15% → 30% |
| latest logs (limit 200): warm p95 | lower | 5 | 7.44 ms | 0.512 ms | 6.9% | 6.79 ms | 8.13 ms | – | not gated: p95 from 20 samples in one run; at least 200 are needed for 10 beyond it |
| histogram, 60 buckets: cold | lower | 5 | 5.97 ms | 2.31 ms | 38.8% | 4.31 ms | 10.00 ms | – | not gated: 5 runs, CV 38.8% → 90% upper bound 75.2% (×1.94) → 3× = 225.5%, rounded up to 230%; floor 15% → 230% exceeds 50%; too noisy to gate with this many runs on this host |
| histogram, 60 buckets: warm p50 | lower | 5 | 3.72 ms | 0.075 ms | 2.0% | 3.66 ms | 3.84 ms | 15% | 5 runs, CV 2.0% → 90% upper bound 3.9% (×1.94) → 3× = 11.7%, rounded up to 15%; floor 15% → 15% |
| histogram, 60 buckets: warm p95 | lower | 5 | 4.80 ms | 0.433 ms | 9.0% | 4.13 ms | 5.30 ms | – | not gated: 5 runs, CV 9.0% → 90% upper bound 17.5% (×1.94) → 3× = 52.4%, rounded up to 55%; floor 15% → 55% exceeds 50%; too noisy to gate with this many runs on this host |
| facets, 2000-event sample: cold | lower | 5 | 21.15 ms | 2.19 ms | 10.3% | 19.15 ms | 24.35 ms | – | not gated: 5 runs, CV 10.3% → 90% upper bound 20.0% (×1.94) → 3× = 60.1%, rounded up to 65%; floor 15% → 65% exceeds 50%; too noisy to gate with this many runs on this host |
| facets, 2000-event sample: warm p50 | lower | 5 | 19.49 ms | 0.793 ms | 4.1% | 18.81 ms | 20.58 ms | 25% | 5 runs, CV 4.1% → 90% upper bound 7.9% (×1.94) → 3× = 23.7%, rounded up to 25%; floor 15% → 25% |
| facets, 2000-event sample: warm p95 | lower | 5 | 22.05 ms | 1.49 ms | 6.8% | 20.79 ms | 24.55 ms | – | not gated: p95 from 20 samples in one run; at least 200 are needed for 10 beyond it |
| trace search (limit 50): cold | lower | 5 | 4.23 ms | 1.46 ms | 34.5% | 3.13 ms | 6.08 ms | – | not gated: 5 runs, CV 34.5% → 90% upper bound 67.0% (×1.94) → 3× = 200.9%, rounded up to 205%; floor 15% → 205% exceeds 50%; too noisy to gate with this many runs on this host |
| trace search (limit 50): warm p50 | lower | 5 | 2.36 ms | 0.173 ms | 7.3% | 2.16 ms | 2.64 ms | 45% | 5 runs, CV 7.3% → 90% upper bound 14.2% (×1.94) → 3× = 42.5%, rounded up to 45%; floor 15% → 45% |
| trace search (limit 50): warm p95 | lower | 5 | 3.41 ms | 0.451 ms | 13.2% | 2.88 ms | 3.84 ms | – | not gated: 5 runs, CV 13.2% → 90% upper bound 25.7% (×1.94) → 3× = 77.1%, rounded up to 80%; floor 15% → 80% exceeds 50%; too noisy to gate with this many runs on this host |
| trace by id: cold | lower | 5 | 0.883 ms | 0.253 ms | 28.7% | 0.544 ms | 1.25 ms | – | not gated: 5 runs, CV 28.7% → 90% upper bound 55.7% (×1.94) → 3× = 167.1%, rounded up to 170%; floor 15% → 170% exceeds 50%; too noisy to gate with this many runs on this host |
| trace by id: warm p50 | lower | 5 | 0.296 ms | 0.103 ms | 34.9% | 0.175 ms | 0.440 ms | – | not gated: 5 runs, CV 34.9% → 90% upper bound 67.6% (×1.94) → 3× = 202.8%, rounded up to 205%; floor 15% → 205% exceeds 50%; too noisy to gate with this many runs on this host |
| trace by id: warm p95 | lower | 5 | 0.554 ms | 0.104 ms | 18.7% | 0.433 ms | 0.710 ms | – | not gated: 5 runs, CV 18.7% → 90% upper bound 36.3% (×1.94) → 3× = 109.0%, rounded up to 110%; floor 15% → 110% exceeds 50%; too noisy to gate with this many runs on this host |
| metric series: cold | lower | 5 | 1.07 ms | 0.586 ms | 55.0% | 0.572 ms | 2.00 ms | – | not gated: 5 runs, CV 55.0% → 90% upper bound 106.7% (×1.94) → 3× = 320.0%, rounded up to 325%; floor 15% → 325% exceeds 50%; too noisy to gate with this many runs on this host |
| metric series: warm p50 | lower | 5 | 0.395 ms | 0.188 ms | 47.6% | 0.248 ms | 0.717 ms | – | not gated: 5 runs, CV 47.6% → 90% upper bound 92.3% (×1.94) → 3× = 276.9%, rounded up to 280%; floor 15% → 280% exceeds 50%; too noisy to gate with this many runs on this host |
| metric series: warm p95 | lower | 5 | 0.641 ms | 0.182 ms | 28.5% | 0.346 ms | 0.794 ms | – | not gated: 5 runs, CV 28.5% → 90% upper bound 55.2% (×1.94) → 3× = 165.6%, rounded up to 170%; floor 15% → 170% exceeds 50%; too noisy to gate with this many runs on this host |
| Mixed load throughput | higher | 5 | 221 req/s | 6 req/s | 2.7% | 212 req/s | 228 req/s | 20% | 5 runs, CV 2.7% → 90% upper bound 5.2% (×1.94) → 3× = 15.6%, rounded up to 20%; floor 15% → 20% |
| Mixed ingest NDJSON (800 events): p50 | lower | 5 | 5.24 ms | 0.297 ms | 5.7% | 4.82 ms | 5.55 ms | 35% | 5 runs, CV 5.7% → 90% upper bound 11.0% (×1.94) → 3× = 33.0%, rounded up to 35%; floor 15% → 35% |
| Mixed ingest NDJSON (800 events): p95 | lower | 5 | 15.15 ms | 1.91 ms | 12.6% | 13.40 ms | 17.99 ms | – | not gated: 5 runs, CV 12.6% → 90% upper bound 24.5% (×1.94) → 3× = 73.4%, rounded up to 75%; floor 15% → 75% exceeds 50%; too noisy to gate with this many runs on this host |
| Mixed ingest NDJSON (800 events): p99 | lower | 5 | 26.03 ms | 2.40 ms | 9.2% | 23.99 ms | 29.96 ms | – | not gated: 5 runs, CV 9.2% → 90% upper bound 17.9% (×1.94) → 3× = 53.7%, rounded up to 55%; floor 15% → 55% exceeds 50%; too noisy to gate with this many runs on this host |
| Mixed ingest JSON array (800 events): p50 | lower | 5 | 5.72 ms | 0.370 ms | 6.5% | 5.34 ms | 6.29 ms | 40% | 5 runs, CV 6.5% → 90% upper bound 12.5% (×1.94) → 3× = 37.6%, rounded up to 40%; floor 15% → 40% |
| Mixed ingest JSON array (800 events): p95 | lower | 5 | 18.72 ms | 3.15 ms | 16.8% | 16.04 ms | 23.88 ms | – | not gated: 5 runs, CV 16.8% → 90% upper bound 32.6% (×1.94) → 3× = 97.9%, rounded up to 100%; floor 15% → 100% exceeds 50%; too noisy to gate with this many runs on this host |
| Mixed ingest JSON array (800 events): p99 | lower | 5 | 28.03 ms | 1.56 ms | 5.6% | 26.75 ms | 30.15 ms | – | not gated: p99 from 147 samples in one run; at least 1000 are needed for 10 beyond it |
| Mixed latest logs (limit 200): p50 | lower | 5 | 17.82 ms | 0.769 ms | 4.3% | 17.23 ms | 19.16 ms | 30% | 5 runs, CV 4.3% → 90% upper bound 8.4% (×1.94) → 3× = 25.1%, rounded up to 30%; floor 15% → 30% |
| Mixed latest logs (limit 200): p95 | lower | 5 | 25.87 ms | 1.70 ms | 6.6% | 24.54 ms | 28.20 ms | 40% | 5 runs, CV 6.6% → 90% upper bound 12.8% (×1.94) → 3× = 38.3%, rounded up to 40%; floor 15% → 40% |
| Mixed latest logs (limit 200): p99 | lower | 5 | 29.04 ms | 2.19 ms | 7.6% | 27.01 ms | 31.76 ms | – | not gated: p99 from 540 samples in one run; at least 1000 are needed for 10 beyond it |
| Mixed count: level = Error: p50 | lower | 5 | 18.21 ms | 0.976 ms | 5.4% | 16.55 ms | 19.11 ms | 35% | 5 runs, CV 5.4% → 90% upper bound 10.4% (×1.94) → 3× = 31.2%, rounded up to 35%; floor 15% → 35% |
| Mixed count: level = Error: p95 | lower | 5 | 28.65 ms | 1.49 ms | 5.2% | 26.66 ms | 30.82 ms | 35% | 5 runs, CV 5.2% → 90% upper bound 10.1% (×1.94) → 3× = 30.3%, rounded up to 35%; floor 15% → 35% |
| Mixed count: level = Error: p99 | lower | 5 | 40.16 ms | 13.37 ms | 33.3% | 32.39 ms | 63.58 ms | – | not gated: 5 runs, CV 33.3% → 90% upper bound 64.6% (×1.94) → 3× = 193.7%, rounded up to 195%; floor 15% → 195% exceeds 50%; too noisy to gate with this many runs on this host |
| Mixed level = Error: p50 | lower | 5 | 42.85 ms | 1.45 ms | 3.4% | 41.66 ms | 45.20 ms | 20% | 5 runs, CV 3.4% → 90% upper bound 6.6% (×1.94) → 3× = 19.7%, rounded up to 20%; floor 15% → 20% |
| Mixed level = Error: p95 | lower | 5 | 55.58 ms | 2.72 ms | 4.9% | 52.67 ms | 59.04 ms | 30% | 5 runs, CV 4.9% → 90% upper bound 9.5% (×1.94) → 3× = 28.5%, rounded up to 30%; floor 15% → 30% |
| Mixed level = Error: p99 | lower | 5 | 63.74 ms | 6.76 ms | 10.6% | 56.38 ms | 72.63 ms | – | not gated: 5 runs, CV 10.6% → 90% upper bound 20.6% (×1.94) → 3× = 61.7%, rounded up to 65%; floor 15% → 65% exceeds 50%; too noisy to gate with this many runs on this host |
| Mixed service = payments: p50 | lower | 5 | 18.62 ms | 0.852 ms | 4.6% | 17.52 ms | 19.86 ms | 30% | 5 runs, CV 4.6% → 90% upper bound 8.9% (×1.94) → 3× = 26.6%, rounded up to 30%; floor 15% → 30% |
| Mixed service = payments: p95 | lower | 5 | 27.25 ms | 1.90 ms | 7.0% | 25.24 ms | 30.37 ms | 45% | 5 runs, CV 7.0% → 90% upper bound 13.5% (×1.94) → 3× = 40.5%, rounded up to 45%; floor 15% → 45% |
| Mixed service = payments: p99 | lower | 5 | 31.43 ms | 2.42 ms | 7.7% | 27.92 ms | 34.00 ms | – | not gated: p99 from 331 samples in one run; at least 1000 are needed for 10 beyond it |
| Mixed traceId (Bloom filter): p50 | lower | 5 | 1.04 ms | 0.063 ms | 6.1% | 0.961 ms | 1.12 ms | 40% | 5 runs, CV 6.1% → 90% upper bound 11.8% (×1.94) → 3× = 35.3%, rounded up to 40%; floor 15% → 40% |
| Mixed traceId (Bloom filter): p95 | lower | 5 | 1.69 ms | 0.165 ms | 9.7% | 1.52 ms | 1.95 ms | – | not gated: 5 runs, CV 9.7% → 90% upper bound 18.9% (×1.94) → 3× = 56.7%, rounded up to 60%; floor 15% → 60% exceeds 50%; too noisy to gate with this many runs on this host |
| Mixed traceId (Bloom filter): p99 | lower | 5 | 2.18 ms | 0.213 ms | 9.8% | 1.81 ms | 2.35 ms | – | not gated: 5 runs, CV 9.8% → 90% upper bound 19.0% (×1.94) → 3× = 56.9%, rounded up to 60%; floor 15% → 60% exceeds 50%; too noisy to gate with this many runs on this host |
| Mixed message contains (scan): p50 | lower | 5 | 69.13 ms | 2.27 ms | 3.3% | 66.07 ms | 71.93 ms | 20% | 5 runs, CV 3.3% → 90% upper bound 6.4% (×1.94) → 3× = 19.1%, rounded up to 20%; floor 15% → 20% |
| Mixed message contains (scan): p95 | lower | 5 | 95.18 ms | 6.76 ms | 7.1% | 88.51 ms | 106.56 ms | – | not gated: p95 from 159 samples in one run; at least 200 are needed for 10 beyond it |
| Mixed message contains (scan): p99 | lower | 5 | 128.79 ms | 39.03 ms | 30.3% | 99.87 ms | 196.13 ms | – | not gated: 5 runs, CV 30.3% → 90% upper bound 58.8% (×1.94) → 3× = 176.3%, rounded up to 180%; floor 15% → 180% exceeds 50%; too noisy to gate with this many runs on this host |
| Mixed histogram, 60 buckets: p50 | lower | 5 | 17.72 ms | 0.904 ms | 5.1% | 16.13 ms | 18.40 ms | 30% | 5 runs, CV 5.1% → 90% upper bound 9.9% (×1.94) → 3× = 29.7%, rounded up to 30%; floor 15% → 30% |
| Mixed histogram, 60 buckets: p95 | lower | 5 | 30.03 ms | 3.61 ms | 12.0% | 26.90 ms | 35.44 ms | – | not gated: 5 runs, CV 12.0% → 90% upper bound 23.3% (×1.94) → 3× = 69.9%, rounded up to 70%; floor 15% → 70% exceeds 50%; too noisy to gate with this many runs on this host |
| Mixed histogram, 60 buckets: p99 | lower | 5 | 51.51 ms | 27.49 ms | 53.4% | 31.34 ms | 89.35 ms | – | not gated: 5 runs, CV 53.4% → 90% upper bound 103.5% (×1.94) → 3× = 310.4%, rounded up to 315%; floor 15% → 315% exceeds 50%; too noisy to gate with this many runs on this host |
| Mixed facets: p50 | lower | 5 | 44.71 ms | 1.66 ms | 3.7% | 42.48 ms | 46.55 ms | 25% | 5 runs, CV 3.7% → 90% upper bound 7.2% (×1.94) → 3× = 21.6%, rounded up to 25%; floor 15% → 25% |
| Mixed facets: p95 | lower | 5 | 66.43 ms | 4.74 ms | 7.1% | 62.89 ms | 74.60 ms | – | not gated: p95 from 145 samples in one run; at least 200 are needed for 10 beyond it |
| Mixed facets: p99 | lower | 5 | 97.30 ms | 40.76 ms | 41.9% | 73.76 ms | 169.91 ms | – | not gated: 5 runs, CV 41.9% → 90% upper bound 81.2% (×1.94) → 3× = 243.7%, rounded up to 245%; floor 15% → 245% exceeds 50%; too noisy to gate with this many runs on this host |
| Mixed trace search (limit 50): p50 | lower | 5 | 3.48 ms | 0.354 ms | 10.2% | 3.08 ms | 3.81 ms | – | not gated: 5 runs, CV 10.2% → 90% upper bound 19.7% (×1.94) → 3× = 59.2%, rounded up to 60%; floor 15% → 60% exceeds 50%; too noisy to gate with this many runs on this host |
| Mixed trace search (limit 50): p95 | lower | 5 | 6.52 ms | 0.408 ms | 6.3% | 6.05 ms | 7.17 ms | 40% | 5 runs, CV 6.3% → 90% upper bound 12.1% (×1.94) → 3× = 36.4%, rounded up to 40%; floor 15% → 40% |
| Mixed trace search (limit 50): p99 | lower | 5 | 7.55 ms | 0.660 ms | 8.7% | 7.00 ms | 8.63 ms | – | not gated: 5 runs, CV 8.7% → 90% upper bound 17.0% (×1.94) → 3× = 50.9%, rounded up to 55%; floor 15% → 55% exceeds 50%; too noisy to gate with this many runs on this host |
| Mixed trace by id: p50 | lower | 5 | 0.477 ms | 0.023 ms | 4.7% | 0.444 ms | 0.499 ms | 30% | 5 runs, CV 4.7% → 90% upper bound 9.2% (×1.94) → 3× = 27.6%, rounded up to 30%; floor 15% → 30% |
| Mixed trace by id: p95 | lower | 5 | 0.835 ms | 0.046 ms | 5.5% | 0.765 ms | 0.881 ms | – | not gated: p95 from 162 samples in one run; at least 200 are needed for 10 beyond it |
| Mixed trace by id: p99 | lower | 5 | 2.97 ms | 4.07 ms | 136.8% | 0.945 ms | 10.24 ms | – | not gated: 5 runs, CV 136.8% → 90% upper bound 265.3% (×1.94) → 3× = 796.0%, rounded up to 800%; floor 15% → 800% exceeds 50%; too noisy to gate with this many runs on this host |
| Mixed list metric names: p50 | lower | 5 | 0.281 ms | 0.019 ms | 6.7% | 0.262 ms | 0.304 ms | 40% | 5 runs, CV 6.7% → 90% upper bound 13.0% (×1.94) → 3× = 39.1%, rounded up to 40%; floor 15% → 40% |
| Mixed list metric names: p95 | lower | 5 | 0.575 ms | 0.048 ms | 8.3% | 0.540 ms | 0.658 ms | – | not gated: p95 from 152 samples in one run; at least 200 are needed for 10 beyond it |
| Mixed list metric names: p99 | lower | 5 | 1.14 ms | 0.847 ms | 74.3% | 0.647 ms | 2.65 ms | – | not gated: 5 runs, CV 74.3% → 90% upper bound 144.0% (×1.94) → 3× = 432.0%, rounded up to 435%; floor 15% → 435% exceeds 50%; too noisy to gate with this many runs on this host |
| Mixed metric series: p50 | lower | 5 | 0.433 ms | 0.018 ms | 4.2% | 0.402 ms | 0.447 ms | 25% | 5 runs, CV 4.2% → 90% upper bound 8.1% (×1.94) → 3× = 24.3%, rounded up to 25%; floor 15% → 25% |
| Mixed metric series: p95 | lower | 5 | 0.716 ms | 0.057 ms | 7.9% | 0.642 ms | 0.783 ms | – | not gated: p95 from 121 samples in one run; at least 200 are needed for 10 beyond it |
| Mixed metric series: p99 | lower | 5 | 0.943 ms | 0.147 ms | 15.6% | 0.795 ms | 1.11 ms | – | not gated: 5 runs, CV 15.6% → 90% upper bound 30.2% (×1.94) → 3× = 90.7%, rounded up to 95%; floor 15% → 95% exceeds 50%; too noisy to gate with this many runs on this host |
| Peak RSS | lower | 5 | 54.42 MB | 1.51 MB | 2.8% | 52.83 MB | 56.02 MB | 20% | 5 runs, CV 2.8% → 90% upper bound 5.4% (×1.94) → 3× = 16.1%, rounded up to 20%; floor 15% → 20% |
| Restart to healthy (mean) | lower | 5 | 518.71 ms | 1.34 ms | 0.3% | 516.77 ms | 520.28 ms | 15% | 5 runs, CV 0.3% → 90% upper bound 0.5% (×1.94) → 3× = 1.5%, rounded up to 5%; floor 15% → 15% |
| Data directory after seeding | lower | 5 | 7.17 MB | 10.84 KB | 0.1% | 7.15 MB | 7.18 MB | 2% | 5 runs, CV 0.1% → 90% upper bound 0.3% (×1.94) → 3× = 0.9%, rounded up to 1%; floor 2% → 2% |
| Stored bytes per event | lower | 5 | 72.9 B | 0.1 B | 0.1% | 72.7 B | 73.0 B | 2% | 5 runs, CV 0.1% → 90% upper bound 0.3% (×1.94) → 3× = 0.9%, rounded up to 1%; floor 2% → 2% |
| Binary size | lower | 5 | 11.81 MB | 0.0 B | 0.0% | 11.81 MB | 11.81 MB | 2% | 5 runs, CV 0.0% → 90% upper bound 0.0% (×1.94) → 3× = 0.0%, rounded up to 0%; floor 2% → 2% |

## Raw data

- `2026-10-15-windows-laptop.json`: every run's measurements, diagnostics and the effective server configuration.
- `2026-10-15-windows-laptop.samples.csv`: every request's latency and HTTP status.
