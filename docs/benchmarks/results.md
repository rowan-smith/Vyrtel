# Vyrtel benchmarks

- Generated: 2026-10-10 04:40:24 UTC
- Vyrtel: 0.1.1
- BenchmarkDotNet job: short
- Dataset: 200,000 seeded log events
- Machine: Microsoft Windows 10.0.26200 (X64), 22 logical cores
- Runtime: .NET 10.0.8

## Ingest

| Benchmark           |    Mean |  Median |     Min |     Max |   StdDev |      Throughput |
|---------------------|--------:|--------:|--------:|--------:|---------:|----------------:|
| Ingest NDJSON batch | 7.48 ms | 7.42 ms | 7.09 ms | 7.93 ms | 0.425 ms | 66,857 events/s |

## Query

| Benchmark                                |      Mean |    Median |       Min |       Max |   StdDev |  Throughput |
|------------------------------------------|----------:|----------:|----------:|----------:|---------:|------------:|
| Count: level = Error                     |  13.99 ms |  14.02 ms |  13.63 ms |  14.31 ms | 0.342 ms |    71 ops/s |
| Filter: level = Error                    |  22.60 ms |  20.54 ms |  19.94 ms |  27.33 ms |  4.10 ms |    44 ops/s |
| Filter: service = payments               |  12.86 ms |  11.16 ms |   9.60 ms |  17.82 ms |  4.37 ms |    78 ops/s |
| Full-text: message contains error (scan) |  69.35 ms |  69.51 ms |  61.25 ms |  77.28 ms |  8.02 ms |    14 ops/s |
| Latest logs (limit 200)                  |  10.35 ms |   9.93 ms |   9.35 ms |  11.76 ms |  1.25 ms |    97 ops/s |
| List metric names                        |  0.348 ms |  0.279 ms |  0.271 ms |  0.495 ms | 0.127 ms | 2,871 ops/s |
| Lookup: traceId = ... (bloom filter)     |   5.93 ms |   4.37 ms |   3.85 ms |   9.59 ms |  3.17 ms |   169 ops/s |
| Range: durationMs > 1990 (block skip)    | 336.75 ms | 332.69 ms | 271.80 ms | 405.76 ms | 67.07 ms |     3 ops/s |
| Trace search (limit 50)                  |   6.27 ms |   6.57 ms |   5.51 ms |   6.73 ms | 0.661 ms |   159 ops/s |

