# Vyrtel benchmarks

- Generated: 2026-10-13 11:32:37 UTC
- Vyrtel: 0.1.1
- BenchmarkDotNet job: short
- Dataset: 200,000 seeded log events
- Machine: Ubuntu 24.04.5 LTS (X64), 4 logical cores
- Runtime: .NET 10.0.12
- Executable: 10.7 MB (release)
- Data directory: 22.7 MB on disk after seeding
- Storage: 203,200 events in 3 segments · 116.91 bytes/event · 4.07x compression · 7.5% index overhead

## Ingest

| Benchmark | Mean | Median | Min | Max | StdDev | Throughput |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Ingest native JSON array | 4.34 ms | 4.34 ms | 4.29 ms | 4.38 ms | 0.049 ms | 115,337 events/s |
| Ingest native NDJSON batch | 4.27 ms | 4.38 ms | 3.79 ms | 4.63 ms | 0.434 ms | 117,127 events/s |
| Ingest OTLP metrics (500 points) | 2.31 ms | 2.27 ms | 2.23 ms | 2.43 ms | 0.104 ms | 216,543 points/s |
| Ingest OTLP trace (3 spans) | 0.272 ms | 0.270 ms | 0.270 ms | 0.274 ms | 0.003 ms | 11,051 spans/s |

## Query

| Benchmark | Mean | Median | Min | Max | StdDev | Throughput |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Count: level = Error | 7.13 ms | 7.11 ms | 7.09 ms | 7.18 ms | 0.044 ms | 140 ops/s |
| Facets: sampled field values | 20.64 ms | 20.55 ms | 20.45 ms | 20.93 ms | 0.253 ms | 48 ops/s |
| Filter: level = Error | 9.44 ms | 9.46 ms | 9.31 ms | 9.54 ms | 0.113 ms | 106 ops/s |
| Filter: service = payments | 4.75 ms | 4.66 ms | 4.63 ms | 4.96 ms | 0.184 ms | 210 ops/s |
| Full-text: message contains error (scan) | 36.61 ms | 36.73 ms | 36.16 ms | 36.94 ms | 0.405 ms | 27 ops/s |
| Histogram: all logs (60 buckets) | 7.47 ms | 7.45 ms | 7.39 ms | 7.56 ms | 0.089 ms | 134 ops/s |
| Latest logs (limit 200) | 4.03 ms | 4.04 ms | 4.02 ms | 4.05 ms | 0.016 ms | 248 ops/s |
| List metric names | 0.196 ms | 0.195 ms | 0.195 ms | 0.197 ms | 0.001 ms | 5,115 ops/s |
| Lookup: traceId = ... (bloom filter) | 2.02 ms | 2.03 ms | 2.00 ms | 2.04 ms | 0.019 ms | 494 ops/s |
| Metric series (avg over range) | 0.284 ms | 0.285 ms | 0.283 ms | 0.285 ms | 0.001 ms | 3,519 ops/s |
| Range: durationMs > 1990 (block skip) | 122.28 ms | 122.53 ms | 121.65 ms | 122.65 ms | 0.550 ms | 8 ops/s |
| Trace by id (all spans) | 0.322 ms | 0.320 ms | 0.318 ms | 0.329 ms | 0.006 ms | 3,101 ops/s |
| Trace search (limit 50) | 5.29 ms | 5.30 ms | 5.29 ms | 5.30 ms | 0.006 ms | 189 ops/s |

