using System.Diagnostics;
using System.Text;
using System.Text.Json;
using Vyrtel.Harness;

namespace Vyrtel.Baseline;

/// <summary>What the first run learns about the server, reported once per baseline.</summary>
public sealed record ServerFacts(string Version, JsonElement? EffectiveConfig, IngestLimits? Ingest, string? Executable, string Profile);

/// <summary>
/// Executes one baseline run on a fresh data directory:
/// <list type="number">
/// <item>seed the fixed dataset serially, counting accepted and rejected requests;</item>
/// <item>for each query, restart the server (process-cold: no index cache, no decoded blocks),
/// time the first request, then repeat it warm;</item>
/// <item>run the concurrent ingest + query mix and record per-operation outcomes and percentiles.</item>
/// </list>
/// The operating system's page cache is not dropped, so "cold" means a cold server process on
/// a warm OS; see docs/benchmarks.md.
/// </summary>
public sealed class BaselineRunner(BaselineOptions options, string configPath)
{
    private static readonly TimeSpan QuiescenceTimeout = TimeSpan.FromSeconds(120);
    private static readonly TimeSpan BackpressureDelay = TimeSpan.FromMilliseconds(200);
    private const int MaxSeedAttempts = 200;

    public Uri BaseUrl { get; } = new($"http://127.0.0.1:{options.Port}");

    public static int TraceCount(int events) => DatasetSeeder.DefaultTraceCount(events);
    public static int MetricPointCount(int events) => DatasetSeeder.DefaultMetricCount(events);

    public async Task<(RunResult Run, ServerFacts Facts)> RunAsync(int run, CancellationToken ct = default)
    {
        var dataDir = Path.Combine(Path.GetTempPath(), $"vyrtel-baseline-{run}-{Guid.NewGuid():N}");
        var samples = new List<RawSample>();
        var started = Stopwatch.StartNew();
        try
        {
            // 1. Seed.
            var factory = new EventFactory(seed: options.Seed);
            IngestPhase seed;
            ServerFacts facts;
            StorageProfile? storage;
            long dataDirBytes, idleRss;
            (long Bytes, string Source) seedPeak;
            await using (var server = await LaunchAsync(dataDir, ct))
            {
                await using var rss = new RssMonitor(server.Process!);
                idleRss = rss.Current;
                using var api = new ApiClient(BaseUrl.ToString());
                Console.WriteLine($"run {run}: seeding {options.Events:N0} logs, {TraceCount(options.Events):N0} traces, {MetricPointCount(options.Events):N0} metric points");
                seed = await SeedAsync(api, factory, run, samples, ct);
                var stored = await WaitForQuiescenceAsync(api, seed.Expected, ct);
                seed = seed with { Stored = stored };
                if (DatasetCheck.Verify(seed.Expected, seed.Acknowledged, stored) is { } problem)
                {
                    throw new InvalidOperationException($"run {run}: {problem}");
                }
                Console.WriteLine($"run {run}: dataset verified: {stored}");
                storage = await api.GetStorageStatsAsync(ct);
                dataDirBytes = server.MeasureDataDirBytes();
                facts = await ReadFactsAsync(api, server, ct);
                seedPeak = await rss.StopAsync();
            }

            var context = new WorkloadContext(factory.SampleTraceId, factory.SampleTraceTargetId, EventFactory.DefaultMetricName);

            // 2. Cold and warm queries, one server restart per query.
            var queries = new List<QueryMeasurement>();
            (long Bytes, string Source) queryPeak = (0, RssMonitor.SampledSource);
            foreach (var query in Workload.Queries(context))
            {
                await using var server = await LaunchAsync(dataDir, ct);
                await using var rss = new RssMonitor(server.Process!);
                var measurement = await MeasureQueryAsync(query, server.StartupTime, run, samples, ct);
                queries.Add(measurement);
                var peak = await rss.StopAsync();
                if (peak.PeakBytes > queryPeak.Bytes) queryPeak = peak;
                Console.WriteLine(
                    $"run {run}: {query.Label,-42} cold {Ms(measurement.ColdMs),9}  warm p50 {Ms(measurement.Warm.P50),9}  p95 {Ms(measurement.Warm.P95),9}  restart {Ms(measurement.RestartMs)}");
            }

            // 3. Concurrent ingest + queries.
            MixedPhase mixed;
            (long Bytes, string Source) mixedPeak;
            await using (var server = await LaunchAsync(dataDir, ct))
            {
                await using var rss = new RssMonitor(server.Process!);
                mixed = await RunMixedAsync(context, run, samples, ct);
                mixedPeak = await rss.StopAsync();
            }
            Console.WriteLine(
                $"run {run}: mixed load {mixed.Requests.Total:N0} requests ({mixed.RequestsPerSecond:N0}/s), " +
                $"{mixed.Requests.Accepted:N0} accepted, {mixed.Requests.Rejected:N0} rejected, {mixed.Requests.TransportErrors:N0} transport errors");

            var overall = new[] { seedPeak, queryPeak, mixedPeak }.MaxBy(p => p.Bytes);
            var memory = new MemoryFacts(
                overall.Bytes, overall.Source, idleRss, seedPeak.Bytes, queryPeak.Bytes, mixedPeak.Bytes);
            var result = new RunResult(
                run,
                started.Elapsed.TotalSeconds,
                seed,
                queries,
                mixed,
                memory,
                storage,
                dataDirBytes,
                queries.Count > 0 ? queries.Average(q => q.RestartMs) : 0)
            {
                Samples = samples,
            };
            return (result, facts);
        }
        finally
        {
            if (!options.KeepData)
            {
                TryDelete(dataDir);
            }
            else
            {
                Console.WriteLine($"run {run}: kept data directory {dataDir}");
            }
        }
    }

