using System.Text.Json;
using Vyrtel.Harness;

namespace Vyrtel.Baseline;

/// <summary>A query measured cold and warm. <see cref="Category"/> groups selective queries apart from scans.</summary>
public sealed record BaselineQuery(string Name, string Label, string Category, Func<ApiClient, Task<RawResponse>> Run);

/// <summary>One operation of the mixed load. Ingest operations report how many events were accepted.</summary>
public sealed record MixedOperation(string Name, string Label, int Weight, bool IsIngest, Func<ApiClient, Task<RawResponse>> Run);

/// <summary>Identifiers the queries look up, all produced by the seeded <see cref="EventFactory"/>.</summary>
public sealed record WorkloadContext(string LogTraceId, string SpanTraceId, string MetricName);

/// <summary>
/// The fixed workload. Query bodies match <see cref="ApiClient"/>'s, so these numbers line up
/// with the BenchmarkDotNet suite and the profiler; only the status is kept instead of throwing.
/// </summary>
public static class Workload
{
    public const string From = "-1h";
    public const int MixedIngestBatch = 800;

    public static IReadOnlyList<BaselineQuery> Queries(WorkloadContext c) =>
    [
        new("logs.level", "level = Error (bitmap)", "selective", api => Search(api, "level = Error", 200)),
        new("logs.service", "service = payments", "selective", api => Search(api, "service = payments", 200)),
        new("logs.traceId", "traceId = … (Bloom filter)", "selective", api => Search(api, $"traceId = \"{c.LogTraceId}\"", 200)),
        new("logs.range", "durationMs > 1990 (zone map)", "selective", api => Search(api, "durationMs > 1990", 200)),
        new("logs.count", "count: level = Error", "selective",
            api => api.PostJsonRaw("api/v1/query/logs/count", new { query = "level = Error", from = From })),
        new("logs.scan.timedOut", "message contains \"timed out\" (scan)", "scan", api => Search(api, "message contains \"timed out\"", 200)),
        new("logs.scan.error", "message contains \"error\" (scan)", "scan", api => Search(api, "message contains \"error\"", 200)),
        new("logs.latest", "latest logs (limit 200)", "recent", api => Search(api, "", 200)),
        new("logs.histogram", "histogram, 60 buckets", "aggregate",
            api => api.PostJsonRaw("api/v1/query/logs/histogram", new { query = "", from = From, buckets = 60 })),
        new("logs.facets", "facets, 2000-event sample", "aggregate",
            api => api.PostJsonRaw("api/v1/query/logs/facets", new { query = "", from = From, sample = 2000 })),
        new("traces.search", "trace search (limit 50)", "trace",
            api => api.PostJsonRaw("api/v1/query/traces", new { query = "", from = From, limit = 50 })),
        new("traces.get", "trace by id", "trace",
            api => api.SendRaw(HttpMethod.Get, $"api/v1/traces/{c.SpanTraceId}?from={Uri.EscapeDataString(From)}", null)),
        new("metrics.series", "metric series", "metric",
            api => api.PostJsonRaw("api/v1/query/metrics", new { name = c.MetricName, from = From })),
    ];

    /// <summary>
    /// The concurrent read/write mix, weighted like the profiler's. Payloads come from their own
    /// seed so they are identical in every run.
    /// </summary>
    public static IReadOnlyList<MixedOperation> Mixed(WorkloadContext c, int seed)
    {
        var ndjson = new EventFactory(seed: seed + 1).Ndjson(MixedIngestBatch);
        var json = new EventFactory(seed: seed + 2).Json(MixedIngestBatch);
        return
        [
            new("ingest.ndjson", $"ingest NDJSON ({MixedIngestBatch} events)", 8, true,
                api => api.IngestRaw("api/v1/events", ndjson, "application/x-ndjson")),
            new("ingest.json", $"ingest JSON array ({MixedIngestBatch} events)", 5, true,
                api => api.IngestRaw("api/v1/events", json, "application/json")),
            new("logs.latest", "latest logs (limit 200)", 18, false, api => Search(api, "", 200)),
            new("logs.count", "count: level = Error", 15, false,
                api => api.PostJsonRaw("api/v1/query/logs/count", new { query = "level = Error", from = From })),
            new("logs.level", "level = Error", 12, false, api => Search(api, "level = Error", 200)),
            new("logs.service", "service = payments", 10, false, api => Search(api, "service = payments", 200)),
            new("logs.traceId", "traceId (Bloom filter)", 10, false, api => Search(api, $"traceId = \"{c.LogTraceId}\"", 200)),
            new("logs.scan", "message contains (scan)", 5, false, api => Search(api, "message contains \"timed out\"", 200)),
            new("logs.histogram", "histogram, 60 buckets", 6, false,
                api => api.PostJsonRaw("api/v1/query/logs/histogram", new { query = "", from = From, buckets = 60 })),
            new("logs.facets", "facets", 5, false,
                api => api.PostJsonRaw("api/v1/query/logs/facets", new { query = "", from = From, sample = 2000 })),
            new("traces.search", "trace search (limit 50)", 10, false,
                api => api.PostJsonRaw("api/v1/query/traces", new { query = "", from = From, limit = 50 })),
            new("traces.get", "trace by id", 5, false,
                api => api.SendRaw(HttpMethod.Get, $"api/v1/traces/{c.SpanTraceId}?from={Uri.EscapeDataString(From)}", null)),
            new("metrics.names", "list metric names", 5, false, api => api.SendRaw(HttpMethod.Get, "api/v1/metrics", null)),
            new("metrics.series", "metric series", 5, false,
                api => api.PostJsonRaw("api/v1/query/metrics", new { name = c.MetricName, from = From })),
        ];
    }

