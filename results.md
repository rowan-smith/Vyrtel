# Vyrtel benchmarks

- Generated: 2026-10-16 08:23:23 UTC
- Vyrtel: 0.1.1
- BenchmarkDotNet job: short
- Dataset: 200,000 seeded log events
- Machine: Ubuntu 24.04.5 LTS (X64), 4 logical cores
- Runtime: .NET 10.0.13
- Executable: 10.7 MB (release)
- Data directory: 22.6 MB on disk after seeding
- Storage: 203,200 events in 3 segments · 116.74 bytes/event · 4.08x compression · 7.5% index overhead

## Ingest

| Benchmark | Mean | Median | Min | Max | StdDev | Throughput |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Ingest native JSON array | 5.06 ms | 5.02 ms | 4.84 ms | 5.33 ms | 0.249 ms | 98,732 events/s |
| Ingest native NDJSON batch | 4.61 ms | 4.72 ms | 4.25 ms | 4.85 ms | 0.312 ms | 108,565 events/s |
| Ingest OTLP metrics (500 points) | 2.36 ms | 2.37 ms | 2.27 ms | 2.45 ms | 0.089 ms | 211,715 points/s |
| Ingest OTLP trace (3 spans) | 0.271 ms | 0.271 ms | 0.270 ms | 0.273 ms | 0.001 ms | 11,058 spans/s |

## Query

| Benchmark | Mean | Median | Min | Max | StdDev | Throughput |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Count: level = Error | 7.24 ms | 7.20 ms | 7.20 ms | 7.30 ms | 0.059 ms | 138 ops/s |
| Facets: sampled field values | 20.88 ms | 20.92 ms | 20.63 ms | 21.10 ms | 0.238 ms | 48 ops/s |
| Filter: level = Error | 9.76 ms | 9.70 ms | 9.49 ms | 10.08 ms | 0.295 ms | 103 ops/s |
| Filter: service = payments | 4.83 ms | 4.95 ms | 4.56 ms | 4.98 ms | 0.229 ms | 207 ops/s |
| Full-text: message contains error (scan) | 35.39 ms | 35.39 ms | 35.14 ms | 35.63 ms | 0.246 ms | 28 ops/s |
| Histogram: all logs (60 buckets) | 7.63 ms | 7.61 ms | 7.55 ms | 7.74 ms | 0.100 ms | 131 ops/s |
| Latest logs (limit 200) | 4.50 ms | 4.46 ms | 4.35 ms | 4.69 ms | 0.176 ms | 222 ops/s |
| List metric names | 0.191 ms | 0.191 ms | 0.189 ms | 0.192 ms | 0.001 ms | 5,248 ops/s |
| Lookup: traceId = ... (bloom filter) | 2.42 ms | 2.29 ms | 2.28 ms | 2.69 ms | 0.233 ms | 413 ops/s |
| Metric series (avg over range) | 0.280 ms | 0.280 ms | 0.279 ms | 0.281 ms | 0.001 ms | 3,568 ops/s |
| Range: durationMs > 1990 (block skip) | 125.78 ms | 124.84 ms | 124.00 ms | 128.51 ms | 2.40 ms | 8 ops/s |
| Trace by id (all spans) | 0.326 ms | 0.326 ms | 0.323 ms | 0.328 ms | 0.002 ms | 3,071 ops/s |
| Trace search (limit 50) | 5.49 ms | 5.50 ms | 5.44 ms | 5.52 ms | 0.039 ms | 182 ops/s |

