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
if (options.Events > 0)
{
    Console.WriteLine($"seeding {options.Events:N0} log events...");
    await DatasetSeeder.SeedLogsAsync(api, factory, options.Events, options.Batch, Console.WriteLine);
    var traceCount = DatasetSeeder.DefaultTraceCount(options.Events);
    Console.WriteLine($"seeding {traceCount:N0} traces...");
    await DatasetSeeder.SeedTracesAsync(api, factory, traceCount, Console.WriteLine);
}

var version = await TryGetVersion(api);
var ops = WorkloadOperations.All(factory.SampleTraceId);

Console.WriteLine($"running {ops.Count} operations, {options.Concurrency} workers, {options.Duration:F0}s...");
var run = await WorkloadRunner.RunAsync(
    server.BaseUrl.ToString(),
    ops,
    options.Concurrency,
    TimeSpan.FromSeconds(options.Duration));

var hotspots = new List<FieldHotspot>();
try
{
    hotspots = ProfileReportWriter.ParseHotspots(await api.GetString("api/v1/system/query-stats"));
}
catch (Exception ex)
{
    Console.WriteLine($"warning: could not read query stats ({ex.Message})");
}

var operations = run.Operations
    .Select(r => new OperationResult(
        Name: r.Op.Name,
        Label: r.Op.Label,
        Count: r.Latency.Count,
        Failures: r.Failures,
        RequestsPerSecond: r.Latency.Count / Math.Max(run.DurationSeconds, 0.001),
        Latency: new LatencyStatsView(
            r.Latency.Min, r.Latency.Mean, r.Latency.Max, r.Latency.StdDev,
            r.Latency.P50, r.Latency.P90, r.Latency.P95, r.Latency.P99)))
    .ToList();

var report = ProfileReportWriter.Build(
    version,
    options.Events,
    run.DurationSeconds,
    run.Concurrency,
    run.TotalRequests,
    run.TotalFailures,
    operations,
    hotspots);

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