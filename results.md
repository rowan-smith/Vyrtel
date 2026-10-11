# Vyrtel benchmarks

- Generated: 2026-10-11 07:42:11 UTC
- Vyrtel: 0.1.1
- BenchmarkDotNet job: short
- Dataset: 200,000 seeded log events
- Machine: Ubuntu 24.04.5 LTS (X64), 4 logical cores
- Runtime: .NET 10.0.12
- Executable: 10.5 MB (release)
- Data directory: 22.6 MB on disk after seeding
- Storage: 203,200 events in 3 segments · 116.67 bytes/event · 4.08x compression · 7.5% index overhead

## Ingest

| Benchmark | Mean | Median | Min | Max | StdDev | Throughput |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Ingest native JSON array | 3.85 ms | 3.83 ms | 3.79 ms | 3.93 ms | 0.074 ms | 129,890 events/s |
| Ingest native NDJSON batch | 3.80 ms | 3.78 ms | 3.44 ms | 4.18 ms | 0.373 ms | 131,599 events/s |
| Ingest OTLP metrics (500 points) | 1.86 ms | 1.88 ms | 1.76 ms | 1.93 ms | 0.087 ms | 269,477 points/s |
| Ingest OTLP trace (3 spans) | 0.176 ms | 0.173 ms | 0.173 ms | 0.181 ms | 0.005 ms | 17,082 spans/s |

## Query

| Benchmark | Mean | Median | Min | Max | StdDev | Throughput |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Count: level = Error | 5.84 ms | 5.85 ms | 5.76 ms | 5.91 ms | 0.075 ms | 171 ops/s |
| Facets: sampled field values | 19.57 ms | 19.64 ms | 18.47 ms | 20.59 ms | 1.06 ms | 51 ops/s |
| Filter: level = Error | 8.50 ms | 8.43 ms | 8.41 ms | 8.66 ms | 0.140 ms | 118 ops/s |
| Filter: service = payments | 3.91 ms | 3.86 ms | 3.82 ms | 4.06 ms | 0.127 ms | 256 ops/s |
| Full-text: message contains error (scan) | 33.02 ms | 33.09 ms | 32.83 ms | 33.14 ms | 0.166 ms | 30 ops/s |
| Histogram: all logs (60 buckets) | 5.95 ms | 5.95 ms | 5.93 ms | 5.98 ms | 0.022 ms | 168 ops/s |
| Latest logs (limit 200) | 3.79 ms | 3.82 ms | 3.70 ms | 3.87 ms | 0.088 ms | 264 ops/s |
| List metric names | 0.111 ms | 0.112 ms | 0.109 ms | 0.112 ms | 0.002 ms | 8,994 ops/s |
| Lookup: traceId = ... (bloom filter) | 1.55 ms | 1.55 ms | 1.54 ms | 1.56 ms | 0.010 ms | 646 ops/s |
| Metric series (avg over range) | 0.187 ms | 0.187 ms | 0.186 ms | 0.187 ms | 0.000 ms | 5,356 ops/s |
| Range: durationMs > 1990 (block skip) | 103.52 ms | 103.62 ms | 102.61 ms | 104.33 ms | 0.867 ms | 10 ops/s |
| Trace by id (all spans) | 0.215 ms | 0.214 ms | 0.213 ms | 0.218 ms | 0.003 ms | 4,654 ops/s |
| Trace search (limit 50) | 4.04 ms | 4.04 ms | 4.03 ms | 4.04 ms | 0.005 ms | 247 ops/s |

