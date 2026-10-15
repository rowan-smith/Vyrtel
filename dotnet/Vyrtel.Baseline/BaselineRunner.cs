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
                await WaitForQuiescenceAsync(api, ct);
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
        var clock = Stopwatch.StartNew();

        async Task Send(string phase, string path, byte[] payload, string mediaType, int events)
        {
            eventsSent += events;
            for (var attempt = 1; ; attempt++)
            {
                var sw = Stopwatch.StartNew();
                var response = await TrySend(() => api.IngestRaw(path, payload, mediaType, ct));
                var ms = sw.Elapsed.TotalMilliseconds;
                tally.Add(response.Status);
                samples.Add(new RawSample(run, phase, path, ms, response.Status));
                switch (RequestTally.Classify(response.Status))
                {
                    case Outcome.Accepted:
                        latency.Record(ms);
                        eventsAccepted += path == "api/v1/events" ? Workload.ParseAccepted(response.Body) : events;
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
            await Send("seed.logs", "api/v1/events", factory.Ndjson(count), "application/x-ndjson", count);
            sent += count;
        }
        for (var i = 0; i < TraceCount(options.Events); i++)
        {
            await Send("seed.traces", "v1/traces", factory.TracePayload(), "application/json", 1);
        }
        var points = MetricPointCount(options.Events);
        for (var sent = 0; sent < points;)
        {
            var count = Math.Min(options.Batch, points - sent);
            await Send("seed.metrics", "v1/metrics", factory.OtlpMetrics(count), "application/json", count);
            sent += count;
        }

        var seconds = clock.Elapsed.TotalSeconds;
        return new IngestPhase(seconds, eventsSent, eventsAccepted, tally.Snapshot(), eventsAccepted / Math.Max(seconds, 0.001), latency.Snapshot());
    }

    /// <summary>Waits until every queue is empty and the event count has stopped changing.</summary>
    private static async Task WaitForQuiescenceAsync(ApiClient api, CancellationToken ct)
    {
        var deadline = DateTime.UtcNow + QuiescenceTimeout;
        long previous = -1;
        while (DateTime.UtcNow < deadline)
        {
            using var doc = JsonDocument.Parse(await api.GetString("api/v1/system/storage", ct));
            var root = doc.RootElement;
            var events = root.GetProperty("eventCount").GetInt64();
            var queued = root.GetProperty("signals").EnumerateObject().Sum(s => s.Value.GetProperty("queueDepth").GetInt64());
            if (queued == 0 && events == previous)
            {
                return;
            }
            previous = events;
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

        var failures = 0;
        var sw = Stopwatch.StartNew();
        var cold = await TrySend(() => query.Run(api));
        var coldMs = sw.Elapsed.TotalMilliseconds;
        samples.Add(new RawSample(run, "query.cold", query.Name, coldMs, cold.Status));
        if (!cold.IsSuccess) failures++;

        var warm = new LatencyRecorder();
        RawResponse? last = null;
        for (var i = 0; i < options.WarmIterations; i++)
        {
            sw.Restart();
            var response = await TrySend(() => query.Run(api));
            var ms = sw.Elapsed.TotalMilliseconds;
            samples.Add(new RawSample(run, "query.warm", query.Name, ms, response.Status));
            if (response.IsSuccess)
            {
                warm.Record(ms);
                last = response;
            }
            else
            {
                failures++;
            }
        }

        return new QueryMeasurement(
            query.Name,
            query.Label,
            query.Category,
            coldMs,
            cold.IsSuccess ? Workload.ParseDiagnostics(cold.Body) : null,
            warm.Snapshot(),
            last is null ? null : Workload.ParseDiagnostics(last.Body),
            restart.TotalMilliseconds,
            failures);
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

        var workers = Enumerable.Range(0, options.Concurrency).Select(async worker =>
        {
            var mine = workerSamples[worker] = [];
            using var api = new ApiClient(BaseUrl.ToString());
            using var sequence = Workload.Sequence(ops, options.Seed, worker).GetEnumerator();
            while (DateTime.UtcNow < deadline && !ct.IsCancellationRequested && sequence.MoveNext())
            {
                var op = sequence.Current;
                var sw = Stopwatch.StartNew();
                var response = await TrySend(() => op.Run(api));
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

        var measured = options.Duration - options.Warmup;
        var operations = ops
            .Select(o => new MixedOperationResult(o.Name, o.Label, tallies[o.Name].Snapshot(), recorders[o.Name].Snapshot()))
            .ToList();
        var requests = RequestTally.Sum(operations.Select(o => o.Requests));
        return new MixedPhase(measured, options.Concurrency, requests.Total / measured, eventsAccepted, requests, operations);
    }

    /// <summary>Runs a request; a transport failure becomes status 0 instead of an exception.</summary>
    private static async Task<RawResponse> TrySend(Func<Task<RawResponse>> send)
    {
        try
        {
            return await send();
        }
        catch (Exception ex) when (ex is HttpRequestException or TaskCanceledException or IOException)
        {
            return new RawResponse(RequestTally.TransportErrorStatus, Encoding.UTF8.GetBytes(ex.Message));
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
