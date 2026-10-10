using BenchmarkDotNet.Attributes;
using Vyrtel.Harness;

namespace Vyrtel.Benchmark;

/// <summary>
/// Query latency against a server already seeded by the harness. Each method exercises a
/// different access path (index probe, block skip, full scan, aggregation) so the website
/// table shows where the cost actually sits.
/// </summary>
[MemoryDiagnoser]
[BenchmarkCategory("Query")]
public class QueryBenchmarks
{
    private static ApiClient _api = null!;
    private static string _traceId = "";

    [GlobalSetup]
    public void Setup()
    {
        var url = Environment.GetEnvironmentVariable("VYRTEL_URL") ?? "http://127.0.0.1:8080";
        _api = new ApiClient(url);
        _traceId = Environment.GetEnvironmentVariable("VYRTEL_SAMPLE_TRACE_ID") ?? new string('0', 32);
    }

    [GlobalCleanup]
    public void Cleanup() => _api.Dispose();

    [Benchmark]
    public Task<int> LogsLatest() => _api.QueryLogs("", 200);

    [Benchmark]
    public Task<int> LogsLevelEqualsError() => _api.QueryLogs("level = Error", 200);

    [Benchmark]
    public Task<int> LogsServiceEqualsPayments() => _api.QueryLogs("service = payments", 200);

    [Benchmark]
    public Task<int> LogsMessageContains() => _api.QueryLogs("message contains \"timed out\"", 200);

    [Benchmark]
    public Task<int> LogsDurationRange() => _api.QueryLogs("durationMs > 1990", 200);

    [Benchmark]
    public Task<int> LogsTraceId() => _api.QueryLogs($"traceId = \"{_traceId}\"", 200);

    [Benchmark]
    public Task<int> LogsCountLevelError() => _api.CountLogs("level = Error");

    [Benchmark]
    public Task<int> TraceSearch() => _api.QueryTraces("", 50);

    [Benchmark]
    public Task<int> MetricNames() => _api.MetricNames();
}
