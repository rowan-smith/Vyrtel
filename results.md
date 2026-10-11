# Vyrtel benchmarks

- Generated: 2026-10-11 14:11:24 UTC
- Vyrtel: 0.1.1
- BenchmarkDotNet job: short
- Dataset: 200,000 seeded log events
- Machine: Ubuntu 24.04.5 LTS (X64), 4 logical cores
- Runtime: .NET 10.0.12
- Executable: 10.6 MB (release)
- Data directory: 22.6 MB on disk after seeding
- Storage: 203,200 events in 3 segments · 116.64 bytes/event · 4.08x compression · 7.5% index overhead

## Ingest

| Benchmark | Mean | Median | Min | Max | StdDev | Throughput |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Ingest native JSON array | 3.18 ms | 2.99 ms | 2.97 ms | 3.58 ms | 0.347 ms | 157,197 events/s |
| Ingest native NDJSON batch | 3.01 ms | 3.00 ms | 2.77 ms | 3.25 ms | 0.242 ms | 166,377 events/s |
| Ingest OTLP metrics (500 points) | 1.72 ms | 1.72 ms | 1.71 ms | 1.73 ms | 0.013 ms | 290,628 points/s |
| Ingest OTLP trace (3 spans) | 0.114 ms | 0.114 ms | 0.111 ms | 0.117 ms | 0.003 ms | 26,302 spans/s |

## Query

| Benchmark | Mean | Median | Min | Max | StdDev | Throughput |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Count: level = Error | 3.90 ms | 3.91 ms | 3.89 ms | 3.91 ms | 0.015 ms | 256 ops/s |
| Facets: sampled field values | 13.54 ms | 13.29 ms | 13.23 ms | 14.11 ms | 0.492 ms | 74 ops/s |
| Filter: level = Error | 5.70 ms | 5.60 ms | 5.54 ms | 5.96 ms | 0.225 ms | 175 ops/s |
| Filter: service = payments | 2.59 ms | 2.58 ms | 2.55 ms | 2.63 ms | 0.044 ms | 387 ops/s |
| Full-text: message contains error (scan) | 21.19 ms | 21.07 ms | 20.89 ms | 21.60 ms | 0.372 ms | 47 ops/s |
| Histogram: all logs (60 buckets) | 4.02 ms | 4.02 ms | 3.98 ms | 4.05 ms | 0.035 ms | 249 ops/s |
| Latest logs (limit 200) | 2.30 ms | 2.31 ms | 2.28 ms | 2.32 ms | 0.020 ms | 434 ops/s |
| List metric names | 0.065 ms | 0.063 ms | 0.061 ms | 0.071 ms | 0.006 ms | 15,369 ops/s |
| Lookup: traceId = ... (bloom filter) | 0.991 ms | 0.992 ms | 0.985 ms | 0.997 ms | 0.006 ms | 1,009 ops/s |
| Metric series (avg over range) | 0.125 ms | 0.124 ms | 0.124 ms | 0.126 ms | 0.001 ms | 8,015 ops/s |
| Range: durationMs > 1990 (block skip) | 79.28 ms | 78.56 ms | 78.08 ms | 81.19 ms | 1.67 ms | 13 ops/s |
| Trace by id (all spans) | 0.147 ms | 0.144 ms | 0.142 ms | 0.156 ms | 0.008 ms | 6,798 ops/s |
| Trace search (limit 50) | 1.88 ms | 1.86 ms | 1.85 ms | 1.93 ms | 0.046 ms | 532 ops/s |