    /// <summary>
    /// The operation sequence a worker follows. Seeded per worker, so every run issues the same
    /// requests in the same order; only how many fit in the duration varies.
    /// </summary>
    public static IEnumerable<MixedOperation> Sequence(IReadOnlyList<MixedOperation> ops, int seed, int worker)
    {
        // Not HashCode.Combine: it is randomised per process, and this must repeat across runs.
        var rng = new Random(unchecked(seed * 7919 + worker * 104_729 + 1));
        var total = ops.Sum(o => o.Weight);
        while (true)
        {
            var roll = rng.Next(total);
            var picked = ops[^1];
            foreach (var op in ops)
            {
                roll -= op.Weight;
                if (roll < 0)
                {
                    picked = op;
                    break;
                }
            }
            yield return picked;
        }
    }

    private static Task<RawResponse> Search(ApiClient api, string query, int limit) =>
        api.PostJsonRaw("api/v1/query/logs", new { query, from = From, limit, direction = "backward" });

    /// <summary>Reads the <c>diagnostics</c> object of a query response, or null when there is none.</summary>
    public static QueryDiagnostics? ParseDiagnostics(byte[] body)
    {
        try
        {
            using var doc = JsonDocument.Parse(body);
            if (doc.RootElement.ValueKind != JsonValueKind.Object
                || !doc.RootElement.TryGetProperty("diagnostics", out var d)
                || d.ValueKind != JsonValueKind.Object)
            {
                return null;
            }
            static double Num(JsonElement e, string name) =>
                e.TryGetProperty(name, out var v) && v.ValueKind == JsonValueKind.Number ? v.GetDouble() : 0;
            var indexes = new List<string>();
            if (d.TryGetProperty("indexes", out var ix) && ix.ValueKind == JsonValueKind.Array)
            {
                foreach (var i in ix.EnumerateArray())
                {
                    var field = i.TryGetProperty("field", out var f) ? f.GetString() : null;
                    var kind = i.TryGetProperty("kind", out var k) ? k.GetString() : null;
                    if (field is not null)
                    {
                        indexes.Add(kind is null ? field : $"{field}:{kind}");
                    }
                }
            }
            return new QueryDiagnostics(
                Num(d, "elapsedMs"),
                (long)Num(d, "segmentsConsidered"),
                (long)Num(d, "segmentsSkipped"),
                (long)Num(d, "blocksConsidered"),
                (long)Num(d, "blocksSkipped"),
                (long)Num(d, "blocksRead"),
                (long)Num(d, "eventsExamined"),
                (long)Num(d, "eventsMatched"),
                (long)Num(d, "bytesRead"),
                (long)Num(d, "unsealedEventsScanned"),
                indexes);
        }
        catch (JsonException)
        {
            return null;
        }
    }

    /// <summary>The <c>accepted</c> count of an ingest response (0 when absent).</summary>
    public static long ParseAccepted(byte[] body)
    {
        try
        {
            using var doc = JsonDocument.Parse(body);
            return doc.RootElement.ValueKind == JsonValueKind.Object
                   && doc.RootElement.TryGetProperty("accepted", out var a)
                   && a.TryGetInt64(out var n)
                ? n
                : 0;
        }
        catch (JsonException)
        {
            return 0;
        }
    }
}
