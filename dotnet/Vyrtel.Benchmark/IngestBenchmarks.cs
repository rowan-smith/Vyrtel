using BenchmarkDotNet.Attributes;
using Vyrtel.Harness;

namespace Vyrtel.Benchmark;

/// <summary>
/// Ingest throughput for each signal. Payloads are generated once so the measurement is
/// dominated by server ingest, not client-side JSON serialization.
/// </summary>
[MemoryDiagnoser]
[BenchmarkCategory("Ingest")]
public class IngestBenchmarks
{
    public const int BatchSize = 500;
    public const int MetricPoints = 500;

    private static ApiClient _api = null!;
    private static byte[] _ndjsonBatch = null!;
    private static byte[] _jsonBatch = null!;
    private static byte[] _trace = null!;
    private static byte[] _metricBatch = null!;

    [GlobalSetup]
    public void Setup()
    {
        var url = Environment.GetEnvironmentVariable("VYRTEL_URL") ?? "http://127.0.0.1:8080";
        _api = new ApiClient(url);
        var factory = new EventFactory(seed: 1337);
        _ndjsonBatch = factory.Ndjson(BatchSize);
        _jsonBatch = factory.Json(BatchSize);
        _trace = factory.TracePayload();
        _metricBatch = factory.OtlpMetrics(MetricPoints);
    }

    [GlobalCleanup]
    public void Cleanup() => _api.Dispose();

    [Benchmark]
    public Task IngestBatch() => _api.IngestNdjson(_ndjsonBatch);

    [Benchmark]
    public Task IngestBatchJson() => _api.IngestJson(_jsonBatch);

    [Benchmark]
    public Task IngestTraces() => _api.IngestOtlpTraces(_trace);

    [Benchmark]
    public Task IngestMetrics() => _api.IngestOtlpMetrics(_metricBatch);
}
