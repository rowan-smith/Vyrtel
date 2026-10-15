# Vyrtel benchmarks

- Generated: 2026-10-15 13:35:18 UTC
- Vyrtel: 0.1.1
- BenchmarkDotNet job: short
- Dataset: 200,000 seeded log events
- Machine: Ubuntu 24.04.5 LTS (X64), 4 logical cores
- Runtime: .NET 10.0.13
- Executable: 10.7 MB (release)
- Data directory: 22.6 MB on disk after seeding
- Storage: 203,200 events in 3 segments · 116.79 bytes/event · 4.07x compression · 7.5% index overhead

## Ingest

| Benchmark | Mean | Median | Min | Max | StdDev | Throughput |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Ingest native JSON array | 3.91 ms | 3.89 ms | 3.80 ms | 4.05 ms | 0.127 ms | 127,757 events/s |
| Ingest native NDJSON batch | 3.96 ms | 3.99 ms | 3.86 ms | 4.04 ms | 0.091 ms | 126,208 events/s |
| Ingest OTLP metrics (500 points) | 2.19 ms | 2.19 ms | 2.16 ms | 2.20 ms | 0.021 ms | 228,767 points/s |
| Ingest OTLP trace (3 spans) | 0.273 ms | 0.273 ms | 0.272 ms | 0.273 ms | 0.001 ms | 11,003 spans/s |

## Query

| Benchmark | Mean | Median | Min | Max | StdDev | Throughput |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Count: level = Error | 7.04 ms | 7.02 ms | 7.01 ms | 7.09 ms | 0.047 ms | 142 ops/s |
| Facets: sampled field values | 20.25 ms | 20.30 ms | 20.13 ms | 20.33 ms | 0.104 ms | 49 ops/s |
| Filter: level = Error | 9.82 ms | 9.87 ms | 9.62 ms | 9.97 ms | 0.177 ms | 102 ops/s |
| Filter: service = payments | 4.70 ms | 4.75 ms | 4.48 ms | 4.87 ms | 0.201 ms | 213 ops/s |
| Full-text: message contains error (scan) | 36.20 ms | 36.44 ms | 35.62 ms | 36.53 ms | 0.502 ms | 28 ops/s |
| Histogram: all logs (60 buckets) | 7.32 ms | 7.33 ms | 7.28 ms | 7.36 ms | 0.042 ms | 137 ops/s |
| Latest logs (limit 200) | 3.97 ms | 3.99 ms | 3.92 ms | 4.00 ms | 0.044 ms | 252 ops/s |
| List metric names | 0.198 ms | 0.198 ms | 0.197 ms | 0.198 ms | 0.001 ms | 5,059 ops/s |
| Lookup: traceId = ... (bloom filter) | 1.87 ms | 1.87 ms | 1.86 ms | 1.88 ms | 0.011 ms | 535 ops/s |
| Metric series (avg over range) | 0.283 ms | 0.285 ms | 0.279 ms | 0.285 ms | 0.003 ms | 3,534 ops/s |
| Range: durationMs > 1990 (block skip) | 124.83 ms | 124.76 ms | 124.46 ms | 125.28 ms | 0.414 ms | 8 ops/s |
| Trace by id (all spans) | 0.322 ms | 0.321 ms | 0.319 ms | 0.326 ms | 0.004 ms | 3,106 ops/s |
| Trace search (limit 50) | 5.40 ms | 5.41 ms | 5.34 ms | 5.44 ms | 0.051 ms | 185 ops/s |

