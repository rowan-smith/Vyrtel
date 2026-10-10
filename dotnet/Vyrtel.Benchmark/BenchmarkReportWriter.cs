using System.Globalization;
using System.Runtime.InteropServices;
using System.Text;
using System.Text.Json;
using System.Text.Json.Serialization;
using BenchmarkDotNet.Reports;
using BenchmarkDotNet.Running;

namespace Vyrtel.Benchmark;

/// <summary>Everything about the run that isn't in the BenchmarkDotNet summary itself.</summary>
public sealed record RunContext(
    string BaseUrl,
    string VyrtelVersion,
    string Job,
    int DatasetEvents,
    bool Launched,
    string? Executable);

public sealed record BenchmarkReport(
    DateTimeOffset GeneratedAt,
    string VyrtelVersion,
    string Job,
    string Server,
    string ServerUrl,
    BenchmarkDataset Dataset,
    HardwareInfo Hardware,
    IReadOnlyList<BenchmarkResult> Benchmarks);

public sealed record BenchmarkDataset(int Events);

public sealed record HardwareInfo(string Os, string Architecture, int LogicalCores, string Runtime, string BenchmarkDotNet);

public sealed record BenchmarkResult(
    string Id,
    string Group,
    string Name,
    string Metric,
    double MeanMs,
    double MedianMs,
    double MinMs,
    double MaxMs,
    double StdDevMs,
    double Value,
    string Unit,
    double? Throughput,
    string? ThroughputUnit);

/// <summary>
/// Turns BenchmarkDotNet summaries into the JSON the website renders and a Markdown copy
/// that is easy to skim in a pull request or the GitHub blob view.
/// </summary>
public static class BenchmarkReportWriter
{
    private static readonly JsonSerializerOptions JsonOptions = new()
    {
        WriteIndented = true,
        PropertyNamingPolicy = JsonNamingPolicy.CamelCase,
        DefaultIgnoreCondition = JsonIgnoreCondition.WhenWritingNull,
    };

    public static void Write(IReadOnlyList<Summary> summaries, RunContext context, string jsonPath)
    {
        var results = new List<BenchmarkResult>();
        foreach (var summary in summaries)
        {
            foreach (var report in summary.Reports)
            {
                var stats = report.ResultStatistics;
                if (stats is null)
                {
                    continue;
                }

                var method = report.BenchmarkCase.Descriptor.WorkloadMethod.Name;
                var meta = BenchmarkCatalog.ByMethod.TryGetValue(method, out var m)
                    ? m
                    : new BenchmarkMetadata("Other", method, BenchmarkMetric.Latency);

                var meanMs = stats.Mean / 1_000_000.0;
                var throughput = meta.Metric == BenchmarkMetric.Throughput
                    ? meta.OperationsPerInvoke * 1000.0 / meanMs
                    : 1000.0 / meanMs;
                var unit = meta.Metric == BenchmarkMetric.Throughput ? "events/s" : "ops/s";
                var value = meta.Metric == BenchmarkMetric.Throughput ? throughput : meanMs;

                results.Add(new BenchmarkResult(
                    Id: method,
                    Group: meta.Group,
                    Name: meta.Display,
                    Metric: meta.Metric == BenchmarkMetric.Throughput ? "throughput" : "latency",
                    MeanMs: Round(meanMs),
                    MedianMs: Round(stats.Median / 1_000_000.0),
                    MinMs: Round(stats.Min / 1_000_000.0),
                    MaxMs: Round(stats.Max / 1_000_000.0),
                    StdDevMs: Round(stats.StandardDeviation / 1_000_000.0),
                    Value: Round(value),
                    Unit: unit,
                    Throughput: meta.Metric == BenchmarkMetric.Latency ? Round(throughput) : null,
                    ThroughputUnit: meta.Metric == BenchmarkMetric.Latency ? unit : null));
            }
        }

        var document = new BenchmarkReport(
            GeneratedAt: DateTimeOffset.UtcNow,
            VyrtelVersion: context.VyrtelVersion,
            Job: context.Job,
            Server: context.Launched ? "launched" : "external",
            ServerUrl: context.BaseUrl,
            Dataset: new BenchmarkDataset(context.DatasetEvents),
            Hardware: new HardwareInfo(
                Os: RuntimeInformation.OSDescription,
                Architecture: RuntimeInformation.OSArchitecture.ToString(),
                LogicalCores: Environment.ProcessorCount,
                Runtime: RuntimeInformation.FrameworkDescription,
                BenchmarkDotNet: typeof(BenchmarkRunner).Assembly.GetName().Version?.ToString() ?? "unknown"),
            Benchmarks: results.OrderBy(r => r.Group).ThenBy(r => r.Name).ToList());

        var full = Path.GetFullPath(jsonPath);
        Directory.CreateDirectory(Path.GetDirectoryName(full)!);
        File.WriteAllText(full, JsonSerializer.Serialize(document, JsonOptions));
        File.WriteAllText(Path.ChangeExtension(full, ".md"), ToMarkdown(document));
    }

    private static double Round(double value) => Math.Round(value, 4, MidpointRounding.AwayFromZero);

    private static string ToMarkdown(BenchmarkReport report)
    {
        var sb = new StringBuilder();
        sb.AppendLine("# Vyrtel benchmarks");
        sb.AppendLine();
        sb.AppendLine($"- Generated: {report.GeneratedAt:yyyy-MM-dd HH:mm:ss} UTC");
        sb.AppendLine($"- Vyrtel: {report.VyrtelVersion}");
        sb.AppendLine($"- BenchmarkDotNet job: {report.Job}");
        sb.AppendLine($"- Dataset: {report.Dataset.Events:N0} seeded log events");
        sb.AppendLine($"- Machine: {report.Hardware.Os} ({report.Hardware.Architecture}), {report.Hardware.LogicalCores} logical cores");
        sb.AppendLine($"- Runtime: {report.Hardware.Runtime}");
        sb.AppendLine();

        foreach (var group in report.Benchmarks.GroupBy(b => b.Group))
        {
            sb.AppendLine($"## {group.Key}");
            sb.AppendLine();
            sb.AppendLine("| Benchmark | Mean | Median | Min | Max | StdDev | Throughput |");
            sb.AppendLine("| --- | ---: | ---: | ---: | ---: | ---: | ---: |");
            foreach (var b in group)
            {
                var throughput = b.Metric == "throughput" ? $"{b.Value:N0} {b.Unit}" : $"{b.Throughput:N0} {b.ThroughputUnit}";
                sb.AppendLine(
                    $"| {b.Name} | {Fmt(b.MeanMs)} ms | {Fmt(b.MedianMs)} ms | {Fmt(b.MinMs)} ms | {Fmt(b.MaxMs)} ms | {Fmt(b.StdDevMs)} ms | {throughput} |");
            }
            sb.AppendLine();
        }

        return sb.ToString();
    }

    private static string Fmt(double value) => value.ToString(value < 1 ? "0.000" : "0.00", CultureInfo.InvariantCulture);
}
