# Vyrtel benchmarks

- Generated: 2026-10-26 11:29:31 UTC
- Vyrtel: 0.1.1
- BenchmarkDotNet job: short
- Dataset: 200,000 seeded log events
- Machine: Ubuntu 24.04.5 LTS (X64), 4 logical cores
- Runtime: .NET 10.0.14
- Executable: 12.8 MB (release)
- Data directory: 22.0 MB on disk after seeding
- Storage: 203,200 events in 3 segments · 113.40 bytes/event · 4.07x compression · 7.5% index overhead

## Ingest

| Benchmark | Mean | Median | Min | Max | StdDev | Throughput |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Ingest native JSON array | 3.79 ms | 3.79 ms | 3.72 ms | 3.86 ms | 0.071 ms | 132,010 events/s |
| Ingest native NDJSON batch | 3.40 ms | 3.55 ms | 3.09 ms | 3.56 ms | 0.267 ms | 146,917 events/s |
| Ingest OTLP metrics (500 points) | 2.39 ms | 2.33 ms | 2.28 ms | 2.57 ms | 0.156 ms | 208,881 points/s |
| Ingest OTLP trace (3 spans) | 0.281 ms | 0.281 ms | 0.279 ms | 0.283 ms | 0.002 ms | 10,664 spans/s |

## Query

| Benchmark | Mean | Median | Min | Max | StdDev | Throughput |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Count: level = Error | 7.33 ms | 7.35 ms | 7.24 ms | 7.41 ms | 0.084 ms | 136 ops/s |
| Facets: sampled field values | 18.57 ms | 18.74 ms | 18.13 ms | 18.83 ms | 0.380 ms | 54 ops/s |
| Filter: level = Error | 10.46 ms | 10.25 ms | 10.18 ms | 10.94 ms | 0.423 ms | 96 ops/s |
| Filter: service = payments | 4.40 ms | 4.36 ms | 4.36 ms | 4.48 ms | 0.069 ms | 227 ops/s |
| Full-text: message contains error (scan) | 37.05 ms | 37.09 ms | 36.92 ms | 37.14 ms | 0.112 ms | 27 ops/s |
| Histogram: all logs (60 buckets) | 7.74 ms | 7.74 ms | 7.70 ms | 7.80 ms | 0.049 ms | 129 ops/s |
| Latest logs (limit 200) | 3.81 ms | 3.80 ms | 3.78 ms | 3.84 ms | 0.030 ms | 263 ops/s |
| List metric names | 0.289 ms | 0.289 ms | 0.289 ms | 0.290 ms | 0.001 ms | 3,459 ops/s |
| Lookup: traceId = ... (bloom filter) | 2.31 ms | 2.26 ms | 2.21 ms | 2.45 ms | 0.130 ms | 434 ops/s |
| Metric series (avg over range) | 0.384 ms | 0.384 ms | 0.382 ms | 0.385 ms | 0.002 ms | 2,606 ops/s |
| Range: durationMs > 1990 (block skip) | 123.74 ms | 123.87 ms | 123.03 ms | 124.32 ms | 0.654 ms | 8 ops/s |
| Trace by id (all spans) | 0.383 ms | 0.387 ms | 0.373 ms | 0.389 ms | 0.009 ms | 2,610 ops/s |
| Trace search (limit 50) | 4.96 ms | 4.95 ms | 4.94 ms | 4.97 ms | 0.014 ms | 202 ops/s |

