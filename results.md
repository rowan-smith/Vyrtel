# Vyrtel benchmarks

- Generated: 2026-10-11 01:40:53 UTC
- Vyrtel: 0.1.1
- BenchmarkDotNet job: short
- Dataset: 200,000 seeded log events
- Machine: Ubuntu 24.04.5 LTS (X64), 4 logical cores
- Runtime: .NET 10.0.12
- Executable: 10.4 MB (release)
- Data directory: 22.7 MB on disk after seeding
- Storage: 203,200 events in 3 segments · 116.93 bytes/event · 4.06x compression · 7.5% index overhead

## Ingest

| Benchmark | Mean | Median | Min | Max | StdDev | Throughput |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Ingest native JSON array | 5.16 ms | 5.13 ms | 4.97 ms | 5.37 ms | 0.201 ms | 96,889 events/s |
| Ingest native NDJSON batch | 4.86 ms | 4.86 ms | 4.73 ms | 4.99 ms | 0.130 ms | 102,888 events/s |
| Ingest OTLP metrics (500 points) | 2.42 ms | 2.43 ms | 2.37 ms | 2.45 ms | 0.045 ms | 206,858 points/s |
| Ingest OTLP trace (3 spans) | 0.273 ms | 0.272 ms | 0.271 ms | 0.275 ms | 0.002 ms | 10,992 spans/s |

## Query

| Benchmark | Mean | Median | Min | Max | StdDev | Throughput |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Count: level = Error | 7.30 ms | 7.31 ms | 7.29 ms | 7.32 ms | 0.014 ms | 137 ops/s |
| Facets: sampled field values | 21.81 ms | 21.78 ms | 21.72 ms | 21.94 ms | 0.115 ms | 46 ops/s |
| Filter: level = Error | 10.87 ms | 10.89 ms | 10.83 ms | 10.90 ms | 0.037 ms | 92 ops/s |
| Filter: service = payments | 6.04 ms | 6.05 ms | 5.81 ms | 6.27 ms | 0.230 ms | 165 ops/s |
| Full-text: message contains error (scan) | 37.22 ms | 37.09 ms | 37.08 ms | 37.47 ms | 0.222 ms | 27 ops/s |
| Histogram: all logs (60 buckets) | 7.65 ms | 7.67 ms | 7.60 ms | 7.68 ms | 0.045 ms | 131 ops/s |
| Latest logs (limit 200) | 5.06 ms | 5.08 ms | 4.98 ms | 5.12 ms | 0.072 ms | 198 ops/s |
| List metric names | 0.196 ms | 0.196 ms | 0.195 ms | 0.197 ms | 0.001 ms | 5,103 ops/s |
| Lookup: traceId = ... (bloom filter) | 2.90 ms | 2.89 ms | 2.89 ms | 2.93 ms | 0.023 ms | 345 ops/s |
| Metric series (avg over range) | 0.288 ms | 0.288 ms | 0.285 ms | 0.291 ms | 0.003 ms | 3,474 ops/s |
| Range: durationMs > 1990 (block skip) | 125.81 ms | 125.49 ms | 124.97 ms | 126.97 ms | 1.04 ms | 8 ops/s |
| Trace by id (all spans) | 0.326 ms | 0.325 ms | 0.322 ms | 0.330 ms | 0.004 ms | 3,072 ops/s |
| Trace search (limit 50) | 5.75 ms | 5.78 ms | 5.64 ms | 5.85 ms | 0.108 ms | 174 ops/s |

