# Vyrtel benchmarks

- Generated: 2026-10-16 09:17:55 UTC
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
| Ingest native JSON array | 4.31 ms | 4.26 ms | 4.24 ms | 4.44 ms | 0.108 ms | 115,914 events/s |
| Ingest native NDJSON batch | 4.33 ms | 4.31 ms | 4.23 ms | 4.44 ms | 0.107 ms | 115,553 events/s |
| Ingest OTLP metrics (500 points) | 2.26 ms | 2.22 ms | 2.16 ms | 2.41 ms | 0.129 ms | 221,031 points/s |
| Ingest OTLP trace (3 spans) | 0.271 ms | 0.271 ms | 0.270 ms | 0.274 ms | 0.002 ms | 11,054 spans/s |

## Query

| Benchmark | Mean | Median | Min | Max | StdDev | Throughput |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Count: level = Error | 7.31 ms | 7.33 ms | 7.21 ms | 7.38 ms | 0.089 ms | 137 ops/s |
| Facets: sampled field values | 22.47 ms | 22.52 ms | 22.26 ms | 22.63 ms | 0.191 ms | 45 ops/s |
| Filter: level = Error | 10.32 ms | 10.20 ms | 10.02 ms | 10.74 ms | 0.377 ms | 97 ops/s |
| Filter: service = payments | 5.22 ms | 5.12 ms | 5.11 ms | 5.42 ms | 0.179 ms | 192 ops/s |
| Full-text: message contains error (scan) | 37.90 ms | 38.02 ms | 37.55 ms | 38.13 ms | 0.306 ms | 26 ops/s |
| Histogram: all logs (60 buckets) | 7.70 ms | 7.70 ms | 7.68 ms | 7.71 ms | 0.011 ms | 130 ops/s |
| Latest logs (limit 200) | 4.17 ms | 4.15 ms | 4.13 ms | 4.23 ms | 0.050 ms | 240 ops/s |
| List metric names | 0.194 ms | 0.195 ms | 0.191 ms | 0.196 ms | 0.003 ms | 5,158 ops/s |
| Lookup: traceId = ... (bloom filter) | 2.28 ms | 2.25 ms | 2.25 ms | 2.34 ms | 0.053 ms | 438 ops/s |
| Metric series (avg over range) | 0.288 ms | 0.288 ms | 0.286 ms | 0.291 ms | 0.002 ms | 3,467 ops/s |
| Range: durationMs > 1990 (block skip) | 126.96 ms | 126.09 ms | 125.21 ms | 129.56 ms | 2.30 ms | 8 ops/s |
| Trace by id (all spans) | 0.328 ms | 0.328 ms | 0.327 ms | 0.331 ms | 0.002 ms | 3,045 ops/s |
| Trace search (limit 50) | 5.83 ms | 5.90 ms | 5.66 ms | 5.93 ms | 0.146 ms | 171 ops/s |

