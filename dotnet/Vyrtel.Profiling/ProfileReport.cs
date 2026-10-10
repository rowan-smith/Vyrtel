using System.Globalization;
using System.Runtime.InteropServices;
using System.Text;
using System.Text.Json;
using System.Text.Json.Serialization;

namespace Vyrtel.Profiling;

public sealed record ProfilingReport(
    DateTimeOffset GeneratedAt,
    string VyrtelVersion,
    int DatasetEvents,
    double DurationSeconds,
    double WarmupSeconds,
    double MeasurementSeconds,
    int Concurrency,
    long TotalRequests,
    long TotalFailures,
    double RequestsPerSecond,
    string Machine,
    IReadOnlyList<OperationResult> Operations,
    IReadOnlyList<FieldHotspot> Hotspots);

public sealed record OperationResult(
    string Name,
    string Label,
    long Count,
    long Failures,
    double RequestsPerSecond,
    LatencyStatsView Latency);

public sealed record LatencyStatsView(
    double Min,
    double Mean,
    double Max,
    double StdDev,
    double P50,
    double P90,
    double P95,
    double P99);

/// <summary>A field the server actually scanned/indexed, ranked by cumulative query time.</summary>
public sealed record FieldHotspot(
    string Signal,
    string Field,
    string Index,
    long Queries,
    long BytesRead,
    long SegmentsScanned,
    long SegmentsSkipped,
    double TotalMs,
    double AvgMs,
    double SkippedRatio);

public static class ProfileReportWriter
{
    private static readonly JsonSerializerOptions JsonOptions = new()
    {
        WriteIndented = true,
        PropertyNamingPolicy = JsonNamingPolicy.CamelCase,
        DefaultIgnoreCondition = JsonIgnoreCondition.WhenWritingNull,
    };

    public static ProfilingReport Build(
        string version,
        int datasetEvents,
        double durationSeconds,
        double warmupSeconds,
        double measurementSeconds,
        int concurrency,
        long totalRequests,
        long totalFailures,
        IReadOnlyList<OperationResult> operations,
        IReadOnlyList<FieldHotspot> hotspots) =>
        new(
            DateTimeOffset.UtcNow,
            version,
            datasetEvents,
            durationSeconds,
            warmupSeconds,
            measurementSeconds,
            concurrency,
            totalRequests,
            totalFailures,
            totalRequests / Math.Max(measurementSeconds, 0.001),
            $"{RuntimeInformation.OSDescription} ({RuntimeInformation.OSArchitecture}), {Environment.ProcessorCount} logical cores",
            operations,
            hotspots);

    public static void Print(ProfilingReport report)
    {
        Console.WriteLine();
        Console.WriteLine($"profile: {report.TotalRequests:N0} requests in {report.MeasurementSeconds:F0}s measured " +
                          $"(~{report.RequestsPerSecond:N0}/s), {report.TotalFailures:N0} failures");
        Console.WriteLine();
        Console.WriteLine($"  {"Operation",-42} {"count",7} {"p50",8} {"p90",8} {"p95",8} {"p99",8}");
        foreach (var op in report.Operations.OrderByDescending(o => o.Latency.P99))
        {
            var l = op.Latency;
            Console.WriteLine(
                $"  {op.Label,-42} {op.Count,7:N0} {Fmt(l.P50),8} {Fmt(l.P90),8} {Fmt(l.P95),8} {Fmt(l.P99),8}");
        }

        if (report.Hotspots.Count > 0)
        {
            Console.WriteLine();
            Console.WriteLine("  top index hotspots by query time:");
            foreach (var h in report.Hotspots.Take(5))
            {
                Console.WriteLine(
                    $"  {h.Field,-28} {h.Signal,-7} {h.Index,-8} {h.Queries,7:N0} queries, {Fmt(h.AvgMs),8} ms avg, {h.SkippedRatio:P0} blocks skipped");
            }
        }
        Console.WriteLine();
    }

    /// <summary>Parses <c>GET /api/v1/system/query-stats</c> into fields ranked by total query time.</summary>
    public static List<FieldHotspot> ParseHotspots(string queryStatsJson) =>
        ParseRows(queryStatsJson).OrderByDescending(h => h.TotalMs).Take(20).ToList();

    /// <summary>Parses the raw per-field rows, without ranking or trimming.</summary>
    public static List<FieldHotspot> ParseRows(string queryStatsJson)
    {
        using var doc = JsonDocument.Parse(queryStatsJson);
        if (!doc.RootElement.TryGetProperty("fields", out var fields) || fields.ValueKind != JsonValueKind.Array)
        {
            return [];
        }

        var result = new List<FieldHotspot>();
        foreach (var row in fields.EnumerateArray())
        {
            static double Number(JsonElement e, string name) =>
                e.TryGetProperty(name, out var v) && v.ValueKind == JsonValueKind.Number ? v.GetDouble() : 0;
            static long Int(JsonElement e, string name) => (long)Number(e, name);

            var queries = Int(row, "queries");
            var totalMs = Number(row, "totalMs");
            var scanned = Int(row, "segmentsScanned");
            var skipped = Int(row, "segmentsSkipped");
            result.Add(new FieldHotspot(
                Signal: row.TryGetProperty("signal", out var s) ? s.GetString() ?? "" : "",
                Field: row.TryGetProperty("field", out var f) ? f.GetString() ?? "" : "",
                Index: row.TryGetProperty("index", out var i) && i.ValueKind == JsonValueKind.String ? i.GetString() ?? "" : "",
                Queries: queries,
                BytesRead: Int(row, "bytesRead"),
                SegmentsScanned: scanned,
                SegmentsSkipped: skipped,
                TotalMs: totalMs,
                AvgMs: queries > 0 ? totalMs / queries : 0,
                SkippedRatio: scanned + skipped > 0 ? skipped / (double)(scanned + skipped) : 0));
        }

        return result;
    }

