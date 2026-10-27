# Vyrtel benchmarks

- Generated: 2026-10-27 12:52:50 UTC
- Vyrtel: 0.1.1
- BenchmarkDotNet job: short
- Dataset: 200,000 seeded log events
- Machine: Ubuntu 26.04.1 LTS (X64), 4 logical cores
- Runtime: .NET 10.0.14
- Executable: 12.8 MB (release)
- Data directory: 21.9 MB on disk after seeding
- Storage: 203,200 events in 3 segments · 113.15 bytes/event · 4.09x compression · 7.5% index overhead

## Ingest

| Benchmark | Mean | Median | Min | Max | StdDev | Throughput |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Ingest native JSON array | 2.37 ms | 2.40 ms | 2.16 ms | 2.55 ms | 0.198 ms | 211,147 events/s |
| Ingest native NDJSON batch | 2.32 ms | 2.36 ms | 2.11 ms | 2.50 ms | 0.201 ms | 215,338 events/s |
| Ingest OTLP metrics (500 points) | 1.53 ms | 1.53 ms | 1.52 ms | 1.55 ms | 0.014 ms | 326,516 points/s |
| Ingest OTLP trace (3 spans) | 0.096 ms | 0.095 ms | 0.095 ms | 0.096 ms | 0.000 ms | 31,405 spans/s |

## Query

| Benchmark | Mean | Median | Min | Max | StdDev | Throughput |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Count: level = Error | 4.03 ms | 4.03 ms | 4.02 ms | 4.03 ms | 0.002 ms | 248 ops/s |
| Facets: sampled field values | 9.31 ms | 9.35 ms | 9.21 ms | 9.38 ms | 0.088 ms | 107 ops/s |
| Filter: level = Error | 5.56 ms | 5.56 ms | 5.55 ms | 5.56 ms | 0.007 ms | 180 ops/s |
| Filter: service = payments | 2.29 ms | 2.29 ms | 2.28 ms | 2.29 ms | 0.005 ms | 437 ops/s |
| Full-text: message contains error (scan) | 19.35 ms | 19.37 ms | 19.32 ms | 19.38 ms | 0.036 ms | 52 ops/s |
| Histogram: all logs (60 buckets) | 4.04 ms | 4.04 ms | 4.04 ms | 4.05 ms | 0.002 ms | 247 ops/s |
| Latest logs (limit 200) | 1.98 ms | 1.98 ms | 1.97 ms | 1.99 ms | 0.010 ms | 505 ops/s |
| List metric names | 0.053 ms | 0.053 ms | 0.052 ms | 0.055 ms | 0.002 ms | 18,737 ops/s |
| Lookup: traceId = ... (bloom filter) | 0.999 ms | 0.997 ms | 0.997 ms | 1.00 ms | 0.003 ms | 1,001 ops/s |
| Metric series (avg over range) | 0.116 ms | 0.116 ms | 0.115 ms | 0.116 ms | 0.000 ms | 8,659 ops/s |
| Range: durationMs > 1990 (block skip) | 63.75 ms | 63.97 ms | 63.13 ms | 64.16 ms | 0.547 ms | 16 ops/s |
| Trace by id (all spans) | 0.127 ms | 0.127 ms | 0.127 ms | 0.127 ms | 0.000 ms | 7,876 ops/s |
| Trace search (limit 50) | 1.56 ms | 1.56 ms | 1.55 ms | 1.57 ms | 0.011 ms | 641 ops/s |

