# Vyrtel benchmarks

- Generated: 2026-10-19 09:46:14 UTC
- Vyrtel: 0.1.1
- BenchmarkDotNet job: short
- Dataset: 200,000 seeded log events
- Machine: Ubuntu 24.04.5 LTS (X64), 4 logical cores
- Runtime: .NET 10.0.13
- Executable: 10.7 MB (release)
- Data directory: 22.6 MB on disk after seeding
- Storage: 203,200 events in 3 segments · 116.65 bytes/event · 4.08x compression · 7.5% index overhead

## Ingest

| Benchmark | Mean | Median | Min | Max | StdDev | Throughput |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Ingest native JSON array | 3.29 ms | 3.31 ms | 3.15 ms | 3.40 ms | 0.125 ms | 152,059 events/s |
| Ingest native NDJSON batch | 3.71 ms | 3.62 ms | 3.46 ms | 4.04 ms | 0.299 ms | 134,953 events/s |
| Ingest OTLP metrics (500 points) | 1.84 ms | 1.69 ms | 1.68 ms | 2.14 ms | 0.263 ms | 271,786 points/s |
| Ingest OTLP trace (3 spans) | 0.123 ms | 0.123 ms | 0.122 ms | 0.125 ms | 0.002 ms | 24,318 spans/s |

## Query

| Benchmark | Mean | Median | Min | Max | StdDev | Throughput |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Count: level = Error | 4.44 ms | 4.43 ms | 4.42 ms | 4.47 ms | 0.023 ms | 225 ops/s |
| Facets: sampled field values | 17.62 ms | 16.36 ms | 15.24 ms | 21.25 ms | 3.19 ms | 57 ops/s |
| Filter: level = Error | 6.02 ms | 5.95 ms | 5.80 ms | 6.30 ms | 0.256 ms | 166 ops/s |
| Filter: service = payments | 2.89 ms | 2.89 ms | 2.84 ms | 2.94 ms | 0.052 ms | 346 ops/s |
| Full-text: message contains error (scan) | 21.78 ms | 21.64 ms | 21.59 ms | 22.11 ms | 0.289 ms | 46 ops/s |
| Histogram: all logs (60 buckets) | 4.34 ms | 4.32 ms | 4.30 ms | 4.39 ms | 0.047 ms | 230 ops/s |
| Latest logs (limit 200) | 2.44 ms | 2.44 ms | 2.43 ms | 2.46 ms | 0.015 ms | 409 ops/s |
| List metric names | 0.068 ms | 0.069 ms | 0.067 ms | 0.070 ms | 0.002 ms | 14,613 ops/s |
| Lookup: traceId = ... (bloom filter) | 1.12 ms | 1.12 ms | 1.09 ms | 1.15 ms | 0.027 ms | 891 ops/s |
| Metric series (avg over range) | 0.134 ms | 0.132 ms | 0.132 ms | 0.137 ms | 0.003 ms | 7,474 ops/s |
| Range: durationMs > 1990 (block skip) | 83.41 ms | 83.40 ms | 83.30 ms | 83.52 ms | 0.111 ms | 12 ops/s |
| Trace by id (all spans) | 0.162 ms | 0.162 ms | 0.160 ms | 0.163 ms | 0.002 ms | 6,175 ops/s |
| Trace search (limit 50) | 2.00 ms | 2.00 ms | 1.99 ms | 2.02 ms | 0.011 ms | 499 ops/s |

