# Vyrtel benchmarks

- Generated: 2026-10-23 11:19:46 UTC
- Vyrtel: 0.1.1
- BenchmarkDotNet job: short
- Dataset: 200,000 seeded log events
- Machine: Ubuntu 24.04.5 LTS (X64), 4 logical cores
- Runtime: .NET 10.0.13
- Executable: 10.8 MB (release)
- Data directory: 21.9 MB on disk after seeding
- Storage: 203,200 events in 3 segments · 113.17 bytes/event · 4.09x compression · 7.5% index overhead

## Ingest

| Benchmark | Mean | Median | Min | Max | StdDev | Throughput |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Ingest native JSON array | 2.49 ms | 2.47 ms | 2.40 ms | 2.60 ms | 0.103 ms | 200,939 events/s |
| Ingest native NDJSON batch | 2.53 ms | 2.59 ms | 2.21 ms | 2.78 ms | 0.290 ms | 197,745 events/s |
| Ingest OTLP metrics (500 points) | 1.54 ms | 1.53 ms | 1.53 ms | 1.55 ms | 0.011 ms | 325,662 points/s |
| Ingest OTLP trace (3 spans) | 0.126 ms | 0.125 ms | 0.123 ms | 0.131 ms | 0.004 ms | 23,757 spans/s |

## Query

| Benchmark | Mean | Median | Min | Max | StdDev | Throughput |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Count: level = Error | 4.28 ms | 4.28 ms | 4.26 ms | 4.31 ms | 0.024 ms | 233 ops/s |
| Facets: sampled field values | 14.12 ms | 14.29 ms | 13.22 ms | 14.83 ms | 0.821 ms | 71 ops/s |
| Filter: level = Error | 7.10 ms | 6.97 ms | 6.78 ms | 7.55 ms | 0.399 ms | 141 ops/s |
| Filter: service = payments | 2.77 ms | 2.83 ms | 2.63 ms | 2.84 ms | 0.120 ms | 362 ops/s |
| Full-text: message contains error (scan) | 25.40 ms | 26.04 ms | 23.06 ms | 27.11 ms | 2.09 ms | 39 ops/s |
| Histogram: all logs (60 buckets) | 4.31 ms | 4.29 ms | 4.26 ms | 4.37 ms | 0.060 ms | 232 ops/s |
| Latest logs (limit 200) | 2.33 ms | 2.32 ms | 2.30 ms | 2.38 ms | 0.045 ms | 429 ops/s |
| List metric names | 0.113 ms | 0.112 ms | 0.112 ms | 0.114 ms | 0.001 ms | 8,869 ops/s |
| Lookup: traceId = ... (bloom filter) | 1.13 ms | 1.11 ms | 1.07 ms | 1.21 ms | 0.070 ms | 886 ops/s |
| Metric series (avg over range) | 0.163 ms | 0.166 ms | 0.153 ms | 0.169 ms | 0.008 ms | 6,153 ops/s |
| Range: durationMs > 1990 (block skip) | 86.54 ms | 84.30 ms | 82.64 ms | 92.68 ms | 5.38 ms | 12 ops/s |
| Trace by id (all spans) | 0.169 ms | 0.170 ms | 0.164 ms | 0.174 ms | 0.005 ms | 5,904 ops/s |
| Trace search (limit 50) | 1.77 ms | 1.76 ms | 1.75 ms | 1.80 ms | 0.028 ms | 564 ops/s |