    /// <summary>
    /// Differences two query-stats snapshots so hotspots cover only the profiling window
    /// (the seeding phase and any warmup queries are subtracted out).
    /// </summary>
    public static List<FieldHotspot> DiffHotspots(string beforeJson, string afterJson)
    {
        var before = ParseRows(beforeJson).ToDictionary(Key);
        var rows = new List<FieldHotspot>();
        foreach (var after in ParseRows(afterJson))
        {
            before.TryGetValue(Key(after), out var b);
            var queries = Math.Max(0, after.Queries - (b?.Queries ?? 0));
            var scanned = Math.Max(0, after.SegmentsScanned - (b?.SegmentsScanned ?? 0));
            var skipped = Math.Max(0, after.SegmentsSkipped - (b?.SegmentsSkipped ?? 0));
            var totalMs = Math.Max(0, after.TotalMs - (b?.TotalMs ?? 0));
            rows.Add(after with
            {
                Queries = queries,
                BytesRead = Math.Max(0, after.BytesRead - (b?.BytesRead ?? 0)),
                SegmentsScanned = scanned,
                SegmentsSkipped = skipped,
                TotalMs = totalMs,
                AvgMs = queries > 0 ? totalMs / queries : 0,
                SkippedRatio = scanned + skipped > 0 ? skipped / (double)(scanned + skipped) : 0,
            });
        }
        return rows.OrderByDescending(h => h.TotalMs).Take(20).ToList();
    }

    private static string Key(FieldHotspot h) => $"{h.Signal}\u0000{h.Field}\u0000{h.Index}";

    public static void Write(ProfilingReport report, string jsonPath)
    {
        var full = Path.GetFullPath(jsonPath);
        Directory.CreateDirectory(Path.GetDirectoryName(full)!);
        File.WriteAllText(full, JsonSerializer.Serialize(report, JsonOptions));
        File.WriteAllText(Path.ChangeExtension(full, ".md"), ToMarkdown(report));
    }

    private static string ToMarkdown(ProfilingReport report)
    {
        var sb = new StringBuilder();
        sb.AppendLine("# Vyrtel profile");
        sb.AppendLine();
        sb.AppendLine($"- Generated: {report.GeneratedAt:yyyy-MM-dd HH:mm:ss} UTC");
        sb.AppendLine($"- Vyrtel: {report.VyrtelVersion}");
        sb.AppendLine($"- Dataset: {report.DatasetEvents:N0} seeded log events");
        sb.AppendLine($"- Load: {report.DurationSeconds:F0}s total ({report.WarmupSeconds:F0}s warmup + {report.MeasurementSeconds:F0}s measured), "
                      + $"{report.Concurrency} concurrent workers, {report.TotalRequests:N0} requests (~{report.RequestsPerSecond:N0}/s), {report.TotalFailures:N0} failures");
        sb.AppendLine($"- Machine: {report.Machine}");
        sb.AppendLine();

        sb.AppendLine("## Latency by operation");
        sb.AppendLine();
        sb.AppendLine("| Operation | Count | RPS | p50 | p90 | p95 | p99 | Mean | Max | Failures |");
        sb.AppendLine("| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |");
        foreach (var op in report.Operations.OrderByDescending(o => o.Latency.P99))
        {
            var l = op.Latency;
            sb.AppendLine(
                $"| {op.Label} | {op.Count:N0} | {op.RequestsPerSecond:N0} | {Fmt(l.P50)} | {Fmt(l.P90)} | {Fmt(l.P95)} | {Fmt(l.P99)} | {Fmt(l.Mean)} | {Fmt(l.Max)} | {op.Failures:N0} |");
        }
        sb.AppendLine();

        if (report.Hotspots.Count > 0)
        {
            sb.AppendLine("## Index hotspots by query time");
            sb.AppendLine();
            sb.AppendLine("| Field | Signal | Index | Queries | Avg ms | Bytes read | Scanned | Skipped | Skipped % |");
            sb.AppendLine("| --- | --- | --- | ---: | ---: | ---: | ---: | ---: | ---: |");
            foreach (var h in report.Hotspots)
            {
                sb.AppendLine(
                    $"| `{h.Field}` | {h.Signal} | {h.Index} | {h.Queries:N0} | {Fmt(h.AvgMs)} | {Bytes(h.BytesRead)} | {h.SegmentsScanned:N0} | {h.SegmentsSkipped:N0} | {h.SkippedRatio:P0} |");
            }
            sb.AppendLine();
        }

        return sb.ToString();
    }

    private static string Fmt(double ms) => ms.ToString(ms < 1 ? "0.000" : "0.00", CultureInfo.InvariantCulture);

    private static string Bytes(long b)
    {
        string[] units = ["B", "KB", "MB", "GB"];
        var v = (double)b;
        var u = 0;
        while (v >= 1024 && u < units.Length - 1)
        {
            v /= 1024;
            u++;
        }
        return $"{v:N1} {units[u]}";
    }
}