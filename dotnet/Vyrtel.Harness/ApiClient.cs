using System.Net.Http.Headers;
using System.Text;
using System.Text.Json;

namespace Vyrtel.Harness;

/// <summary>
/// Thin helper over the Vyrtel HTTP API shared by the benchmark and the profiler. Every
/// call reads the full response body so connection reuse is measured, and throws with
/// the server's error text when a request fails.
/// </summary>
public sealed class ApiClient : IDisposable
{
    private readonly HttpClient _http;

    public ApiClient(string baseUrl)
    {
        _http = new HttpClient
        {
            BaseAddress = new Uri(baseUrl.TrimEnd('/') + "/"),
            Timeout = TimeSpan.FromMinutes(2),
        };
    }

    private static StringContent Json(object body) =>
        new(JsonSerializer.Serialize(body), Encoding.UTF8, "application/json");

    public Task<int> QueryLogs(string query, int limit, string from = "-1h") =>
        PostJson("api/v1/query/logs", new { query, from, limit, direction = "backward" });

    public Task<int> CountLogs(string query, string from = "-1h") =>
        PostJson("api/v1/query/logs/count", new { query, from });

    public Task<int> QueryTraces(string query, int limit, string from = "-1h") =>
        PostJson("api/v1/query/traces", new { query, from, limit });

    public Task<int> Histogram(string signal, string query, int buckets = 60, string from = "-1h") =>
        PostJson($"api/v1/query/{signal}/histogram", new { query, from, buckets });

    public Task<int> Facets(string signal, string query, int sample = 2000, string from = "-1h") =>
        PostJson($"api/v1/query/{signal}/facets", new { query, from, sample });

    public Task<int> MetricQuery(string name, string from = "-1h") =>
        PostJson("api/v1/query/metrics", new { name, from });

    public Task<int> GetTrace(string traceId, string from = "-1h") =>
        Send("GET", $"api/v1/traces/{traceId}?from={Uri.EscapeDataString(from)}", null);

    public Task<int> MetricNames() => Send("GET", "api/v1/metrics", content: null);

    /// <summary>
    /// Reads the server's own storage counters. Returns null when the endpoint is unavailable
    /// (older builds), so a profile never blocks a benchmark run.
    /// </summary>
    public async Task<StorageProfile?> GetStorageStatsAsync(CancellationToken ct = default)
    {
        try
        {
            using var doc = JsonDocument.Parse(await GetString("api/v1/system/storage", ct));
            var root = doc.RootElement;
            static long GetInt(JsonElement r, string name) =>
                r.TryGetProperty(name, out var v) && v.TryGetInt64(out var n) ? n : 0;
            static double? GetDouble(JsonElement r, string name) =>
                r.TryGetProperty(name, out var v) && v.TryGetDouble(out var n) ? n : null;
            return new StorageProfile(
                RawBytes: GetInt(root, "rawBytes"),
                StoredBytes: GetInt(root, "storedBytes"),
                EventCount: GetInt(root, "eventCount"),
                SegmentCount: GetInt(root, "segmentCount"),
                CompressionRatio: GetDouble(root, "compressionRatio"),
                IndexOverhead: GetDouble(root, "indexOverhead"),
                ReceivedBytes: GetInt(root, "receivedBytes"),
                SegmentBytes: GetInt(root, "segmentBytes"),
                IndexBytes: GetInt(root, "indexBytes"),
                WalBytes: GetInt(root, "walBytes"),
                MetadataBytes: GetInt(root, "metadataBytes"));
        }
        catch
        {
            return null;
        }
    }

    public Task IngestNdjson(byte[] payload, CancellationToken ct = default) =>
        Ingest("api/v1/events", payload, "application/x-ndjson", ct);

    public Task IngestJson(byte[] payload, CancellationToken ct = default) =>
        Ingest("api/v1/events", payload, "application/json", ct);

    public Task IngestOtlpTraces(byte[] payload, CancellationToken ct = default) =>
        Ingest("v1/traces", payload, "application/json", ct);

    public Task IngestOtlpMetrics(byte[] payload, CancellationToken ct = default) =>
        Ingest("v1/metrics", payload, "application/json", ct);

    public async Task<string> GetString(string path, CancellationToken ct = default)
    {
        using var response = await _http.GetAsync(path, ct);
        var body = await response.Content.ReadAsStringAsync(ct);
        if (!response.IsSuccessStatusCode)
        {
            throw new HttpRequestException($"GET {path} -> {(int)response.StatusCode}: {body}");
        }
        return body;
    }

    private async Task Ingest(string path, byte[] payload, string mediaType, CancellationToken ct)
    {
        using var content = new ByteArrayContent(payload);
        content.Headers.ContentType = new MediaTypeHeaderValue(mediaType);
        using var response = await _http.PostAsync(path, content, ct);
        var body = await response.Content.ReadAsStringAsync(ct);
        if (!response.IsSuccessStatusCode)
        {
            throw new HttpRequestException($"POST {path} -> {(int)response.StatusCode}: {body}");
        }
    }

    private async Task<int> Send(string method, string path, HttpContent? content)
    {
        using var request = new HttpRequestMessage(new HttpMethod(method), path) { Content = content };
        using var response = await _http.SendAsync(request);
        var bytes = await response.Content.ReadAsByteArrayAsync();
        if (!response.IsSuccessStatusCode)
        {
            throw new HttpRequestException($"{method} {path} -> {(int)response.StatusCode}: {Encoding.UTF8.GetString(bytes)}");
        }
        return bytes.Length;
    }

    private async Task<int> PostJson(string path, object body)
    {
        using var content = Json(body);
        using var response = await _http.PostAsync(path, content);
        var bytes = await response.Content.ReadAsByteArrayAsync();
        if (!response.IsSuccessStatusCode)
        {
            throw new HttpRequestException($"POST {path} -> {(int)response.StatusCode}: {Encoding.UTF8.GetString(bytes)}");
        }
        return bytes.Length;
    }

    public void Dispose() => _http.Dispose();
}