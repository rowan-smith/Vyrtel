using System.Text;
using System.Text.Json;
using BenchmarkDotNet.Reports;
using BenchmarkDotNet.Running;
using Vyrtel.Benchmark;
using Vyrtel.Harness;

var options = CliOptions.Parse(args);
if (options.ShowHelp)
{
    CliOptions.PrintHelp();
    return 0;
}
if (options.List)
{
    CliOptions.PrintList();
    return 0;
}

Console.WriteLine($"vyrtel-benchmark: job={options.Job} url={options.Url} events={options.Events:N0} batch={options.Batch}");

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

// Snapshot the build and the on-disk footprint now, before the benchmark run, so the numbers
// describe the seeded dataset rather than the traffic the benchmarks themselves generate.
var build = server.MeasureBuild();
var storage = await api.GetStorageStatsAsync();
if (storage is null)
{
    Console.WriteLine("warning: could not read /api/v1/system/storage (older server build?)");
}
else
{
    Console.WriteLine($"storage: {storage.EventCount:N0} events, {storage.BytesStoredPerEvent:F2} bytes/event, {storage.SegmentCount} segments");
}
Console.WriteLine($"executable: {build.ExecutableSize} ({build.Profile}) · data dir: {build.DataDirSize}");

Environment.SetEnvironmentVariable("VYRTEL_URL", server.BaseUrl.ToString().TrimEnd('/'));
Environment.SetEnvironmentVariable("VYRTEL_SAMPLE_TRACE_ID", factory.SampleTraceId);
Environment.SetEnvironmentVariable("VYRTEL_SAMPLE_TRACE_TARGET_ID", factory.SampleTraceTargetId);
Environment.SetEnvironmentVariable("VYRTEL_METRIC_NAME", metricName);

var summaries = new List<Summary>();
foreach (var type in new[] { typeof(QueryBenchmarks), typeof(IngestBenchmarks) })
{
    var artifacts = Path.Combine(Path.GetTempPath(), "vyrtel-bdn", type.Name);
    summaries.Add(BenchmarkRunner.Run(type, BenchmarkConfig.Create(options.Job, artifacts)));
}

var context = new RunContext(
    server.BaseUrl.ToString(),
    version,
    options.Job,
    options.Events,
    server.Launched,
    server.ExecutablePath,
    options.Label,
    build,
    storage);

var output = Path.GetFullPath(options.Output);
BenchmarkReportWriter.Write(summaries, context, output);
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