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
if (options.Events > 0)
{
    Console.WriteLine($"seeding {options.Events:N0} log events...");
    await DatasetSeeder.SeedLogsAsync(api, factory, options.Events, options.Batch, Console.WriteLine);
    var traceCount = DatasetSeeder.DefaultTraceCount(options.Events);
    Console.WriteLine($"seeding {traceCount:N0} traces...");
    await DatasetSeeder.SeedTracesAsync(api, factory, traceCount, Console.WriteLine);
}
else
{
    Console.WriteLine("skipping seeding (--events 0); assuming the server already has data");
}

var version = await TryGetVersion(api);

Environment.SetEnvironmentVariable("VYRTEL_URL", server.BaseUrl.ToString().TrimEnd('/'));
Environment.SetEnvironmentVariable("VYRTEL_SAMPLE_TRACE_ID", factory.SampleTraceId);

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
    server.ExecutablePath);

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