    private Task<VyrtelServer> LaunchAsync(string dataDir, CancellationToken ct) =>
        VyrtelServer.StartOrConnectAsync(new VyrtelServerOptions
        {
            BaseUrl = BaseUrl,
            ExecutablePath = options.Server,
            DataDir = dataDir,
            LogLevel = options.LogLevel,
            AllowExisting = false,
            KeepData = true, // the run deletes it after every phase is done
            ExtraArguments = ["--config", configPath],
        }, ct);

    private async Task<IngestPhase> SeedAsync(ApiClient api, EventFactory factory, int run, List<RawSample> samples, CancellationToken ct)
    {
        var tally = new RequestTally();
        var latency = new LatencyRecorder();
        long eventsSent = 0, eventsAccepted = 0;
        var expected = SignalCounts.Zero;
        var acknowledged = SignalCounts.Zero;
        var clock = Stopwatch.StartNew();

        // `items` is what the fixture contains, counted from the payload itself, never from the
        // server's reply: a server that loses events must not lower its own bar.
        async Task Send(string phase, string path, byte[] payload, string mediaType, SignalCounts items)
        {
            var events = items.Logs + items.Traces + items.Metrics;
            eventsSent += events;
            expected = expected.Add(items);
            for (var attempt = 1; ; attempt++)
            {
                var sw = Stopwatch.StartNew();
                var response = await QueryMeasurer.TrySend(() => api.IngestRaw(path, payload, mediaType, ct));
                var ms = sw.Elapsed.TotalMilliseconds;
                tally.Add(response.Status);
                samples.Add(new RawSample(run, phase, path, ms, response.Status));
                switch (RequestTally.Classify(response.Status))
                {
                    case Outcome.Accepted:
                        latency.Record(ms);
                        // Only native ingest reports a count; OTLP replies carry none.
                        var acked = items.Logs > 0 ? items with { Logs = Workload.ParseAccepted(response.Body) } : items;
                        acknowledged = acknowledged.Add(acked);
                        eventsAccepted += acked.Logs + acked.Traces + acked.Metrics;
                        return;
                    case Outcome.Backpressure or Outcome.TransportError when attempt < MaxSeedAttempts:
                        // The dataset must arrive complete, so retry as a well-behaved client would.
                        await Task.Delay(BackpressureDelay, ct);
                        continue;
                    default:
                        throw new InvalidOperationException(
                            $"seeding {path} failed with status {response.Status} after {attempt} attempt(s): {Encoding.UTF8.GetString(response.Body)}");
                }
            }
        }

        for (var sent = 0; sent < options.Events;)
        {
            var count = Math.Min(options.Batch, options.Events - sent);
            await Send("seed.logs", "api/v1/events", factory.Ndjson(count), "application/x-ndjson", new SignalCounts(count, 0, 0));
            sent += count;
        }
        for (var i = 0; i < TraceCount(options.Events); i++)
        {
            var trace = factory.TracePayload();
            await Send("seed.traces", "v1/traces", trace, "application/json", new SignalCounts(0, DatasetCheck.CountItems(trace, "spans"), 0));
        }
        var points = MetricPointCount(options.Events);
        for (var sent = 0; sent < points;)
        {
            var count = Math.Min(options.Batch, points - sent);
            var metrics = factory.OtlpMetrics(count);
            await Send("seed.metrics", "v1/metrics", metrics, "application/json", new SignalCounts(0, 0, DatasetCheck.CountItems(metrics, "dataPoints")));
            sent += count;
        }

        var seconds = clock.Elapsed.TotalSeconds;
        return new IngestPhase(seconds, eventsSent, eventsAccepted, tally.Snapshot(), eventsAccepted / Math.Max(seconds, 0.001), latency.Snapshot())
        {
            Expected = expected,
            Acknowledged = acknowledged,
        };
    }

