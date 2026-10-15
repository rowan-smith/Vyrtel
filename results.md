# Vyrtel benchmarks

- Generated: 2026-10-15 13:31:29 UTC
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
| Ingest native JSON array | 4.98 ms | 4.79 ms | 4.78 ms | 5.37 ms | 0.339 ms | 100,352 events/s |
| Ingest native NDJSON batch | 4.64 ms | 4.64 ms | 4.61 ms | 4.66 ms | 0.024 ms | 107,873 events/s |
| Ingest OTLP metrics (500 points) | 2.51 ms | 2.50 ms | 2.49 ms | 2.54 ms | 0.023 ms | 199,187 points/s |
| Ingest OTLP trace (3 spans) | 0.181 ms | 0.181 ms | 0.181 ms | 0.182 ms | 0.001 ms | 16,556 spans/s |

## Query

| Benchmark | Mean | Median | Min | Max | StdDev | Throughput |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Count: level = Error | 6.33 ms | 6.34 ms | 6.30 ms | 6.36 ms | 0.032 ms | 158 ops/s |
| Facets: sampled field values | 20.63 ms | 20.55 ms | 20.53 ms | 20.81 ms | 0.158 ms | 48 ops/s |
| Filter: level = Error | 9.63 ms | 9.68 ms | 9.44 ms | 9.76 ms | 0.168 ms | 104 ops/s |
| Filter: service = payments | 4.64 ms | 4.59 ms | 4.58 ms | 4.75 ms | 0.094 ms | 216 ops/s |
| Full-text: message contains error (scan) | 36.25 ms | 36.08 ms | 36.08 ms | 36.60 ms | 0.299 ms | 28 ops/s |
| Histogram: all logs (60 buckets) | 6.76 ms | 6.74 ms | 6.73 ms | 6.82 ms | 0.050 ms | 148 ops/s |
| Latest logs (limit 200) | 4.05 ms | 4.05 ms | 3.97 ms | 4.13 ms | 0.082 ms | 247 ops/s |
| List metric names | 0.122 ms | 0.122 ms | 0.122 ms | 0.123 ms | 0.000 ms | 8,196 ops/s |
| Lookup: traceId = ... (bloom filter) | 2.03 ms | 2.02 ms | 2.01 ms | 2.06 ms | 0.023 ms | 493 ops/s |
| Metric series (avg over range) | 0.212 ms | 0.211 ms | 0.209 ms | 0.216 ms | 0.003 ms | 4,718 ops/s |
| Range: durationMs > 1990 (block skip) | 126.42 ms | 126.03 ms | 125.41 ms | 127.83 ms | 1.26 ms | 8 ops/s |
| Trace by id (all spans) | 0.266 ms | 0.266 ms | 0.264 ms | 0.268 ms | 0.002 ms | 3,758 ops/s |
| Trace search (limit 50) | 3.12 ms | 3.13 ms | 3.08 ms | 3.16 ms | 0.038 ms | 320 ops/s |

