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
- **Executable** is the size of the release binary the server under test ran, and **Data
  directory** the on-disk footprint of the seeded dataset.
- **Storage** comes from the server itself: bytes stored per event, compression ratio and
  index overhead for the seeded dataset.
- Query timings include the cost of serializing the response over the loopback interface,
  not just server CPU.

## Methodology

- **Server**: `cargo build --release`, launched with default durability against a temporary
  data directory on the CI machine.
- **Data**: a deterministic mix of HTTP request logs across 8 services with a 2% error rate
  and ~10k distinct customers, plus OTLP traces and metric points. The same `EventFactory`
  backs the load generator, the benchmark and the profiler, so the workload stays realistic.
- **Coverage**: ingest throughput (native NDJSON, native JSON array, OTLP traces, OTLP metrics)
  and query latency across filters, full-text scans, counts, histograms, facets, trace
  lookup/search and metric series.
- **Job**: BenchmarkDotNet `ShortRun` (`--job short`). CI uses the same command as a local
  run, so results are reproducible.
- **Footprint**: right after seeding, the harness records the release binary size, the data
  directory on disk and the server's own storage counters. Those are captured before the
  benchmark traffic starts, so they describe the dataset rather than the load.

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

The profiler discards the first few seconds as warmup (`--warmup`, default 3) and diffs the
server's query-stats counters across the measured window, so the reported hotspots cover only
the load test — not dataset seeding.

The rollup below is regenerated automatically on the CI machine whenever `main` changes.

`.github/workflows/benchmark.yml` runs the benchmark on every `main` change and commits the
rollup rendered below. Pushing a release tag (`v*`) captures a separate snapshot under
`docs/benchmarks/versions/<tag>.json`; the **Version history** section charts those snapshots
so improvements between releases are visible. Use `--label <tag> --out docs/benchmarks/versions/<tag>.json`
to produce one locally.

<!--BENCHMARK_RESULTS-->
