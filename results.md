# Vyrtel benchmarks

- Generated: 2026-10-21 11:04:27 UTC
- Vyrtel: 0.1.1
- BenchmarkDotNet job: short
- Dataset: 200,000 seeded log events
- Machine: Ubuntu 24.04.5 LTS (X64), 4 logical cores
- Runtime: .NET 10.0.13
- Executable: 10.8 MB (release)
- Data directory: 22.0 MB on disk after seeding
- Storage: 203,200 events in 3 segments · 113.36 bytes/event · 4.07x compression · 7.5% index overhead

## Ingest

| Benchmark | Mean | Median | Min | Max | StdDev | Throughput |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Ingest native JSON array | 3.82 ms | 3.84 ms | 3.60 ms | 4.04 ms | 0.219 ms | 130,758 events/s |
| Ingest native NDJSON batch | 3.63 ms | 3.62 ms | 3.33 ms | 3.93 ms | 0.303 ms | 137,881 events/s |
| Ingest OTLP metrics (500 points) | 2.29 ms | 2.26 ms | 2.26 ms | 2.35 ms | 0.051 ms | 218,218 points/s |
| Ingest OTLP trace (3 spans) | 0.276 ms | 0.274 ms | 0.273 ms | 0.280 ms | 0.003 ms | 10,889 spans/s |

## Query

| Benchmark | Mean | Median | Min | Max | StdDev | Throughput |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Count: level = Error | 7.49 ms | 7.41 ms | 7.36 ms | 7.70 ms | 0.185 ms | 134 ops/s |
| Facets: sampled field values | 19.97 ms | 19.89 ms | 19.60 ms | 20.44 ms | 0.423 ms | 50 ops/s |
| Filter: level = Error | 10.98 ms | 11.09 ms | 10.74 ms | 11.10 ms | 0.207 ms | 91 ops/s |
| Filter: service = payments | 5.21 ms | 5.24 ms | 5.11 ms | 5.27 ms | 0.087 ms | 192 ops/s |
| Full-text: message contains error (scan) | 39.04 ms | 38.97 ms | 38.80 ms | 39.36 ms | 0.286 ms | 26 ops/s |
| Histogram: all logs (60 buckets) | 7.76 ms | 7.72 ms | 7.60 ms | 7.96 ms | 0.183 ms | 129 ops/s |
| Latest logs (limit 200) | 4.33 ms | 4.28 ms | 4.26 ms | 4.43 ms | 0.093 ms | 231 ops/s |
| List metric names | 0.194 ms | 0.194 ms | 0.193 ms | 0.195 ms | 0.001 ms | 5,152 ops/s |
| Lookup: traceId = ... (bloom filter) | 2.30 ms | 2.29 ms | 2.21 ms | 2.39 ms | 0.087 ms | 436 ops/s |
| Metric series (avg over range) | 0.283 ms | 0.284 ms | 0.280 ms | 0.285 ms | 0.003 ms | 3,535 ops/s |
| Range: durationMs > 1990 (block skip) | 125.24 ms | 124.93 ms | 123.87 ms | 126.91 ms | 1.55 ms | 8 ops/s |
| Trace by id (all spans) | 0.322 ms | 0.321 ms | 0.318 ms | 0.328 ms | 0.005 ms | 3,104 ops/s |
| Trace search (limit 50) | 5.44 ms | 5.47 ms | 5.36 ms | 5.50 ms | 0.074 ms | 184 ops/s |

