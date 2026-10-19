# Vyrtel benchmarks

- Generated: 2026-10-19 20:57:05 UTC
- Vyrtel: 0.1.1
- BenchmarkDotNet job: short
- Dataset: 200,000 seeded log events
- Machine: Ubuntu 24.04.5 LTS (X64), 4 logical cores
- Runtime: .NET 10.0.13
- Executable: 10.8 MB (release)
- Data directory: 22.0 MB on disk after seeding
- Storage: 203,200 events in 3 segments · 113.30 bytes/event · 4.08x compression · 7.5% index overhead

## Ingest

| Benchmark | Mean | Median | Min | Max | StdDev | Throughput |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Ingest native JSON array | 3.57 ms | 3.50 ms | 3.47 ms | 3.76 ms | 0.157 ms | 139,869 events/s |
| Ingest native NDJSON batch | 3.65 ms | 3.67 ms | 3.48 ms | 3.79 ms | 0.155 ms | 137,143 events/s |
| Ingest OTLP metrics (500 points) | 2.38 ms | 2.37 ms | 2.34 ms | 2.42 ms | 0.039 ms | 210,161 points/s |
| Ingest OTLP trace (3 spans) | 0.239 ms | 0.239 ms | 0.238 ms | 0.240 ms | 0.001 ms | 12,559 spans/s |

## Query

| Benchmark | Mean | Median | Min | Max | StdDev | Throughput |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Count: level = Error | 7.40 ms | 7.40 ms | 7.36 ms | 7.44 ms | 0.039 ms | 135 ops/s |
| Facets: sampled field values | 19.36 ms | 19.28 ms | 19.05 ms | 19.74 ms | 0.351 ms | 52 ops/s |
| Filter: level = Error | 11.23 ms | 11.25 ms | 10.99 ms | 11.45 ms | 0.231 ms | 89 ops/s |
| Filter: service = payments | 4.47 ms | 4.46 ms | 4.40 ms | 4.54 ms | 0.071 ms | 224 ops/s |
| Full-text: message contains error (scan) | 38.80 ms | 38.84 ms | 38.68 ms | 38.86 ms | 0.099 ms | 26 ops/s |
| Histogram: all logs (60 buckets) | 7.67 ms | 7.65 ms | 7.62 ms | 7.73 ms | 0.057 ms | 130 ops/s |
| Latest logs (limit 200) | 4.29 ms | 4.31 ms | 3.95 ms | 4.62 ms | 0.339 ms | 233 ops/s |
| List metric names | 0.161 ms | 0.161 ms | 0.161 ms | 0.162 ms | 0.001 ms | 6,204 ops/s |
| Lookup: traceId = ... (bloom filter) | 2.35 ms | 2.22 ms | 2.20 ms | 2.61 ms | 0.233 ms | 426 ops/s |
| Metric series (avg over range) | 0.260 ms | 0.255 ms | 0.254 ms | 0.272 ms | 0.010 ms | 3,840 ops/s |
| Range: durationMs > 1990 (block skip) | 123.57 ms | 123.57 ms | 123.53 ms | 123.60 ms | 0.037 ms | 8 ops/s |
| Trace by id (all spans) | 0.259 ms | 0.260 ms | 0.257 ms | 0.260 ms | 0.002 ms | 3,858 ops/s |
| Trace search (limit 50) | 5.10 ms | 5.10 ms | 5.08 ms | 5.11 ms | 0.019 ms | 196 ops/s |

