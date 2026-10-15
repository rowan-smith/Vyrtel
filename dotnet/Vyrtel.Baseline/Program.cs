using System.Diagnostics;
using System.Text.Json;
using Vyrtel.Baseline;
using Vyrtel.Harness;

const int ExitRegression = 2;

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
    return comparison.Regressions > 0 ? ExitRegression : 0;
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
    return cmp.Regressions > 0 ? ExitRegression : 0;
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
    Console.WriteLine($"comparison with {(cmp.BaselineLabel.Length > 0 ? cmp.BaselineLabel : "baseline")} ({cmp.BaselineVersion}): {cmp.Regressions} regression(s)");
    foreach (var warning in cmp.Warnings)
    {
        Console.WriteLine($"  warning: {warning}");
    }
    foreach (var m in cmp.Metrics.Where(m => m.Verdict is Verdict.Regression or Verdict.Improvement or Verdict.Missing))
    {
        Console.WriteLine($"  {BaselineReportWriter.Label(m.Verdict).Trim('*'),-18} {m.Label}: {m.WorsePercent:+0.0;-0.0;0.0}% worse (budget {m.BudgetPercent:0}%)");
    }
}
