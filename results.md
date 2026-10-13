# Vyrtel benchmarks

- Generated: 2026-10-13 21:09:52 UTC
- Vyrtel: 0.1.1
- BenchmarkDotNet job: short
- Dataset: 200,000 seeded log events
- Machine: Ubuntu 24.04.5 LTS (X64), 4 logical cores
- Runtime: .NET 10.0.13
- Executable: 10.7 MB (release)
- Data directory: 22.6 MB on disk after seeding
- Storage: 203,200 events in 3 segments · 116.77 bytes/event · 4.07x compression · 7.5% index overhead

## Ingest

| Benchmark | Mean | Median | Min | Max | StdDev | Throughput |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Ingest native JSON array | 4.77 ms | 4.76 ms | 4.61 ms | 4.94 ms | 0.166 ms | 104,816 events/s |
| Ingest native NDJSON batch | 4.20 ms | 4.24 ms | 4.10 ms | 4.25 ms | 0.083 ms | 119,153 events/s |
| Ingest OTLP metrics (500 points) | 2.44 ms | 2.44 ms | 2.41 ms | 2.46 ms | 0.027 ms | 205,217 points/s |
| Ingest OTLP trace (3 spans) | 0.271 ms | 0.271 ms | 0.270 ms | 0.272 ms | 0.001 ms | 11,072 spans/s |

## Query

| Benchmark | Mean | Median | Min | Max | StdDev | Throughput |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Count: level = Error | 7.25 ms | 7.24 ms | 7.21 ms | 7.29 ms | 0.043 ms | 138 ops/s |
| Facets: sampled field values | 21.64 ms | 21.63 ms | 21.35 ms | 21.95 ms | 0.301 ms | 46 ops/s |
| Filter: level = Error | 10.67 ms | 10.80 ms | 10.36 ms | 10.85 ms | 0.272 ms | 94 ops/s |
| Filter: service = payments | 4.91 ms | 4.89 ms | 4.80 ms | 5.05 ms | 0.125 ms | 204 ops/s |
| Full-text: message contains error (scan) | 37.49 ms | 37.43 ms | 37.37 ms | 37.67 ms | 0.160 ms | 27 ops/s |
| Histogram: all logs (60 buckets) | 7.88 ms | 7.94 ms | 7.77 ms | 7.94 ms | 0.102 ms | 127 ops/s |
| Latest logs (limit 200) | 4.70 ms | 4.64 ms | 4.53 ms | 4.95 ms | 0.217 ms | 213 ops/s |
| List metric names | 0.197 ms | 0.197 ms | 0.196 ms | 0.198 ms | 0.001 ms | 5,087 ops/s |
| Lookup: traceId = ... (bloom filter) | 2.11 ms | 2.10 ms | 2.10 ms | 2.14 ms | 0.022 ms | 474 ops/s |
| Metric series (avg over range) | 0.285 ms | 0.285 ms | 0.284 ms | 0.286 ms | 0.001 ms | 3,512 ops/s |
| Range: durationMs > 1990 (block skip) | 124.29 ms | 124.19 ms | 124.01 ms | 124.67 ms | 0.340 ms | 8 ops/s |
| Trace by id (all spans) | 0.327 ms | 0.325 ms | 0.325 ms | 0.330 ms | 0.003 ms | 3,059 ops/s |
| Trace search (limit 50) | 5.59 ms | 5.59 ms | 5.53 ms | 5.66 ms | 0.065 ms | 179 ops/s |

