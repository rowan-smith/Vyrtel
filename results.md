# Vyrtel benchmarks

- Generated: 2026-10-27 11:14:28 UTC
- Vyrtel: 0.1.1
- BenchmarkDotNet job: short
- Dataset: 200,000 seeded log events
- Machine: Ubuntu 26.04.1 LTS (X64), 4 logical cores
- Runtime: .NET 10.0.14
- Executable: 12.8 MB (release)
- Data directory: 22.0 MB on disk after seeding
- Storage: 203,200 events in 3 segments · 113.36 bytes/event · 4.07x compression · 7.5% index overhead

## Ingest

| Benchmark | Mean | Median | Min | Max | StdDev | Throughput |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Ingest native JSON array | 3.93 ms | 3.91 ms | 3.88 ms | 4.01 ms | 0.070 ms | 127,094 events/s |
| Ingest native NDJSON batch | 3.59 ms | 3.54 ms | 3.25 ms | 3.96 ms | 0.357 ms | 139,384 events/s |
| Ingest OTLP metrics (500 points) | 2.25 ms | 2.26 ms | 2.22 ms | 2.28 ms | 0.027 ms | 222,109 points/s |
| Ingest OTLP trace (3 spans) | 0.229 ms | 0.230 ms | 0.227 ms | 0.230 ms | 0.001 ms | 13,106 spans/s |

## Query

| Benchmark | Mean | Median | Min | Max | StdDev | Throughput |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Count: level = Error | 7.17 ms | 7.18 ms | 7.16 ms | 7.18 ms | 0.007 ms | 139 ops/s |
| Facets: sampled field values | 15.72 ms | 15.28 ms | 15.09 ms | 16.79 ms | 0.931 ms | 64 ops/s |
| Filter: level = Error | 9.89 ms | 9.82 ms | 9.78 ms | 10.05 ms | 0.147 ms | 101 ops/s |
| Filter: service = payments | 4.01 ms | 3.97 ms | 3.97 ms | 4.07 ms | 0.058 ms | 250 ops/s |
| Full-text: message contains error (scan) | 34.58 ms | 34.52 ms | 34.51 ms | 34.71 ms | 0.110 ms | 29 ops/s |
| Histogram: all logs (60 buckets) | 7.42 ms | 7.38 ms | 7.35 ms | 7.51 ms | 0.085 ms | 135 ops/s |
| Latest logs (limit 200) | 3.71 ms | 3.57 ms | 3.51 ms | 4.04 ms | 0.288 ms | 270 ops/s |
| List metric names | 0.163 ms | 0.163 ms | 0.163 ms | 0.163 ms | 0.001 ms | 6,134 ops/s |
| Lookup: traceId = ... (bloom filter) | 1.92 ms | 1.92 ms | 1.92 ms | 1.92 ms | 0.002 ms | 521 ops/s |
| Metric series (avg over range) | 0.250 ms | 0.250 ms | 0.248 ms | 0.253 ms | 0.002 ms | 3,995 ops/s |
| Range: durationMs > 1990 (block skip) | 111.21 ms | 110.62 ms | 110.07 ms | 112.96 ms | 1.53 ms | 9 ops/s |
| Trace by id (all spans) | 0.270 ms | 0.270 ms | 0.270 ms | 0.271 ms | 0.001 ms | 3,700 ops/s |
| Trace search (limit 50) | 4.63 ms | 4.63 ms | 4.62 ms | 4.63 ms | 0.006 ms | 216 ops/s |

