using System.Text.Json;

namespace Vyrtel.Baseline;

/// <summary>
/// One field's share of the server's query statistics (<c>GET /api/v1/system/query-stats</c>)
/// over the measured mixed-load window, ranked by query time.
/// </summary>
public sealed record FieldHotspot(
    string Signal,
    string Field,
    string Index,
    long Queries,
    long BytesRead,
    long SegmentsScanned,
    long SegmentsSkipped,
    double TotalMs)
{
    public double AvgMs => Queries > 0 ? TotalMs / Queries : 0;

    /// <summary>Share of the segments considered that the index let the query skip.</summary>
    public double SkippedRatio => SegmentsScanned + SegmentsSkipped > 0
        ? SegmentsSkipped / (double)(SegmentsScanned + SegmentsSkipped)
        : 0;
}

public static class Hotspots
{
    /// <summary>Rows kept in a report.</summary>
    public const int Top = 20;

    /// <summary>The per-field rows of a query-stats response, as reported (cumulative).</summary>
    public static List<FieldHotspot> Parse(string queryStatsJson)
    {
        using var doc = JsonDocument.Parse(queryStatsJson);
        if (!doc.RootElement.TryGetProperty("fields", out var fields) || fields.ValueKind != JsonValueKind.Array)
        {
            return [];
        }
        static double Number(JsonElement e, string name) =>
            e.TryGetProperty(name, out var v) && v.ValueKind == JsonValueKind.Number ? v.GetDouble() : 0;
        static string Text(JsonElement e, string name) =>
            e.TryGetProperty(name, out var v) && v.ValueKind == JsonValueKind.String ? v.GetString() ?? "" : "";
        return fields.EnumerateArray()
            .Select(row => new FieldHotspot(
                Text(row, "signal"),
                Text(row, "field"),
                Text(row, "index"),
                (long)Number(row, "queries"),
                (long)Number(row, "bytesRead"),
                (long)Number(row, "segmentsScanned"),
                (long)Number(row, "segmentsSkipped"),
                Number(row, "totalMs")))
            .ToList();
    }

    /// <summary>
    /// What the queries between two snapshots added, per field, ranked by query time: the
    /// statistics are cumulative since the server started, so without the difference seeding,
    /// the cold/warm phase and the warmup would count too. Fields with no queries in the window
    /// are left out.
    /// </summary>
    public static List<FieldHotspot> Diff(string beforeJson, string afterJson)
    {
        var before = Parse(beforeJson).ToDictionary(h => (h.Signal, h.Field));
        return Parse(afterJson)
            .Select(after => before.TryGetValue((after.Signal, after.Field), out var b)
                ? after with
                {
                    Queries = Math.Max(0, after.Queries - b.Queries),
                    BytesRead = Math.Max(0, after.BytesRead - b.BytesRead),
                    SegmentsScanned = Math.Max(0, after.SegmentsScanned - b.SegmentsScanned),
                    SegmentsSkipped = Math.Max(0, after.SegmentsSkipped - b.SegmentsSkipped),
                    TotalMs = Math.Max(0, after.TotalMs - b.TotalMs),
                }
                : after)
            .Where(h => h.Queries > 0)
            .OrderByDescending(h => h.TotalMs)
            .ThenBy(h => h.Field, StringComparer.Ordinal)
            .Take(Top)
            .ToList();
    }
}
