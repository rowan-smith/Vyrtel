# Vyrtel benchmarks

- Generated: 2026-10-11 12:04:48 UTC
- Vyrtel: 0.1.1
- BenchmarkDotNet job: short
- Dataset: 200,000 seeded log events
- Machine: Ubuntu 24.04.5 LTS (X64), 4 logical cores
- Runtime: .NET 10.0.12
- Executable: 10.5 MB (release)
- Data directory: 22.6 MB on disk after seeding
- Storage: 203,200 events in 3 segments · 116.86 bytes/event · 4.07x compression · 7.5% index overhead

## Ingest

| Benchmark | Mean | Median | Min | Max | StdDev | Throughput |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Ingest native JSON array | 4.84 ms | 4.87 ms | 4.71 ms | 4.93 ms | 0.113 ms | 103,366 events/s |
| Ingest native NDJSON batch | 4.91 ms | 5.01 ms | 4.67 ms | 5.05 ms | 0.209 ms | 101,876 events/s |
| Ingest OTLP metrics (500 points) | 2.27 ms | 2.28 ms | 2.21 ms | 2.33 ms | 0.061 ms | 220,204 points/s |
| Ingest OTLP trace (3 spans) | 0.279 ms | 0.279 ms | 0.279 ms | 0.280 ms | 0.000 ms | 10,744 spans/s |

## Query

| Benchmark | Mean | Median | Min | Max | StdDev | Throughput |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Count: level = Error | 7.25 ms | 7.26 ms | 7.25 ms | 7.26 ms | 0.008 ms | 138 ops/s |
| Facets: sampled field values | 21.20 ms | 21.18 ms | 21.12 ms | 21.32 ms | 0.101 ms | 47 ops/s |
| Filter: level = Error | 10.60 ms | 10.58 ms | 10.47 ms | 10.73 ms | 0.130 ms | 94 ops/s |
| Filter: service = payments | 5.06 ms | 4.95 ms | 4.94 ms | 5.28 ms | 0.191 ms | 198 ops/s |
| Full-text: message contains error (scan) | 37.83 ms | 37.73 ms | 37.57 ms | 38.20 ms | 0.325 ms | 26 ops/s |
| Histogram: all logs (60 buckets) | 7.67 ms | 7.62 ms | 7.59 ms | 7.79 ms | 0.110 ms | 130 ops/s |
| Latest logs (limit 200) | 4.39 ms | 4.31 ms | 4.29 ms | 4.57 ms | 0.159 ms | 228 ops/s |
| List metric names | 0.205 ms | 0.205 ms | 0.203 ms | 0.207 ms | 0.002 ms | 4,879 ops/s |
| Lookup: traceId = ... (bloom filter) | 2.29 ms | 2.25 ms | 2.22 ms | 2.39 ms | 0.092 ms | 437 ops/s |
| Metric series (avg over range) | 0.293 ms | 0.293 ms | 0.292 ms | 0.294 ms | 0.001 ms | 3,416 ops/s |
| Range: durationMs > 1990 (block skip) | 126.65 ms | 126.61 ms | 125.53 ms | 127.80 ms | 1.14 ms | 8 ops/s |
| Trace by id (all spans) | 0.334 ms | 0.334 ms | 0.333 ms | 0.335 ms | 0.001 ms | 2,996 ops/s |
| Trace search (limit 50) | 5.62 ms | 5.60 ms | 5.57 ms | 5.71 ms | 0.078 ms | 178 ops/s |

