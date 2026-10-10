using Vyrtel.Harness;

namespace Vyrtel.Profiling;

/// <summary>One operation the profiler mixes into the load, with a relative weight.</summary>
public sealed record WorkloadOp(string Name, string Label, int Weight, Func<ApiClient, Task> Run);

/// <summary>The identifiers the workload queries, resolved from the seeded dataset.</summary>
public sealed record WorkloadContext(string LogTraceId, string SpanTraceId, string MetricName);

/// <summary>
/// The mixed workload: a realistic read/write mix so latency percentiles reflect actual
/// serving conditions (concurrent ingest + queries) rather than isolated calls.
/// </summary>
public static class WorkloadOperations
{
    private const int IngestBatchSize = 800;

    private static readonly byte[] NdjsonPayload = new EventFactory(seed: 1337).Ndjson(IngestBatchSize);
    private static readonly byte[] JsonPayload = new EventFactory(seed: 1337).Json(IngestBatchSize);

    public static IReadOnlyList<WorkloadOp> All(WorkloadContext context) =>
    [
        new("ingest.ndjson", $"Ingest NDJSON batch ({IngestBatchSize} events)", 8, api => api.IngestNdjson(NdjsonPayload)),
        new("ingest.json", $"Ingest JSON array ({IngestBatchSize} events)", 5, api => api.IngestJson(JsonPayload)),
        new("logs.latest", "Query: latest logs (limit 200)", 18, api => api.QueryLogs("", 200)),
        new("logs.count", "Count: level = Error", 15, api => api.CountLogs("level = Error")),
        new("logs.level", "Query: level = Error", 12, api => api.QueryLogs("level = Error", 200)),
        new("logs.service", "Query: service = payments", 10, api => api.QueryLogs("service = payments", 200)),
        new("logs.trace", "Query: traceId (bloom filter)", 10, api => api.QueryLogs($"traceId = \"{context.LogTraceId}\"", 200)),
        new("logs.scan", "Query: message contains (scan)", 5, api => api.QueryLogs("message contains \"timed out\"", 200)),
        new("logs.histogram", "Histogram: all logs (60 buckets)", 6, api => api.Histogram("logs", "", 60)),
        new("logs.facets", "Facets: sampled field values", 5, api => api.Facets("logs", "", 2000)),
        new("traces.search", "Trace search (limit 50)", 10, api => api.QueryTraces("", 50)),
        new("traces.get", "Trace by id (all spans)", 5, api => api.GetTrace(context.SpanTraceId)),
        new("metrics.names", "List metric names", 5, api => api.MetricNames()),
        new("metrics.series", "Metric series (avg over range)", 5, api => api.MetricQuery(context.MetricName)),
    ];
}