    /// <summary>
    /// Waits until every queue is empty and the per-signal counts either match the fixture or
    /// have stopped changing, and returns what is stored. The caller decides whether it is complete.
    /// </summary>
    private static async Task<SignalCounts> WaitForQuiescenceAsync(ApiClient api, SignalCounts expected, CancellationToken ct)
    {
        var deadline = DateTime.UtcNow + QuiescenceTimeout;
        SignalCounts? previous = null;
        while (DateTime.UtcNow < deadline)
        {
            using var doc = JsonDocument.Parse(await api.GetString("api/v1/system/storage", ct));
            var queued = doc.RootElement.GetProperty("signals").EnumerateObject().Sum(s => s.Value.GetProperty("queueDepth").GetInt64());
            var stored = DatasetCheck.ParseStored(await api.GetString("api/v1/system/info", ct));
            if (queued == 0 && (stored == expected || stored == previous))
            {
                return stored;
            }
            previous = stored;
            await Task.Delay(250, ct);
        }
        throw new TimeoutException($"the server did not finish writing the seeded data within {QuiescenceTimeout}");
    }

    private static async Task<ServerFacts> ReadFactsAsync(ApiClient api, VyrtelServer server, CancellationToken ct)
    {
        var version = "unknown";
        IngestLimits? ingest = null;
        using (var info = JsonDocument.Parse(await api.GetString("api/v1/system/info", ct)))
        {
            var root = info.RootElement;
            version = root.TryGetProperty("version", out var v) ? v.GetString() ?? version : version;
            if (root.TryGetProperty("ingest", out var i))
            {
                ingest = new IngestLimits(
                    i.GetProperty("maxConcurrent").GetInt64(),
                    i.GetProperty("concurrencySource").GetString() ?? "",
                    i.GetProperty("maxRequestBytes").GetInt64());
            }
        }
        using var config = JsonDocument.Parse(await api.GetString("api/v1/system/config", ct));
        var effective = config.RootElement.TryGetProperty("config", out var c) ? c.Clone() : (JsonElement?)null;
        return new ServerFacts(version, effective, ingest, server.ExecutablePath, server.BuildProfile);
    }

    private async Task<QueryMeasurement> MeasureQueryAsync(
        BaselineQuery query, TimeSpan restart, int run, List<RawSample> samples, CancellationToken ct)
    {
        using var api = new ApiClient(BaseUrl.ToString());
        // Open the connection first, so the cold timing is the query and not the TCP handshake.
        await api.GetString("health", ct);
        return await QueryMeasurer.MeasureAsync(query, () => query.Run(api), options.WarmIterations, restart.TotalMilliseconds, run, samples);
    }

