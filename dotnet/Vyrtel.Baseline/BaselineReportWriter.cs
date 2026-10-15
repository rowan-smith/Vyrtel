using System.Globalization;
using System.Text;
using System.Text.Json;
using System.Text.Json.Serialization;
using Vyrtel.Harness;

namespace Vyrtel.Baseline;

/// <summary>
/// Writes a baseline as JSON (the comparable record), Markdown (for review) and CSV (every raw
/// latency sample, so nothing is lost to aggregation).
/// </summary>
public static class BaselineReportWriter
{
    public static readonly JsonSerializerOptions JsonOptions = new()
    {
        WriteIndented = true,
        PropertyNamingPolicy = JsonNamingPolicy.CamelCase,
        DefaultIgnoreCondition = JsonIgnoreCondition.WhenWritingNull,
        Converters = { new JsonStringEnumConverter(JsonNamingPolicy.CamelCase) },
    };

    private static readonly CultureInfo Inv = CultureInfo.InvariantCulture;

    public static BaselineReport Read(string path)
    {
        var report = JsonSerializer.Deserialize<BaselineReport>(File.ReadAllText(path), JsonOptions)
            ?? throw new InvalidDataException($"{path} is not a baseline report");
        return report.SchemaVersion == BaselineReport.CurrentSchemaVersion
            ? report
            : throw new InvalidDataException(
                $"{path} has report schema {report.SchemaVersion}; this tool reads schema {BaselineReport.CurrentSchemaVersion}");
    }

    /// <summary>Writes <paramref name="jsonPath"/> plus <c>.md</c> and <c>.samples.csv</c> beside it; returns all three paths.</summary>
    public static IReadOnlyList<string> Write(BaselineReport report, string jsonPath)
    {
        var json = Path.GetFullPath(jsonPath);
        Directory.CreateDirectory(Path.GetDirectoryName(json)!);
        var md = Path.ChangeExtension(json, ".md");
        var csv = Path.ChangeExtension(json, ".samples.csv");
        File.WriteAllText(json, JsonSerializer.Serialize(report, JsonOptions));
        File.WriteAllText(md, ToMarkdown(report, Path.GetFileName(json), Path.GetFileName(csv)));
        File.WriteAllText(csv, ToCsv(report.Runs.SelectMany(r => r.Samples)));
        return [json, md, csv];
    }

    public static string ToCsv(IEnumerable<RawSample> samples)
    {
        var sb = new StringBuilder("run,phase,operation,latency_ms,status\n");
        foreach (var s in samples)
        {
            sb.Append(Inv, $"{s.Run},{s.Phase},{s.Operation},{s.LatencyMs:0.###},{s.Status}\n");
        }
        return sb.ToString();
    }

