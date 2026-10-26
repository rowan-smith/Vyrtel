# Vyrtel benchmarks

- Generated: 2026-10-26 20:51:04 UTC
- Vyrtel: 0.1.1
- BenchmarkDotNet job: short
- Dataset: 200,000 seeded log events
- Machine: Ubuntu 24.04.5 LTS (X64), 4 logical cores
- Runtime: .NET 10.0.14
- Executable: 12.8 MB (release)
- Data directory: 21.9 MB on disk after seeding
- Storage: 203,200 events in 3 segments · 113.21 bytes/event · 4.09x compression · 7.5% index overhead

## Ingest

| Benchmark | Mean | Median | Min | Max | StdDev | Throughput |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Ingest native JSON array | 2.94 ms | 3.10 ms | 2.59 ms | 3.14 ms | 0.308 ms | 169,856 events/s |
| Ingest native NDJSON batch | 2.58 ms | 2.55 ms | 2.33 ms | 2.85 ms | 0.259 ms | 194,176 events/s |
| Ingest OTLP metrics (500 points) | 1.62 ms | 1.62 ms | 1.59 ms | 1.64 ms | 0.024 ms | 309,403 points/s |
| Ingest OTLP trace (3 spans) | 0.126 ms | 0.127 ms | 0.123 ms | 0.127 ms | 0.003 ms | 23,905 spans/s |

## Query

| Benchmark | Mean | Median | Min | Max | StdDev | Throughput |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Count: level = Error | 4.66 ms | 4.51 ms | 4.49 ms | 4.98 ms | 0.276 ms | 215 ops/s |
| Facets: sampled field values | 15.23 ms | 15.22 ms | 15.09 ms | 15.39 ms | 0.154 ms | 66 ops/s |
| Filter: level = Error | 7.29 ms | 7.26 ms | 7.22 ms | 7.40 ms | 0.090 ms | 137 ops/s |
| Filter: service = payments | 2.87 ms | 2.88 ms | 2.83 ms | 2.92 ms | 0.045 ms | 348 ops/s |
| Full-text: message contains error (scan) | 27.23 ms | 27.28 ms | 27.15 ms | 27.28 ms | 0.074 ms | 37 ops/s |
| Histogram: all logs (60 buckets) | 4.97 ms | 4.90 ms | 4.58 ms | 5.41 ms | 0.418 ms | 201 ops/s |
| Latest logs (limit 200) | 2.60 ms | 2.57 ms | 2.57 ms | 2.65 ms | 0.047 ms | 385 ops/s |
| List metric names | 0.126 ms | 0.126 ms | 0.126 ms | 0.127 ms | 0.001 ms | 7,943 ops/s |
| Lookup: traceId = ... (bloom filter) | 1.22 ms | 1.24 ms | 1.19 ms | 1.24 ms | 0.029 ms | 817 ops/s |
| Metric series (avg over range) | 0.178 ms | 0.178 ms | 0.175 ms | 0.181 ms | 0.003 ms | 5,623 ops/s |
| Range: durationMs > 1990 (block skip) | 83.91 ms | 84.04 ms | 83.56 ms | 84.12 ms | 0.304 ms | 12 ops/s |
| Trace by id (all spans) | 0.193 ms | 0.189 ms | 0.188 ms | 0.201 ms | 0.007 ms | 5,187 ops/s |
| Trace search (limit 50) | 1.84 ms | 1.84 ms | 1.83 ms | 1.85 ms | 0.009 ms | 544 ops/s |