    private async Task<MixedPhase> RunMixedAsync(WorkloadContext context, int run, List<RawSample> samples, CancellationToken ct)
    {
        var ops = Workload.Mixed(context, options.Seed);
        var tallies = ops.ToDictionary(o => o.Name, _ => new RequestTally());
        var recorders = ops.ToDictionary(o => o.Name, _ => new LatencyRecorder());
        long eventsAccepted = 0;
        var workerSamples = new List<RawSample>[options.Concurrency];

        var start = DateTime.UtcNow;
        var recordFrom = start + TimeSpan.FromSeconds(options.Warmup);
        var deadline = start + TimeSpan.FromSeconds(options.Duration);

        // Query statistics when the measured window opens, so the hotspots cover it alone.
        using var statsApi = new ApiClient(BaseUrl.ToString());
        var statsBefore = Task.Run(async () =>
        {
            var wait = recordFrom - DateTime.UtcNow;
            if (wait > TimeSpan.Zero)
            {
                await Task.Delay(wait, ct);
            }
            return await TryQueryStatsAsync(statsApi, run, ct);
        }, ct);

        var workers = Enumerable.Range(0, options.Concurrency).Select(async worker =>
        {
            var mine = workerSamples[worker] = [];
            using var api = new ApiClient(BaseUrl.ToString());
            using var sequence = Workload.Sequence(ops, options.Seed, worker).GetEnumerator();
            while (DateTime.UtcNow < deadline && !ct.IsCancellationRequested && sequence.MoveNext())
            {
                var op = sequence.Current;
                var sw = Stopwatch.StartNew();
                var response = await QueryMeasurer.TrySend(() => op.Run(api));
                var ms = sw.Elapsed.TotalMilliseconds;
                if (DateTime.UtcNow < recordFrom)
                {
                    continue;
                }
                tallies[op.Name].Add(response.Status);
                mine.Add(new RawSample(run, "mixed", op.Name, ms, response.Status));
                if (response.IsSuccess)
                {
                    recorders[op.Name].Record(ms);
                    if (op.IsIngest)
                    {
                        Interlocked.Add(ref eventsAccepted, Workload.ParseAccepted(response.Body));
                    }
                }
            }
        });
        await Task.WhenAll(workers);
        foreach (var list in workerSamples)
        {
            samples.AddRange(list ?? []);
        }
        var before = await statsBefore;
        var after = await TryQueryStatsAsync(statsApi, run, ct);
        var hotspots = before is not null && after is not null ? Hotspots.Diff(before, after) : null;

        var measured = options.Duration - options.Warmup;
        var operations = ops
            .Select(o => new MixedOperationResult(o.Name, o.Label, tallies[o.Name].Snapshot(), recorders[o.Name].Snapshot()))
            .ToList();
        var requests = RequestTally.Sum(operations.Select(o => o.Requests));
        return new MixedPhase(measured, options.Concurrency, requests.Total / measured, eventsAccepted, requests, operations, hotspots);
    }

    /// <summary>
    /// A query-stats snapshot, or null with a warning: hotspots are diagnostics, so a missing
    /// snapshot leaves them out of the report instead of failing the run.
    /// </summary>
    private static async Task<string?> TryQueryStatsAsync(ApiClient api, int run, CancellationToken ct)
    {
        try
        {
            return await api.GetString("api/v1/system/query-stats", ct);
        }
        catch (HttpRequestException e)
        {
            Console.WriteLine($"run {run}: no index hotspots (query statistics unavailable: {e.Message})");
            return null;
        }
    }

    private static void TryDelete(string dir)
    {
        for (var attempt = 0; attempt < 5; attempt++)
        {
            try
            {
                if (Directory.Exists(dir)) Directory.Delete(dir, recursive: true);
                return;
            }
            catch (IOException)
            {
                Thread.Sleep(200); // a just-killed server can hold handles briefly on Windows
            }
            catch (UnauthorizedAccessException)
            {
                Thread.Sleep(200);
            }
        }
    }

    private static string Ms(double ms) => ms < 1 ? $"{ms:0.000} ms" : $"{ms:0.00} ms";
}
