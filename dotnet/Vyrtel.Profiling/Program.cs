using System.Text.Json;
using Vyrtel.Harness;
using Vyrtel.Profiling;

var options = ProfileCliOptions.Parse(args);
if (options.ShowHelp)
{
    ProfileCliOptions.PrintHelp();
    return 0;
}

Console.WriteLine(
    $"vyrtel-profile: url={options.Url} events={options.Events:N0} duration={options.Duration:F0}s concurrency={options.Concurrency}");

var serverOptions = new VyrtelServerOptions
{
    BaseUrl = new Uri(options.Url),
    ExecutablePath = options.Server,
    DataDir = options.DataDir,
    LogLevel = options.LogLevel,
    AllowLaunch = !options.NoLaunch,
    KeepData = options.KeepData,
};

await using var server = await VyrtelServer.StartOrConnectAsync(serverOptions);
using var api = new ApiClient(server.BaseUrl.ToString());

var factory = new EventFactory(seed: 42);
string metricName = EventFactory.DefaultMetricName;
if (options.Events > 0)
{
    Console.WriteLine($"seeding {options.Events:N0} log events...");
    await DatasetSeeder.SeedLogsAsync(api, factory, options.Events, options.Batch, Console.WriteLine);
    var traceCount = DatasetSeeder.DefaultTraceCount(options.Events);
    Console.WriteLine($"seeding {traceCount:N0} traces...");
    await DatasetSeeder.SeedTracesAsync(api, factory, traceCount, Console.WriteLine);
    var metricCount = DatasetSeeder.DefaultMetricCount(options.Events);
    Console.WriteLine($"seeding {metricCount:N0} metric points...");
    await DatasetSeeder.SeedMetricsAsync(api, factory, metricCount, options.Batch, Console.WriteLine);
    metricName = await TryGetFirstMetricName(api) ?? EventFactory.DefaultMetricName;
}
else
{
    Console.WriteLine("skipping seeding (--events 0); assuming the server already has data");
}

var version = await TryGetVersion(api);

// Snapshot the build and the on-disk footprint before the load test starts, so the numbers
// describe the seeded dataset rather than the traffic generated below.
var build = server.MeasureBuild();
var storage = await api.GetStorageStatsAsync();
Console.WriteLine($"executable: {build.ExecutableSize} ({build.Profile}) · data dir: {build.DataDirSize}");
if (storage is null)
{
    Console.WriteLine("warning: could not read /api/v1/system/storage (older server build?)");
}
else
{
    Console.WriteLine($"storage: {storage.EventCount:N0} events, {storage.BytesStoredPerEvent:F2} bytes/event, {storage.SegmentCount} segments");
}

var context = new WorkloadContext(factory.SampleTraceId, factory.SampleTraceTargetId, metricName);
var ops = WorkloadOperations.All(context);

var statsBefore = await TryGetQueryStats(api);
Console.WriteLine(
    $"running {ops.Count} operations, {options.Concurrency} workers, {options.Duration:F0}s (warmup {options.Warmup:F0}s)...");
var run = await WorkloadRunner.RunAsync(
    server.BaseUrl.ToString(),
    ops,
    options.Concurrency,
    TimeSpan.FromSeconds(options.Duration),
    TimeSpan.FromSeconds(options.Warmup));
var statsAfter = await TryGetQueryStats(api);

List<FieldHotspot> hotspots = statsAfter is null
    ? []
    : statsBefore is null
        ? ProfileReportWriter.ParseHotspots(statsAfter)
        : ProfileReportWriter.DiffHotspots(statsBefore, statsAfter);

var operations = run.Operations
    .Select(r => new OperationResult(
        Name: r.Op.Name,
        Label: r.Op.Label,
        Count: r.Latency.Count,
        Failures: r.Failures,
        RequestsPerSecond: r.Latency.Count / Math.Max(run.MeasurementSeconds, 0.001),
        Latency: new LatencyStatsView(
            r.Latency.Min, r.Latency.Mean, r.Latency.Max, r.Latency.StdDev,
            r.Latency.P50, r.Latency.P90, r.Latency.P95, r.Latency.P99)))
    .ToList();

var report = ProfileReportWriter.Build(
    version,
    options.Events,
    run.DurationSeconds,
    run.WarmupSeconds,
    run.MeasurementSeconds,
    run.Concurrency,
    run.TotalRequests,
    run.TotalFailures,
    operations,
    hotspots,
    build,
    storage);

ProfileReportWriter.Print(report);

var output = Path.GetFullPath(options.Output);
ProfileReportWriter.Write(report, output);
Console.WriteLine($"wrote {output}");
Console.WriteLine($"wrote {Path.ChangeExtension(output, ".md")}");
return 0;

static async Task<string> TryGetVersion(ApiClient api)
{
    try
    {
        using var doc = JsonDocument.Parse(await api.GetString("api/v1/system/info"));
        return doc.RootElement.TryGetProperty("version", out var v) ? v.GetString() ?? "unknown" : "unknown";
    }
    catch (Exception ex)
    {
        Console.WriteLine($"warning: could not read server version ({ex.Message})");
        return "unknown";
    }
}

static async Task<string?> TryGetQueryStats(ApiClient api)
{
    try
    {
        return await api.GetString("api/v1/system/query-stats");
    }
    catch (Exception ex)
    {
        Console.WriteLine($"warning: could not read query stats ({ex.Message})");
        return null;
    }
}

static async Task<string?> TryGetFirstMetricName(ApiClient api)
{
    try
    {
        using var doc = JsonDocument.Parse(await api.GetString("api/v1/metrics"));
        if (doc.RootElement.TryGetProperty("metrics", out var metrics) && metrics.ValueKind == JsonValueKind.Array)
        {
            foreach (var metric in metrics.EnumerateArray())
            {
                if (metric.TryGetProperty("name", out var name) && name.ValueKind == JsonValueKind.String)
                {
                    return name.GetString();
                }
            }
        }
    }
    catch (Exception ex)
    {
        Console.WriteLine($"warning: could not read metric names ({ex.Message})");
    }
    return null;
}