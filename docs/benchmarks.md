# Benchmarks

Vyrtel is measured on the same machine on every run, so the numbers stay comparable over
time. The [.NET benchmark harness](../dotnet/Vyrtel.Benchmark) launches a release build of
the server against a throw-away data directory, seeds a deterministic dataset, then drives
it over HTTP with [BenchmarkDotNet](https://benchmarkdotnet.org/).

## How to read this

- **Mean / Median / Min / Max / StdDev** are wall-clock latency per request, in
  milliseconds. Lower is better.
- **Throughput** is the sustained rate: events per second for ingest, operations per second
  for queries.
- Query timings include the cost of serializing the response over the loopback interface,
  not just server CPU.

## Methodology

- **Server**: `cargo build --release`, launched with default durability against a temporary
  data directory on the CI machine.
- **Data**: a deterministic mix of HTTP request logs across 8 services with a 2% error rate
  and ~10k distinct customers, plus OTLP traces. The same `EventFactory` backs the load
  generator, the benchmark and the profiler, so the workload stays realistic.
- **Job**: BenchmarkDotNet `ShortRun` (`--job short`). CI uses the same command as a local
  run, so results are reproducible.

Reproduce locally:

```shell
cargo build --release -p server
dotnet run --project dotnet/Vyrtel.Benchmark -c Release -- --job short
```

For a latency breakdown under a mixed read/write load (p50/p90/p95/p99 per operation, plus
the server's own index hotspots), use the profiler:

```shell
dotnet run --project dotnet/Vyrtel.Profiling -c Release -- --duration 30 --concurrency 4
```

The rollup below is regenerated automatically on the CI machine whenever `main` changes.

<!--BENCHMARK_RESULTS-->
