# Vyrtel benchmarks

- Generated: 2026-10-11 03:00:42 UTC
- Vyrtel: 0.1.1
- BenchmarkDotNet job: short
- Dataset: 200,000 seeded log events
- Machine: Ubuntu 24.04.5 LTS (X64), 4 logical cores
- Runtime: .NET 10.0.12
- Executable: 10.5 MB (release)
- Data directory: 22.6 MB on disk after seeding
- Storage: 203,200 events in 3 segments · 116.77 bytes/event · 4.07x compression · 7.5% index overhead

## Ingest

| Benchmark | Mean | Median | Min | Max | StdDev | Throughput |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Ingest native JSON array | 4.06 ms | 4.09 ms | 3.87 ms | 4.21 ms | 0.174 ms | 123,292 events/s |
| Ingest native NDJSON batch | 4.28 ms | 4.37 ms | 4.03 ms | 4.43 ms | 0.218 ms | 116,856 events/s |
| Ingest OTLP metrics (500 points) | 3.83 ms | 3.73 ms | 2.57 ms | 5.18 ms | 1.31 ms | 130,663 points/s |
| Ingest OTLP trace (3 spans) | 0.185 ms | 0.174 ms | 0.172 ms | 0.208 ms | 0.020 ms | 16,236 spans/s |

## Query

| Benchmark | Mean | Median | Min | Max | StdDev | Throughput |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Count: level = Error | 5.84 ms | 5.83 ms | 5.82 ms | 5.86 ms | 0.021 ms | 171 ops/s |
| Facets: sampled field values | 18.28 ms | 17.89 ms | 17.64 ms | 19.31 ms | 0.902 ms | 55 ops/s |
| Filter: level = Error | 8.04 ms | 7.98 ms | 7.88 ms | 8.25 ms | 0.192 ms | 124 ops/s |
| Filter: service = payments | 3.73 ms | 3.73 ms | 3.71 ms | 3.76 ms | 0.026 ms | 268 ops/s |
| Full-text: message contains error (scan) | 30.39 ms | 30.27 ms | 29.91 ms | 31.00 ms | 0.558 ms | 33 ops/s |
| Histogram: all logs (60 buckets) | 5.90 ms | 5.91 ms | 5.85 ms | 5.94 ms | 0.043 ms | 170 ops/s |
| Latest logs (limit 200) | 3.27 ms | 3.22 ms | 3.21 ms | 3.38 ms | 0.095 ms | 306 ops/s |
| List metric names | 0.113 ms | 0.113 ms | 0.112 ms | 0.114 ms | 0.001 ms | 8,853 ops/s |
| Lookup: traceId = ... (bloom filter) | 1.58 ms | 1.58 ms | 1.56 ms | 1.60 ms | 0.023 ms | 632 ops/s |
| Metric series (avg over range) | 0.190 ms | 0.190 ms | 0.189 ms | 0.191 ms | 0.001 ms | 5,265 ops/s |
| Range: durationMs > 1990 (block skip) | 102.01 ms | 101.36 ms | 101.27 ms | 103.39 ms | 1.20 ms | 10 ops/s |
| Trace by id (all spans) | 0.214 ms | 0.214 ms | 0.212 ms | 0.214 ms | 0.001 ms | 4,683 ops/s |
| Trace search (limit 50) | 4.03 ms | 4.01 ms | 4.00 ms | 4.06 ms | 0.034 ms | 248 ops/s |

