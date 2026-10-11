# Vyrtel benchmarks

- Generated: 2026-10-11 04:31:50 UTC
- Vyrtel: 0.1.1
- BenchmarkDotNet job: short
- Dataset: 200,000 seeded log events
- Machine: Ubuntu 24.04.5 LTS (X64), 4 logical cores
- Runtime: .NET 10.0.12
- Executable: 10.5 MB (release)
- Data directory: 22.6 MB on disk after seeding
- Storage: 203,200 events in 3 segments · 116.84 bytes/event · 4.07x compression · 7.5% index overhead

## Ingest

| Benchmark | Mean | Median | Min | Max | StdDev | Throughput |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Ingest native JSON array | 4.61 ms | 4.61 ms | 4.45 ms | 4.79 ms | 0.169 ms | 108,368 events/s |
| Ingest native NDJSON batch | 4.32 ms | 4.28 ms | 4.04 ms | 4.62 ms | 0.290 ms | 115,830 events/s |
| Ingest OTLP metrics (500 points) | 2.29 ms | 2.27 ms | 2.23 ms | 2.36 ms | 0.063 ms | 218,535 points/s |
| Ingest OTLP trace (3 spans) | 0.272 ms | 0.272 ms | 0.271 ms | 0.272 ms | 0.000 ms | 11,041 spans/s |

## Query

| Benchmark | Mean | Median | Min | Max | StdDev | Throughput |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Count: level = Error | 6.95 ms | 6.95 ms | 6.93 ms | 6.98 ms | 0.029 ms | 144 ops/s |
| Facets: sampled field values | 20.11 ms | 20.11 ms | 20.07 ms | 20.15 ms | 0.041 ms | 50 ops/s |
| Filter: level = Error | 9.68 ms | 9.63 ms | 9.55 ms | 9.85 ms | 0.154 ms | 103 ops/s |
| Filter: service = payments | 5.32 ms | 5.19 ms | 5.17 ms | 5.59 ms | 0.241 ms | 188 ops/s |
| Full-text: message contains error (scan) | 36.41 ms | 36.54 ms | 36.08 ms | 36.61 ms | 0.287 ms | 27 ops/s |
| Histogram: all logs (60 buckets) | 7.49 ms | 7.47 ms | 7.38 ms | 7.63 ms | 0.128 ms | 133 ops/s |
| Latest logs (limit 200) | 4.30 ms | 4.36 ms | 4.17 ms | 4.38 ms | 0.118 ms | 232 ops/s |
| List metric names | 0.193 ms | 0.194 ms | 0.192 ms | 0.194 ms | 0.001 ms | 5,170 ops/s |
| Lookup: traceId = ... (bloom filter) | 2.16 ms | 2.13 ms | 2.10 ms | 2.24 ms | 0.070 ms | 463 ops/s |
| Metric series (avg over range) | 0.280 ms | 0.280 ms | 0.279 ms | 0.282 ms | 0.001 ms | 3,569 ops/s |
| Range: durationMs > 1990 (block skip) | 123.60 ms | 123.04 ms | 123.00 ms | 124.75 ms | 0.998 ms | 8 ops/s |
| Trace by id (all spans) | 0.317 ms | 0.315 ms | 0.314 ms | 0.322 ms | 0.004 ms | 3,154 ops/s |
| Trace search (limit 50) | 5.42 ms | 5.42 ms | 5.40 ms | 5.43 ms | 0.018 ms | 185 ops/s |