    public static string ToMarkdown(BaselineReport r, string jsonName = "report.json", string csvName = "report.samples.csv")
    {
        var sb = new StringBuilder();
        void Line(string text = "") => sb.Append(text).Append('\n');

        Line("# Vyrtel performance baseline");
        Line();
        Line($"- Generated: {r.GeneratedAt.ToString("yyyy-MM-dd HH:mm:ss", Inv)} UTC");
        if (r.Label.Length > 0) Line($"- Label: {r.Label}");
        Line($"- Vyrtel: {r.VyrtelVersion}" + (r.Build.GitCommit is null ? "" : $" at `{r.Build.GitCommit[..Math.Min(12, r.Build.GitCommit.Length)]}`{(r.Build.GitDirty == true ? " (uncommitted changes)" : "")}"));
        Line($"- Duration: {Duration(r.TotalDurationSeconds)} for {r.Runs.Count} run(s) ({string.Join(", ", r.Runs.Select(x => Duration(x.DurationSeconds)))})");
        var w = r.Workload;
        Line(string.Create(Inv, $"- Workload: seed {w.Seed}; {w.Events:N0} log events, {w.Traces:N0} traces, {w.MetricPoints:N0} metric points in batches of {w.Batch}; ")
             + string.Create(Inv, $"each query once cold then {w.WarmIterations}× warm; mixed load {w.MixedDurationSeconds:0}s ({w.MixedWarmupSeconds:0}s warmup discarded) with {w.MixedConcurrency} workers"));
        Line();

        Line("## Host and build");
        Line();
        Line("| | |");
        Line("| --- | --- |");
        Line($"| CPU | {r.Host.CpuModel}, {r.Host.LogicalCores} logical cores |");
        Line($"| Memory | {ByteFormat.Format(r.Host.TotalMemoryBytes)} available to the harness |");
        Line($"| OS | {r.Host.Os} ({r.Host.Architecture}) |");
        Line($"| Disk | {(r.Host.Disk is { } d ? $"{d.Mount} {d.Format} ({d.Type}), {ByteFormat.Format(d.FreeBytes)} free of {ByteFormat.Format(d.TotalBytes)}" : "unknown")} |");
        Line($"| Harness runtime | {r.Host.Runtime} |");
        if (r.Host.Notes.Length > 0) Line($"| Notes | {r.Host.Notes} |");
        Line($"| Binary | {ByteFormat.Format(r.Build.ExecutableBytes)} ({r.Build.Profile}){(r.Build.ExecutableSha256 is { } sha ? $", SHA-256 `{sha[..16]}…`" : "")} |");
        if (r.Build.Rustc is not null) Line($"| Compiler | {r.Build.Rustc} |");
        Line($"| Configuration | `{r.Config.File}`, SHA-256 `{r.Config.Sha256[..16]}…` |");
        if (r.Config.Ingest is { } ingest)
        {
            Line($"| Ingest admission | {ingest.MaxConcurrent} concurrent ({ingest.ConcurrencySource}), {ByteFormat.Format(ingest.MaxRequestBytes)} max request |");
        }
        if (r.Compression is { } c)
        {
            Line(string.Create(Inv, $"| Compression | {c.Method}: {Num(c.Ratio, "0.00")}× ratio, {Num(c.IndexOverhead * 100, "0.0")}% index overhead, {c.BytesStoredPerEvent:0.0} bytes/event, {c.SegmentCount} segments, {ByteFormat.Format(c.DataDirBytes)} data directory |"));
        }
        Line();

        Line("## Memory");
        Line();
        Line("| Run | Peak RSS | Idle after start | Seeding peak | Query phase peak | Mixed load peak | Source |");
        Line("| ---: | ---: | ---: | ---: | ---: | ---: | --- |");
        foreach (var run in r.Runs)
        {
            var m = run.Memory;
            Line($"| {run.Run} | {ByteFormat.Format(m.PeakRssBytes)} | {ByteFormat.Format(m.IdleRssBytes)} | {ByteFormat.Format(m.SeedPeakRssBytes)} | {ByteFormat.Format(m.QueryPeakRssBytes)} | {ByteFormat.Format(m.MixedPeakRssBytes)} | {m.PeakRssSource} |");
        }
        Line();

        Line("## Accepted and rejected requests");
        Line();
        Line("Backpressure is 429/503 (retryable); seeding retries it, so the dataset always arrives complete.");
        Line();
        Line("| Run | Phase | Requests | Accepted | Backpressure | Too large (413) | Other rejected | Transport errors | Items accepted | Throughput | p50 | p95 | p99 |");
        Line("| ---: | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |");
        foreach (var run in r.Runs)
        {
            var s = run.Seed;
            var q = s.Requests;
            Line(string.Create(Inv, $"| {run.Run} | seeding | {q.Total:N0} | {q.Accepted:N0} | {q.RejectedBackpressure:N0} | {q.RejectedTooLarge:N0} | {q.RejectedOther:N0} | {q.TransportErrors:N0} | {s.ItemsAccepted:N0} | {s.ItemsPerSecond:N0} items/s | {Ms(s.Latency.P50)} | {Ms(s.Latency.P95)} | {Ms(s.Latency.P99)} |"));
            var m = run.Mixed;
            var mq = m.Requests;
            Line(string.Create(Inv, $"| {run.Run} | mixed | {mq.Total:N0} | {mq.Accepted:N0} | {mq.RejectedBackpressure:N0} | {mq.RejectedTooLarge:N0} | {mq.RejectedOther:N0} | {mq.TransportErrors:N0} | {m.EventsAccepted:N0} | {m.RequestsPerSecond:N0} req/s | | | |"));
        }
        Line();

        Line("## Mixed load by operation");
        Line();
        Line("Mean across runs; ± is the run-to-run coefficient of variation.");
        Line();
        Line("| Operation | Requests (run 1) | Rejected (all runs) | p50 | p95 | p99 |");
        Line("| --- | ---: | ---: | ---: | ---: | ---: |");
        var firstMixed = r.Runs.FirstOrDefault()?.Mixed.Operations ?? [];
        foreach (var op in firstMixed)
        {
            var all = r.Runs.Select(x => x.Mixed.Operations.FirstOrDefault(o => o.Name == op.Name)).Where(o => o is not null).ToList();
            var rejected = all.Sum(o => o!.Requests.Rejected + o.Requests.TransportErrors);
            Line(string.Create(Inv, $"| {op.Label} | {op.Requests.Total:N0} | {rejected:N0} | {Spread(all.Select(o => o!.Latency.P50))} | {Spread(all.Select(o => o!.Latency.P95))} | {Spread(all.Select(o => o!.Latency.P99))} |"));
        }
        Line();

        Line("## Queries: cold and warm");
        Line();
        Line("Cold is the first request after a server restart (no index cache, nothing decoded; the OS page cache is not dropped). Warm is the following repetitions. Diagnostics are the server's own, from run 1.");
        Line();
        Line("| Query | Kind | Cold | Warm p50 | Warm p95 | Cold ÷ warm | Segments (skipped/considered) | Blocks read (skipped) | Events examined → matched | Bytes read cold → warm | Indexes |");
        Line("| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | --- |");
        foreach (var q in r.Runs.FirstOrDefault()?.Queries ?? [])
        {
            var all = r.Runs.Select(x => x.Queries.FirstOrDefault(y => y.Name == q.Name)).Where(y => y is not null).ToList();
            var coldMean = all.Average(y => y!.ColdMs);
            var warmMean = all.Average(y => y!.Warm.P50);
            var cd = q.ColdDiagnostics;
            var wd = q.WarmDiagnostics;
            var ratio = warmMean > 0 ? string.Create(Inv, $"{coldMean / warmMean:0.0}×") : "–";
            var timings = $"| {q.Label} | {q.Category} | {Spread(all.Select(y => y!.ColdMs))} | {Spread(all.Select(y => y!.Warm.P50))} | {Spread(all.Select(y => y!.Warm.P95))} | {ratio} |";
            var diagnostics = cd is null
                ? " – | – | – | – | – |"
                : string.Create(Inv,
                    $" {cd.SegmentsSkipped}/{cd.SegmentsConsidered} | {cd.BlocksRead} ({cd.BlocksSkipped}) | {cd.EventsExamined:N0} → {cd.EventsMatched:N0} | {ByteFormat.Format(cd.BytesRead)} → {(wd is null ? "–" : ByteFormat.Format(wd.BytesRead))} | {(cd.Indexes.Count == 0 ? "none" : string.Join(", ", cd.Indexes))} |");
            Line(timings + diagnostics);
        }
        var failed = r.Runs.SelectMany(x => x.Queries).Sum(q => q.Failures);
        if (failed > 0)
        {
            Line();
            Line($"**{failed} query request(s) failed**; see `{csvName}` (status column).");
        }
        Line();

        Line("## Run-to-run variance and proposed regression budgets");
        Line();
        Line(string.Create(Inv, $"budget = max(floor, 3 × CV × k(n) rounded up), where k(n) turns the CV of n runs into a {BaselineStatistics.Confidence:P0} upper confidence bound; floor {BaselineStatistics.TimingFloorPercent:0}% for timings and memory, {BaselineStatistics.SizeFloorPercent:0}% for sizes; not gated with fewer than {BaselineStatistics.MinRunsForBudget} runs or a budget over {BaselineStatistics.MaxBudgetPercent:0}%. See docs/benchmarks.md#regression-budgets."));
        Line();
        Line("| Metric | Better | Runs | Mean | Std dev | CV | Min | Max | Budget | Justification |");
        Line("| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | --- |");
        foreach (var m in r.Summary)
        {
            Line(string.Create(Inv,
                $"| {m.Label} | {m.Better.ToString().ToLowerInvariant()} | {m.Runs} | {Value(m.Mean, m.Unit)} | {Value(m.StdDev, m.Unit)} | {Num(m.CvPercent, "0.0")}% | {Value(m.Min, m.Unit)} | {Value(m.Max, m.Unit)} | {(m.BudgetPercent is { } b ? $"{b:0}%" : "–")} | {m.Justification} |"));
        }
        Line();

        if (r.Comparison is { } cmp)
        {
            Line($"## Comparison with {(cmp.BaselineLabel.Length > 0 ? cmp.BaselineLabel : "baseline")} ({cmp.BaselineVersion}, {cmp.BaselineGeneratedAt.ToString("yyyy-MM-dd", Inv)})");
            Line();
            Line($"{cmp.Regressions} regression(s) beyond budget.");
            Line();
            foreach (var warning in cmp.Warnings)
            {
                Line($"- ⚠ {warning}");
            }
            if (cmp.Warnings.Count > 0) Line();
            Line(ComparisonTable(cmp));
        }

        Line("## Raw data");
        Line();
        Line($"- `{jsonName}`: every run's measurements, diagnostics and the effective server configuration.");
        Line($"- `{csvName}`: every request's latency and HTTP status.");
        return sb.ToString();
    }

