# Vyrtel benchmarks

- Generated: 2026-10-10 06:39:38 UTC
- Vyrtel: 0.1.1
- BenchmarkDotNet job: short
- Dataset: 200,000 seeded log events
- Machine: Ubuntu 24.04.5 LTS (X64), 4 logical cores
- Runtime: .NET 10.0.12
- Executable: 10.4 MB (release)
- Data directory: 22.6 MB on disk after seeding
- Storage: 203,200 events in 3 segments · 116.76 bytes/event · 4.08x compression · 7.5% index overhead

## Ingest

| Benchmark | Mean | Median | Min | Max | StdDev | Throughput |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Ingest native JSON array | 4.14 ms | 4.01 ms | 3.88 ms | 4.52 ms | 0.340 ms | 120,795 events/s |
| Ingest native NDJSON batch | 4.36 ms | 4.24 ms | 4.20 ms | 4.64 ms | 0.244 ms | 114,595 events/s |
| Ingest OTLP metrics (500 points) | 2.07 ms | 2.08 ms | 2.01 ms | 2.11 ms | 0.048 ms | 242,036 points/s |
| Ingest OTLP trace (3 spans) | 0.157 ms | 0.156 ms | 0.155 ms | 0.161 ms | 0.003 ms | 19,072 spans/s |

## Query

| Benchmark | Mean | Median | Min | Max | StdDev | Throughput |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Count: level = Error | 5.31 ms | 5.26 ms | 5.25 ms | 5.41 ms | 0.088 ms | 188 ops/s |
| Facets: sampled field values | 13.51 ms | 13.57 ms | 13.37 ms | 13.59 ms | 0.125 ms | 74 ops/s |
| Filter: level = Error | 7.23 ms | 7.23 ms | 7.16 ms | 7.29 ms | 0.065 ms | 138 ops/s |
| Filter: service = payments | 3.72 ms | 3.72 ms | 3.68 ms | 3.76 ms | 0.041 ms | 269 ops/s |
| Full-text: message contains error (scan) | 26.55 ms | 26.10 ms | 25.62 ms | 27.94 ms | 1.22 ms | 38 ops/s |
| Histogram: all logs (60 buckets) | 5.38 ms | 5.34 ms | 5.32 ms | 5.48 ms | 0.088 ms | 186 ops/s |
| Latest logs (limit 200) | 3.25 ms | 3.22 ms | 3.19 ms | 3.34 ms | 0.081 ms | 308 ops/s |
| List metric names | 0.106 ms | 0.105 ms | 0.105 ms | 0.107 ms | 0.001 ms | 9,454 ops/s |
| Lookup: traceId = ... (bloom filter) | 1.54 ms | 1.52 ms | 1.50 ms | 1.59 ms | 0.044 ms | 650 ops/s |
| Metric series (avg over range) | 0.169 ms | 0.168 ms | 0.166 ms | 0.172 ms | 0.003 ms | 5,928 ops/s |
| Range: durationMs > 1990 (block skip) | 93.24 ms | 93.46 ms | 91.87 ms | 94.40 ms | 1.28 ms | 11 ops/s |
| Trace by id (all spans) | 0.233 ms | 0.233 ms | 0.229 ms | 0.236 ms | 0.004 ms | 4,297 ops/s |
| Trace search (limit 50) | 2.25 ms | 2.25 ms | 2.24 ms | 2.26 ms | 0.007 ms | 444 ops/s |

