using Vyrtel.Harness;

namespace Vyrtel.Profiling;

/// <summary>One operation the profiler mixes into the load, with a relative weight.</summary>
public sealed record WorkloadOp(string Name, string Label, int Weight, Func<ApiClient, string, Task> Run);

/// <summary>
/// The mixed workload: a realistic read/write mix so latency percentiles reflect actual
/// serving conditions (concurrent ingest + queries) rather than isolated calls.
/// </summary>
public static class WorkloadOperations
{
    private const int IngestBatchSize = 800;

    private static readonly byte[] IngestPayload = new EventFactory(seed: 1337).Ndjson(IngestBatchSize);

    public static IReadOnlyList<WorkloadOp> All(string sampleTraceId) =>
    [
        new("ingest", $"Ingest NDJSON batch ({IngestBatchSize} events)", 8, Ingest),
        new("logs.latest", "Query: latest logs (limit 200)", 18, (api, _) => api.QueryLogs("", 200)),
        new("logs.count", "Count: level = Error", 15, (api, _) => api.CountLogs("level = Error")),
        new("logs.level", "Query: level = Error", 12, (api, _) => api.QueryLogs("level = Error", 200)),
        new("logs.service", "Query: service = payments", 10, (api, _) => api.QueryLogs("service = payments", 200)),
        new("logs.trace", "Query: traceId (bloom filter)", 10, (api, _) => api.QueryLogs($"traceId = \"{sampleTraceId}\"", 200)),
        new("logs.scan", "Query: message contains (scan)", 5, (api, _) => api.QueryLogs("message contains \"timed out\"", 200)),
        new("traces.search", "Trace search (limit 50)", 10, (api, _) => api.QueryTraces("", 50)),
        new("metrics.names", "List metric names", 5, (api, _) => api.MetricNames()),
    ];

    private static Task Ingest(ApiClient api, string _) => api.IngestNdjson(IngestPayload);
}