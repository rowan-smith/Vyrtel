namespace Vyrtel.Benchmark;

public enum BenchmarkMetric
{
    /// <summary>Reported as the mean wall-clock latency of one request (milliseconds).</summary>
    Latency,

    /// <summary>Reported as sustained throughput (events per second).</summary>
    Throughput,
}

/// <summary>Human-friendly metadata for one <see cref="BenchmarkDotNet.Attributes.BenchmarkAttribute"/> method.</summary>
public sealed record BenchmarkMetadata(string Group, string Display, BenchmarkMetric Metric, int OperationsPerInvoke = 1);

/// <summary>
/// Maps benchmark method names to the labels the website renders. Keeping this explicit
/// (rather than inferring from PascalCase) is what makes the generated table readable.
/// </summary>
public static class BenchmarkCatalog
{
    public static readonly IReadOnlyDictionary<string, BenchmarkMetadata> ByMethod =
        new Dictionary<string, BenchmarkMetadata>
        {
            ["LogsLatest"] = new("Query", "Latest logs (limit 200)", BenchmarkMetric.Latency),
            ["LogsLevelEqualsError"] = new("Query", "Filter: level = Error", BenchmarkMetric.Latency),
            ["LogsServiceEqualsPayments"] = new("Query", "Filter: service = payments", BenchmarkMetric.Latency),
            ["LogsMessageContains"] = new("Query", "Full-text: message contains error (scan)", BenchmarkMetric.Latency),
            ["LogsDurationRange"] = new("Query", "Range: durationMs > 1990 (block skip)", BenchmarkMetric.Latency),
            ["LogsTraceId"] = new("Query", "Lookup: traceId = ... (bloom filter)", BenchmarkMetric.Latency),
            ["LogsCountLevelError"] = new("Query", "Count: level = Error", BenchmarkMetric.Latency),
            ["TraceSearch"] = new("Query", "Trace search (limit 50)", BenchmarkMetric.Latency),
            ["MetricNames"] = new("Query", "List metric names", BenchmarkMetric.Latency),
            ["IngestBatch"] = new("Ingest", "Ingest NDJSON batch", BenchmarkMetric.Throughput, IngestBenchmarks.BatchSize),
        };
}
