# Vyrtel benchmarks

- Generated: 2026-10-16 07:02:29 UTC
- Vyrtel: 0.1.1
- BenchmarkDotNet job: short
- Dataset: 200,000 seeded log events
- Machine: Ubuntu 24.04.5 LTS (X64), 4 logical cores
- Runtime: .NET 10.0.13
- Executable: 10.7 MB (release)
- Data directory: 22.6 MB on disk after seeding
- Storage: 203,200 events in 3 segments · 116.81 bytes/event · 4.07x compression · 7.5% index overhead

## Ingest

| Benchmark | Mean | Median | Min | Max | StdDev | Throughput |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Ingest native JSON array | 4.15 ms | 4.11 ms | 4.04 ms | 4.30 ms | 0.134 ms | 120,381 events/s |
| Ingest native NDJSON batch | 3.98 ms | 3.83 ms | 3.76 ms | 4.35 ms | 0.324 ms | 125,659 events/s |
| Ingest OTLP metrics (500 points) | 2.26 ms | 2.29 ms | 2.16 ms | 2.31 ms | 0.081 ms | 221,609 points/s |
| Ingest OTLP trace (3 spans) | 0.269 ms | 0.268 ms | 0.268 ms | 0.270 ms | 0.001 ms | 11,174 spans/s |

## Query

| Benchmark | Mean | Median | Min | Max | StdDev | Throughput |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Count: level = Error | 7.12 ms | 7.13 ms | 7.07 ms | 7.17 ms | 0.053 ms | 140 ops/s |
| Facets: sampled field values | 19.60 ms | 19.72 ms | 19.11 ms | 19.99 ms | 0.452 ms | 51 ops/s |
| Filter: level = Error | 9.58 ms | 9.58 ms | 9.52 ms | 9.64 ms | 0.056 ms | 104 ops/s |
| Filter: service = payments | 4.64 ms | 4.59 ms | 4.45 ms | 4.89 ms | 0.220 ms | 215 ops/s |
| Full-text: message contains error (scan) | 35.39 ms | 35.19 ms | 35.00 ms | 35.97 ms | 0.515 ms | 28 ops/s |
| Histogram: all logs (60 buckets) | 7.39 ms | 7.43 ms | 7.26 ms | 7.49 ms | 0.120 ms | 135 ops/s |
| Latest logs (limit 200) | 4.10 ms | 4.11 ms | 4.00 ms | 4.18 ms | 0.090 ms | 244 ops/s |
| List metric names | 0.192 ms | 0.193 ms | 0.191 ms | 0.193 ms | 0.001 ms | 5,203 ops/s |
| Lookup: traceId = ... (bloom filter) | 2.08 ms | 2.09 ms | 2.07 ms | 2.09 ms | 0.010 ms | 480 ops/s |
| Metric series (avg over range) | 0.278 ms | 0.279 ms | 0.273 ms | 0.282 ms | 0.005 ms | 3,597 ops/s |
| Range: durationMs > 1990 (block skip) | 124.77 ms | 124.52 ms | 124.10 ms | 125.69 ms | 0.821 ms | 8 ops/s |
| Trace by id (all spans) | 0.320 ms | 0.322 ms | 0.316 ms | 0.323 ms | 0.004 ms | 3,121 ops/s |
| Trace search (limit 50) | 5.50 ms | 5.52 ms | 5.39 ms | 5.59 ms | 0.099 ms | 182 ops/s |