    public static string ComparisonTable(ComparisonResult cmp)
    {
        var sb = new StringBuilder();
        sb.Append("| Metric | Baseline | Current | Worse by | Budget | Verdict |\n");
        sb.Append("| --- | ---: | ---: | ---: | ---: | --- |\n");
        foreach (var m in cmp.Metrics.OrderBy(m => m.Verdict == Verdict.Regression ? 0 : m.Verdict == Verdict.Improvement ? 1 : 2))
        {
            sb.Append(Inv,
                $"| {m.Label} | {Value(m.BaselineMean, m.Unit)} | {(m.CurrentMean is { } c ? Value(c, m.Unit) : "–")} | {(m.WorsePercent is { } w ? $"{w:+0.0;-0.0;0.0}%" : "–")} | {(m.BudgetPercent is { } b ? $"{b:0}%" : "–")} | {Label(m.Verdict)} |\n");
        }
        return sb.ToString();
    }

    public static string Label(Verdict v) => v switch
    {
        Verdict.Regression => "**regression**",
        Verdict.Improvement => "improvement",
        Verdict.WithinBudget => "within budget",
        Verdict.NotGated => "not gated",
        Verdict.Missing => "missing in current",
        _ => v.ToString(),
    };

    private static string Spread(IEnumerable<double> values)
    {
        var list = values.ToList();
        if (list.Count == 0) return "–";
        var mean = list.Average();
        if (list.Count < 2 || mean == 0) return Ms(mean);
        var sd = Math.Sqrt(list.Sum(v => (v - mean) * (v - mean)) / (list.Count - 1));
        return string.Create(Inv, $"{Ms(mean)} ±{sd / mean * 100:0}%");
    }

    private static string Value(double v, string unit) => unit switch
    {
        "ms" => Ms(v),
        "bytes" => ByteFormat.Format(v, v < 10240 ? 1 : 2),
        _ => string.Create(Inv, $"{v:N0} {unit}"),
    };

    private static string Ms(double ms) => ms.ToString(ms < 1 ? "0.000" : "0.00", Inv) + " ms";

    private static string Num(double? v, string format) => v?.ToString(format, Inv) ?? "?";

    private static string Duration(double seconds) =>
        seconds >= 90 ? string.Create(Inv, $"{seconds / 60:0.0} min") : string.Create(Inv, $"{seconds:0} s");
}
