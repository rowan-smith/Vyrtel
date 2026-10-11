# Vyrtel benchmarks

- Generated: 2026-10-11 08:29:29 UTC
- Vyrtel: 0.1.1
- BenchmarkDotNet job: short
- Dataset: 200,000 seeded log events
- Machine: Ubuntu 24.04.5 LTS (X64), 4 logical cores
- Runtime: .NET 10.0.12
- Executable: 10.5 MB (release)
- Data directory: 22.6 MB on disk after seeding
- Storage: 203,200 events in 3 segments · 116.70 bytes/event · 4.08x compression · 7.5% index overhead

## Ingest

| Benchmark | Mean | Median | Min | Max | StdDev | Throughput |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Ingest native JSON array | 4.04 ms | 3.86 ms | 3.71 ms | 4.54 ms | 0.444 ms | 123,866 events/s |
| Ingest native NDJSON batch | 4.34 ms | 4.50 ms | 4.01 ms | 4.50 ms | 0.280 ms | 115,249 events/s |
| Ingest OTLP metrics (500 points) | 1.68 ms | 1.65 ms | 1.63 ms | 1.74 ms | 0.059 ms | 298,268 points/s |
| Ingest OTLP trace (3 spans) | 0.119 ms | 0.120 ms | 0.115 ms | 0.120 ms | 0.003 ms | 25,302 spans/s |

## Query

| Benchmark | Mean | Median | Min | Max | StdDev | Throughput |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Count: level = Error | 4.43 ms | 4.38 ms | 4.38 ms | 4.52 ms | 0.081 ms | 226 ops/s |
| Facets: sampled field values | 16.23 ms | 16.03 ms | 15.94 ms | 16.72 ms | 0.425 ms | 62 ops/s |
| Filter: level = Error | 6.86 ms | 6.71 ms | 6.68 ms | 7.20 ms | 0.289 ms | 146 ops/s |
| Filter: service = payments | 3.01 ms | 3.00 ms | 2.97 ms | 3.07 ms | 0.049 ms | 332 ops/s |
| Full-text: message contains error (scan) | 27.55 ms | 26.91 ms | 26.63 ms | 29.11 ms | 1.36 ms | 36 ops/s |
| Histogram: all logs (60 buckets) | 4.35 ms | 4.32 ms | 4.31 ms | 4.42 ms | 0.057 ms | 230 ops/s |
| Latest logs (limit 200) | 2.59 ms | 2.58 ms | 2.56 ms | 2.64 ms | 0.038 ms | 386 ops/s |
| List metric names | 0.072 ms | 0.071 ms | 0.070 ms | 0.074 ms | 0.002 ms | 13,948 ops/s |
| Lookup: traceId = ... (bloom filter) | 1.13 ms | 1.11 ms | 1.09 ms | 1.20 ms | 0.057 ms | 883 ops/s |
| Metric series (avg over range) | 0.135 ms | 0.134 ms | 0.134 ms | 0.135 ms | 0.001 ms | 7,434 ops/s |
| Range: durationMs > 1990 (block skip) | 80.82 ms | 80.32 ms | 80.08 ms | 82.06 ms | 1.08 ms | 12 ops/s |
| Trace by id (all spans) | 0.151 ms | 0.151 ms | 0.151 ms | 0.152 ms | 0.000 ms | 6,608 ops/s |
| Trace search (limit 50) | 2.21 ms | 2.20 ms | 2.12 ms | 2.32 ms | 0.101 ms | 452 ops/s |

