using System.Net.Http.Headers;
using BenchmarkDotNet.Attributes;
using Vyrtel.Harness;

namespace Vyrtel.Benchmark;

/// <summary>
/// Ingest throughput: posts a fixed NDJSON batch and reports events per second. The
/// payload is generated once so the measurement is dominated by server ingest, not JSON
/// serialization on the client.
/// </summary>
[MemoryDiagnoser]
[BenchmarkCategory("Ingest")]
public class IngestBenchmarks
{
    public const int BatchSize = 500;

    private static HttpClient _http = null!;
    private static byte[] _payload = null!;

    [GlobalSetup]
    public void Setup()
    {
        var url = Environment.GetEnvironmentVariable("VYRTEL_URL") ?? "http://127.0.0.1:8080";
        _http = new HttpClient
        {
            BaseAddress = new Uri(url.TrimEnd('/') + "/"),
            Timeout = TimeSpan.FromMinutes(2),
        };
        _payload = new EventFactory(seed: 1337).Ndjson(BatchSize);
    }

    [GlobalCleanup]
    public void Cleanup() => _http.Dispose();

    [Benchmark]
    public async Task<int> IngestBatch()
    {
        using var content = new ByteArrayContent(_payload);
        content.Headers.ContentType = new MediaTypeHeaderValue("application/x-ndjson");
        using var response = await _http.PostAsync("api/v1/events", content);
        var bytes = await response.Content.ReadAsByteArrayAsync();
        if (!response.IsSuccessStatusCode)
        {
            throw new HttpRequestException($"POST api/v1/events -> {(int)response.StatusCode}");
        }
        return bytes.Length;
    }
}
