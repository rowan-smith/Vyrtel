using System.Text;

namespace Vyrtel.Harness;

/// <summary>
/// Seeds a running Vyrtel server with the same deterministic fixture the benchmark and
/// the profiler query. Logs go to <c>POST /api/v1/events</c> as NDJSON, traces to
/// <c>POST /v1/traces</c> as OTLP/JSON, so every run starts from the same dataset shape.
/// </summary>
public static class DatasetSeeder
{
    public static async Task SeedLogsAsync(
        ApiClient api,
        EventFactory factory,
        int total,
        int batch = 500,
        Action<string>? report = null,
        CancellationToken ct = default)
    {
        var sent = 0;
        var step = Math.Max(batch * 20, 10_000);
        while (sent < total)
        {
            var count = Math.Min(batch, total - sent);
            await api.IngestNdjson(factory.Ndjson(count), ct);
            sent += count;
            if ((sent % step == 0 || sent == total) && report is not null)
            {
                report($"  {sent:N0}/{total:N0} logs");
            }
        }
    }

    public static async Task SeedTracesAsync(
        ApiClient api,
        EventFactory factory,
        int count,
        Action<string>? report = null,
        CancellationToken ct = default)
    {
        for (var i = 1; i <= count; i++)
        {
            await api.IngestOtlpTraces(Encoding.UTF8.GetBytes(factory.NextTrace().ToJsonString()), ct);
            if ((i % 250 == 0 || i == count) && report is not null)
            {
                report($"  {i:N0}/{count:N0} traces");
            }
        }
    }

    public static async Task SeedMetricsAsync(
        ApiClient api,
        EventFactory factory,
        int points,
        int batch = 500,
        Action<string>? report = null,
        CancellationToken ct = default)
    {
        var sent = 0;
        var step = Math.Max(batch * 10, 1_000);
        while (sent < points)
        {
            var count = Math.Min(batch, points - sent);
            await api.IngestOtlpMetrics(factory.OtlpMetrics(count), ct);
            sent += count;
            if ((sent % step == 0 || sent == points) && report is not null)
            {
                report($"  {sent:N0}/{points:N0} metric points");
            }
        }
    }

    /// <summary>Sensible number of traces to seed for a given log-event count.</summary>
    public static int DefaultTraceCount(int events) => Math.Clamp(events / 100, 100, 1000);

    /// <summary>Sensible number of metric data points to seed for a given log-event count.</summary>
    public static int DefaultMetricCount(int events) => Math.Clamp(events / 1_000, 50, 2_000);
}