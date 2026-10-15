using System.Diagnostics;
using System.Text.Json;
using Vyrtel.Baseline;
using Vyrtel.Harness;


// Every failure (an unreadable or incompatible report, an incomplete dataset, a server that will
// not start) is one line on stderr and exit status 1, never a stack trace that looks like a crash.
AppDomain.CurrentDomain.UnhandledException += (_, e) =>
{
    Console.Error.WriteLine($"vyrtel-baseline: {(e.ExceptionObject as Exception)?.Message ?? e.ExceptionObject}");
    Environment.Exit(1);
};

BaselineOptions options;
try
{
    options = BaselineOptions.Parse(args);
}
catch (Exception ex) when (ex is ArgumentException or FormatException or OverflowException)
{
    Console.Error.WriteLine($"vyrtel-baseline: {ex.Message}");
    return 1;
}

if (options.ShowHelp)
{
    BaselineOptions.PrintHelp();
    return 0;
}

if (options.CompareOnly is var (baselinePath, currentPath))
{
    var comparison = BaselineComparer.Compare(BaselineReportWriter.Read(baselinePath), BaselineReportWriter.Read(currentPath));
    PrintComparison(comparison);
    return BaselineComparer.ExitCode(comparison);
}

// Read the earlier report first so a bad path fails before minutes of measuring.
var previous = options.CompareWith is null ? null : BaselineReportWriter.Read(options.CompareWith);
var configPath = options.ResolveConfigPath();
var runner = new BaselineRunner(options, configPath);
Console.WriteLine(
    $"vyrtel-baseline: {options.Runs} run(s), seed {options.Seed}, {options.Events:N0} events, config {configPath}, {runner.BaseUrl}");

var clock = Stopwatch.StartNew();
var runs = new List<RunResult>();
ServerFacts? facts = null;
for (var run = 1; run <= options.Runs; run++)
{
    var (result, serverFacts) = await runner.RunAsync(run);
    runs.Add(result);
    facts ??= serverFacts;
}

var repoRoot = VyrtelServer.FindRepositoryRoot();
var build = HostProbe.Build(facts!.Profile, facts.Executable, repoRoot);
var firstStorage = runs[0].Storage;
var report = new BaselineReport(
    BaselineReport.CurrentSchemaVersion,
    DateTimeOffset.UtcNow,
    options.Label,
    facts.Version,
    new WorkloadSpec(
        options.Seed,
        options.Events,
        BaselineRunner.TraceCount(options.Events),
        BaselineRunner.MetricPointCount(options.Events),
        options.Batch,
        options.Runs,
        options.WarmIterations,
        options.Duration,
        options.Warmup,
        options.Concurrency),
    new PinnedConfig(
        repoRoot is not null && configPath.StartsWith(repoRoot, StringComparison.OrdinalIgnoreCase)
            ? Path.GetRelativePath(repoRoot, configPath).Replace('\\', '/')
            : configPath,
        HostProbe.Sha256OfText(configPath),
        facts.EffectiveConfig,
        facts.Ingest),
    HostProbe.Describe(Path.GetTempPath(), options.Notes),
    build,
    Compression(facts.EffectiveConfig, firstStorage, runs[0].DataDirBytes),
    clock.Elapsed.TotalSeconds,
    runs,
    BaselineStatistics.Summarize(runs, build.ExecutableBytes));

if (previous is not null)
{
    report = report with { Comparison = BaselineComparer.Compare(previous, report) };
}

foreach (var path in BaselineReportWriter.Write(report, options.Output))
{
    Console.WriteLine($"wrote {path}");
}

PrintSummary(report);
if (report.Comparison is { } cmp)
{
    PrintComparison(cmp);
    return BaselineComparer.ExitCode(cmp);
}
return 0;

static CompressionInfo? Compression(JsonElement? config, StorageProfile? storage, long dataDirBytes)
{
    int? level = null;
    if (config is { } c
        && c.TryGetProperty("storage", out var s)
        && s.TryGetProperty("zstd_level", out var z)
        && z.TryGetInt32(out var l))
    {
        level = l;
    }
    if (storage is null && level is null)
    {
        return null;
    }
    return new CompressionInfo(
        level is null ? "zstd, independently compressed column chunks" : $"zstd level {level}, independently compressed column chunks",
        level,
        storage?.CompressionRatio,
        storage?.IndexOverhead,
        storage?.BytesStoredPerEvent ?? 0,
        storage?.SegmentCount ?? 0,
        dataDirBytes);
}

static void PrintSummary(BaselineReport report)
{
    Console.WriteLine();
    Console.WriteLine($"baseline: {report.Runs.Count} run(s) in {report.TotalDurationSeconds:0}s on {report.Host.CpuModel} ({report.Host.LogicalCores} cores)");
    Console.WriteLine($"  binary {ByteFormat.Format(report.Build.ExecutableBytes)}, peak RSS {ByteFormat.Format(report.Runs.Max(r => r.Memory.PeakRssBytes))}, {report.Compression?.Method}");
    var gated = report.Summary.Count(m => m.BudgetPercent is not null);
    Console.WriteLine($"  {report.Summary.Count} metrics, {gated} with a proposed budget");
    if (report.Runs.Count < BaselineStatistics.MinRunsForBudget)
    {
        Console.WriteLine($"  no budgets: run at least {BaselineStatistics.MinRunsForBudget} times (--runs) to measure run-to-run variance");
        return;
    }
    foreach (var m in report.Summary.Where(m => m.BudgetPercent is null))
    {
        Console.WriteLine($"  not gated: {m.Label}: {m.Justification}");
    }
}

static void PrintComparison(ComparisonResult cmp)
{
    Console.WriteLine();
    var name = cmp.BaselineLabel.Length > 0 ? cmp.BaselineLabel : "baseline";
    Console.WriteLine($"comparison with {name} ({cmp.BaselineVersion}): {(cmp.Passed ? "PASSED" : "FAILED")}, {cmp.Gated} gated metric(s)");
    foreach (var reason in cmp.FailureReasons)
    {
        Console.WriteLine($"  {reason}");
    }
    foreach (var m in cmp.Metrics.Where(m => m.Verdict is Verdict.Regression or Verdict.Improvement || m is { Verdict: Verdict.Missing, BudgetPercent: not null }))
    {
        var change = m.WorsePercent is { } w ? BaselineReportWriter.Change(w, m.Unit) : "–";
        Console.WriteLine($"  {BaselineReportWriter.Label(m.Verdict).Trim('*'),-18} {m.Label}: {change} worse (budget {BaselineReportWriter.Budget(m.BudgetPercent, m.Unit)})");
    }
}
