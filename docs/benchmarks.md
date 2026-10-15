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

## Reproducible baselines

The rollup at the end of this page tracks trends on one CI machine. To decide whether a change
made Vyrtel slower or heavier, compare like with like: the same seeded workload, the same pinned
server configuration and the same host, repeated often enough to know how noisy that host is.
[`dotnet/Vyrtel.Baseline`](../dotnet/Vyrtel.Baseline) does this and writes a report a reviewer
can check.

```shell
cargo build --release -p server
dotnet run --project dotnet/Vyrtel.Baseline -c Release -- --runs 5 --label main --notes "NVMe SSD, AC power"
```

The defaults take about a minute per run on a laptop. Reports go to `target/baseline/`:
`report.json` (the record that comparisons read), `report.md` (for review) and
`report.samples.csv` (the latency and HTTP status of every request).

### What one run does

Each run is independent: a new temporary data directory and a newly launched release server
with the [pinned configuration](#pinned-configuration) on `127.0.0.1:18480`. If anything is
already listening on that port the run stops, because it could not know that server's
configuration (use `--port` to choose another).

1. **Seed.** 100,000 log events (NDJSON, 500 per request), 1,000 OTLP traces and 100 metric
   points from the same `EventFactory` as the benchmark and profiler (seed 42), sent one request
   at a time. A 429 or 503 counts as backpressure and is retried after 200 ms, so the dataset
   always arrives complete. Any other rejection stops the run: an incomplete dataset would make
   every later number wrong. The run then waits until every queue is empty and checks the
   dataset: the log events, spans and metric points counted from the fixture payloads must equal
   both what the server acknowledged and what `GET /api/v1/system/info` reports storing, per
   signal. Any shortfall or surplus stops the run, because a server that loses acknowledged events
   would leave a smaller dataset and every query would look faster. It then records the storage
   counters and data directory size.
2. **Cold and warm queries.** For each of the 13 queries below, the server is restarted on the
   same data directory, the connection is opened with `GET /health`, and the query is timed
   once (**cold**: a new process with an empty index cache and nothing decoded) and then 20
   more times (**warm**). The server's own `diagnostics` are kept for the cold request and the
   last warm one. Every one of these requests must succeed: an HTTP error or no response (a
   timeout, a refused connection) stops the run. A failed request has no latency worth
   comparing, and timing only the successes would let a broken endpoint record nothing, or 0 ms,
   and look like an improvement.
3. **Mixed load.** 4 workers send a weighted mix of NDJSON and JSON ingest (800 events per
   request) and queries for 20 s; the first 3 s are discarded. Each worker's sequence comes from
   its own seed, so every run sends the same requests in the same order; only how many fit in
   the time varies. Every request is classified as accepted, backpressure (429/503), too large
   (413), other rejection or transport error, per operation, with p50/p90/p95/p99 latency.
4. **Memory.** Server RSS is sampled every 100 ms in every phase, and the OS's own high-water
   mark (`PeakWorkingSet` on Windows, `VmHWM` on Linux) is read before each server stops. The
   report gives the larger of the two and says which it was.

| Query | Kind | Exercises |
|---|---|---|
| `level = Error`, `service = payments` | selective | bitmap indexes |
| `traceId = "…"` | selective | Bloom filters (skips most segments and blocks) |
| `durationMs > 1990` | selective | zone maps |
| count `level = Error` | selective | counting without returning events |
| `message contains "timed out"`, `message contains "error"` | scan | full-text scan, no index |
| latest 200 logs | recent | reading newest blocks first |
| histogram (60 buckets), facets (2,000-event sample) | aggregate | aggregation over the range |
| trace search, trace by id | trace | span storage |
| metric series | metric | metric storage |

Options: `--runs`, `--seed`, `--events`, `--batch`, `--warm-iterations`, `--duration`, `--warmup`,
`--concurrency`, `--port`, `--server`, `--config`, `--label`, `--notes`, `--out`, `--compare`,
`--log-level`, `--keep-data` (`--help` describes each). Changing any workload option makes the
report incomparable with earlier ones, and a comparison between them fails.

### Pinned configuration

[`dotnet/Vyrtel.Baseline/baseline.toml`](../dotnet/Vyrtel.Baseline/baseline.toml) writes out every
setting that affects throughput, latency or memory, so a change to Vyrtel's defaults cannot
silently move a baseline. It matches the defaults except:

| Setting | Baseline | Default | Why |
|---|---|---|---|
| `storage.segment_target_size` | `4MB` | `64MB` | The seeded data is sealed into several segments, so queries exercise segment pruning, index loading and decompression instead of only the in-memory active segment. |
| `storage.compaction` | `false` | `true` | A background merge starting partway through a measurement would make repeated runs disagree. |
| `alerts.enabled` | `false` | `true` | No rule evaluation in the background. |
| `ingest.max_concurrent` | `3` | `0` (derived: 3) | Written out so a change to the derivation shows up as a configuration change. A unit test (`baseline_config_file_is_valid_and_pins_the_derived_ingest_concurrency`) fails if the two drift apart or the file stops loading. |

The report records the file's SHA-256, the effective configuration from
`GET /api/v1/system/config` and the ingest admission from `GET /api/v1/system/info`. Editing the
file starts a new baseline: re-record it before comparing.

### Reading the report

| Section | What it states |
|---|---|
| Header | Generation time, label, Vyrtel version and commit (flagged if the tree had uncommitted changes), total and per-run **duration**, the workload |
| Host and build | **CPU** model and logical cores, memory available, OS and architecture, the volume holding the data directories (file system, free space), harness runtime, your `--notes`, **binary size**, profile and SHA-256, `rustc` version, configuration SHA-256, ingest admission, and the **compression method** (zstd level, ratio, index overhead, bytes per event, segment count) |
| Memory | **Peak RSS** per run and per phase (seeding, queries, mixed load), idle RSS after start, and whether the peak came from the OS or sampling |
| Accepted and rejected requests | Per run and phase: requests, accepted, backpressure, too large, other rejections, transport errors, items accepted, throughput and latency percentiles. The JSON also holds the seeded dataset's expected, acknowledged and stored counts per signal |
| Mixed load by operation | p50/p95/p99 per operation, averaged across runs with the run-to-run spread, and rejections |
| Queries: cold and warm | Cold and warm p50/p95, their ratio, and diagnostics: segments skipped of considered, blocks read (skipped), events examined → matched, bytes read cold → warm, the indexes used |
| Variance and budgets | For every metric: runs, mean, standard deviation, coefficient of variation (CV), min, max, the proposed budget and the arithmetic behind it |

The JSON keeps every run's raw numbers and diagnostics; the CSV keeps every request. Nothing
in the report asserts an absolute number: what is fast on one machine says little about another.

### Regression budgets

A budget is how much worse than the baseline's mean a new mean may be before it counts as a
regression. Budgets are derived from the measured run-to-run variation, not chosen per metric:

```
budget = max(floor, 3 × CV × k(n), rounded up)
CV     = sample standard deviation ÷ mean, over the baseline's n runs
k(n)   = √((n − 1) ÷ χ²₀.₁₀(n − 1))     3 runs: 3.08   5 runs: 1.94   10 runs: 1.47
```

- **3 standard deviations.** A mean three standard deviations worse than the baseline is
  unlikely to be noise.
- **k(n): an upper bound on the spread.** A few runs usually underestimate the true spread, so the
  CV is first raised to its 90% upper confidence bound (assuming roughly normal noise). Without
  this, two baselines of the *same* commit on the reference laptop disagreed: 8 of 74 metrics
  moved by more than three of the first baseline's 3-run standard deviations, and the comparison
  reported a regression (latest logs, warm p95, +11.6% against a 10% budget). With both baselines'
  six runs pooled, none did. More runs give a smaller k(n) and tighter budgets.
- **Rounding and floors.** Timings round up to a multiple of 5%, sizes to 1%. Within-session
  runs run back to back, so they cannot see drift *between* sessions (temperature, background
  load, power state). The floors cover it, and are set from measurements rather than chosen:
  across four baselines of the same commit recorded in separate sessions on the reference laptop,
  timings whose spread-based budget was under 10% drifted by up to 11.2%, so the timing floor is
  **15%**; data-directory size varied 0.55% across all 16 runs and the binary not at all, so the
  size floor is **2%**. On another host, check the floors with a
  [same-commit comparison](#checking-a-host-with-a-same-commit-comparison).
- **Enough samples for a percentile.** A percentile is gated only when every run had at least 10
  samples beyond it: 20 for p50, 200 for p95, 1,000 for p99. A per-run p99 from 400 requests is
  decided by its top four, and the p95 of 20 warm repetitions is effectively the second largest,
  so one scheduler stall would decide the verdict. Such metrics are still reported; raise
  `--warm-iterations` or `--duration` to gate them.
- **Only successful work is throughput.** The gated throughput metrics are successful requests
  per second and accepted ingest events per second. A server that answers 429 quickly sends
  *more* responses per second while doing less work, so attempted requests per second is
  reported but never gated.
- **Failure shares are absolute.** The share of requests not accepted (backpressure, rejections
  and transport errors), for seeding and the mixed load, is budgeted in percentage points:
  `max(1 point, 3 × standard deviation × k(n))`. A relative budget would be undefined for a
  baseline with no failures, which is the normal case. The 1-point floor is a policy choice: the
  reference laptop rejected nothing in its runs, so there was no spread to measure.
- **Not gated** when there are fewer than 3 runs (no spread to estimate), the mean is zero, the
  percentile has too few samples, or the budget would exceed 50% (too noisy to tell a regression
  from a bad run with that many runs). The report says which, with the arithmetic.

Record a baseline you intend to compare against with `--runs 5` or more on a quiet machine (on AC
power, other heavy programs closed). Three runs is the minimum: it gates only the steadiest
metrics.

### Comparing two baselines

Compare a new run against an earlier report, or two existing reports:

```shell
dotnet run --project dotnet/Vyrtel.Baseline -c Release -- --runs 3 --compare target/baseline/reference.json
dotnet run --project dotnet/Vyrtel.Baseline -c Release -- compare old/report.json new/report.json
```

Each metric gets a verdict from the **baseline's** budget: *regression* (worse by more than the
budget), *improvement* (better by more than the budget), *within budget*, *not gated* or
*missing*. The comparison **passes** only when all of these hold:

- no metric regressed;
- no metric the baseline gated is missing from the current report (a mixed-load operation with
  no successful request has no latency, so its metrics go missing);
- at least one metric was gated (a baseline with fewer than 3 runs gates nothing);
- both reports measured the same thing: pinned configuration, workload, CPU, core count, OS,
  architecture, build profile and report schema. Otherwise a difference says nothing about the
  code. The number of runs may differ.

The report and console list every reason a comparison failed. Exit status is 0 when it passed,
2 when it did not, and 1 on an error, so a script can act on it.

### Checking a host with a same-commit comparison

Before trusting budgets on a new machine, record two baselines of the **same** commit in separate
sessions (for example, an hour apart) and compare them in both directions:

```shell
dotnet run --project dotnet/Vyrtel.Baseline -c Release -- --runs 5 --out target/baseline/a.json
# later
dotnet run --project dotnet/Vyrtel.Baseline -c Release -- --runs 3 --out target/baseline/b.json --compare target/baseline/a.json
dotnet run --project dotnet/Vyrtel.Baseline -c Release -- compare target/baseline/b.json target/baseline/a.json
```

Nothing changed, so any *regression* is noise the budgets did not cover. If one appears, look
at the samples CSV for the run that stands out: a run with fewer mixed-load requests than the
others means the whole machine was slower (another program, throttling) and that session should
be re-recorded. If it recurs on a quiet machine, the host drifts more than the floors allow:
compare against a baseline recorded in the same session as the candidate, or use quieter
hardware.

### Reference baseline

[`baselines/2026-10-15-windows-laptop.md`](benchmarks/baselines/2026-10-15-windows-laptop.md)
(raw data: [`.json`](benchmarks/baselines/2026-10-15-windows-laptop.json) and
[`.samples.csv`](benchmarks/baselines/2026-10-15-windows-laptop.samples.csv)) is a 5-run baseline
of 0.1.1 with the defaults above, on an Intel Core Ultra 7 155H laptop (22 logical cores, 32 GB,
NVMe SSD, Windows 11, on AC power with desktop applications open). The server was built from
`b61cd28`; the working tree differed only in this tool, its tests and docs, which the report
flags as uncommitted changes. It is evidence of what the tool reports and how noisy that machine
is, not a performance promise for other hardware.

These files are **report schema 1**, recorded before successful-throughput and failure-share
metrics, the dataset check and pass/fail comparisons were added. The current tool (schema 2)
refuses them rather than reinterpreting them: a schema-1 baseline gated attempted throughput,
which counts 429 responses as work. They remain the evidence for the budget rule below. Record
a schema-2 reference on your own machine before comparing (see
[Checking a host](#checking-a-host-with-a-same-commit-comparison)).

| What | Measured (mean of 5 runs, run-to-run CV) |
|---|---|
| Duration | 5.5 min (65–66 s per run) |
| Requests | seeding 1,201 per run, all accepted; mixed load 3,597–3,876 per run, all accepted, none rejected |
| Mixed load throughput | 221 requests/s (CV 2.7%) |
| Peak RSS | 54.4 MB (CV 2.8%), during the mixed load; 12.4 MB idle after start |
| Binary | 11.8 MB release build |
| Storage | zstd level 3: 4.01× compression, 7.6% index overhead, 73 bytes per event, 22 segments, 7.2 MB on disk |
| Selective query, warm | `traceId` (Bloom filter) 0.61 ms p50, skipping 20 of 22 segments and 108 blocks; `level = Error` 20.0 ms p50 (CV 1.8%) |
| Message scan, warm | `message contains "timed out"` 37.9 ms p50 (CV 2.4%), reading all 22 segments (177 blocks, 5.4 MB) |
| Cold ÷ warm (run 1) | about 1.0× for message scans (they read the same blocks either way), 1.9× for `level = Error`, 2.6–2.7× for `traceId` and `count` (index regions loaded on first use) |

How many metrics a 5-run baseline can gate on this laptop, by kind:

| Metric kind | Gated | Run-to-run CV |
|---|---:|---|
| Mixed-load p50 per operation | 13 of 14 | 3.3–10.2% |
| Warm p50 per query | 10 of 13 | 0.7–47.6% |
| Mixed-load p95 per operation | 5 of 14 | 4.9–16.8% |
| Cold query (one request per run) | 2 of 13 | 2.5–55% |
| Warm p95 (20 samples), mixed-load p99 (under 1,000 samples) | 0 of 27 | too few samples |
| Seeding p95, throughput, peak RSS, restart, sizes | 7 of 8 | 0–11.5% |

Cold timings are a single request each and vary too much between runs to gate here: they are
reported for review, and their diagnostics show where the cold cost comes from.

#### Run-to-run evidence

Six baselines of the same commit were recorded on the reference laptop in separate sessions.
Nothing changed between them, so every reported regression is a false alarm:

| Comparison | Budget rule | Metrics gated | Regressions reported |
|---|---|---:|---:|
| A (3 runs) ↔ B (3 runs) | first version: 3 × CV, 10% floor | 60, 61 | 1 each way (warm p95s, +11.6% and +15.7%) |
| C (5 runs) → D (3 runs, a disturbed session) | 3 × CV × k(n), 10% floor, any percentile | 34 | 1 (mixed-load `count` p99, +42.7%: one run's p99 was 60 ms against 30–33 ms) |
| A ↔ B, C ↔ D, C → A, C → B, from their stored runs | final rule | 13–28 | 0, except D → C: 1 at the then 10% floor (+11.2%) |
| E (5 runs, the reference) ↔ F (3 runs, [report](benchmarks/baselines/2026-10-15-windows-laptop-same-commit-check.md)) | final rule, recorded after it was fixed | 37, 31 | **0 each way**; largest gated change +8.5% against a 35% budget |

The first two rows are why the rule has k(n) and the tail-sample requirement, the third is where
the 15% timing floor comes from, and the last is the check on fresh sessions the rule was not
tuned on. TC-02's "identical workloads twice with variance" is rows E and F: both reports hold
every run's values, and the comparison tables show the differences.

Two later sessions on the same laptop (with schema 2) show what the gate does when the host
itself changes. With the battery at 18% and charging, mixed-load throughput fell from 221 to
about 100 successful requests/s and `level = Error` p50 rose from 20 ms to 100 ms on the same
commit. The tool gated only 17–18 of 93 metrics, because within-session spread was too large to
trust, and one comparison failed with four regressions of 39–104%. That is a real 2× slowdown of
the host, correctly reported; it is also why a reference must be recorded on a quiet machine on
AC power, and why a failure on a laptop should be confirmed by re-recording both reports.

The `baseline` job in `.github/workflows/benchmark.yml` records a 3-run baseline on each `main`
change and `v*` tag and uploads it as the `baseline-<ref>-<sha>` workflow artifact (kept 90
days), with the Markdown in the job summary. Shared runners are noisy, so their budgets describe
that hardware; compare two artifacts from the same runner type, not a runner against a laptop.

### When a run fails

| Message | Cause and fix |
|---|---|
| `A server is already listening at http://127.0.0.1:18480/` | Stop that server or pass `--port`. |
| `no executable was found` | `cargo build --release -p server`, or pass `--server <path>` / set `VYRTEL_BIN`. |
| `Vyrtel exited with code … before becoming healthy` | The server rejected the configuration or data directory; its output is printed above the message (`[vyrtel] …`). Check `--config`. |
| `seeding … failed with status 413` (or 400) | A seeding request was refused for a reason other than backpressure. With `--batch` raised, requests can exceed `ingest.max_request_size` (1 MB). |
| `seeding … failed with status 429 after 200 attempt(s)` | Backpressure never cleared for 40 s; the server is stuck or the host overloaded. |
| `the server did not finish writing the seeded data` | Queues did not drain within 120 s. |
| `query … failed with HTTP 500: … on warm request 7 of 20; … no baseline was recorded` (or `got no HTTP response`) | A cold or warm query was not answered successfully. The message names the query, the request, the status and the start of the response body. Like an incomplete dataset, this is a bug in the build under test. |
| `the seeded dataset is incomplete, so no baseline was recorded: …` | The server acknowledged or stored a different number of log events, spans or metric points than the fixture holds; the message names each signal and both counts. Treat it as a bug in the build under test, not a measurement problem. |
| `has report schema …` | The report was written by an incompatible version of the tool; re-record the baseline. |

The tool deletes each run's data directory (`%TEMP%` or `$TMPDIR`, named `vyrtel-baseline-*`)
unless `--keep-data` is given. If it is interrupted (Ctrl+C), a launched `vyrtel` process and
that directory can be left behind: stop the process and delete the directory.

### Limitations

- **One host at a time.** Budgets hold for the host and configuration in the report. There is no
  universal guarantee across hardware, and comparing reports from different hosts is not
  meaningful (such a comparison fails as incompatible).
- **Cold means a cold server process, not a cold disk.** The OS page cache is not dropped (that
  needs root and differs per OS), so the data files are usually still cached in memory.
- **Compaction is off**, unlike production defaults; its cost is not measured here.
- **Restart time** is measured by polling `/health` every 50 ms. On Windows a refused connection
  takes about 0.5 s to fail, so restart times there have a 0.5 s floor; treat them as
  comparable only on the same OS.
- **Event timestamps** follow the wall clock (the last 30 minutes before the run), so their
  absolute values differ between runs while everything else in the dataset is identical.
- **Peak RSS on macOS** comes from 100 ms sampling only, which can miss short spikes.
- **Disk type** (SSD or HDD, local or network) is not detected; put it in `--notes`.

## CI rollup

The rollup below is regenerated automatically on the CI machine whenever `main` changes.

`.github/workflows/benchmark.yml` runs the benchmark on every `main` change and commits the
rollup rendered below to the `benchmark-results` branch. Pushing a release tag (`v*`) captures a
separate snapshot under `versions/<tag>.json` on that branch; the **Version history** section charts those snapshots
so improvements between releases are visible. Use `--label <tag> --out docs/benchmarks/versions/<tag>.json`
to produce one locally.

<!--BENCHMARK_RESULTS-->
