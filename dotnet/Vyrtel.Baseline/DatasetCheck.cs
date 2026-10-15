using System.Text.Json;

namespace Vyrtel.Baseline;

/// <summary>Events per signal: log events, spans and metric data points.</summary>
public sealed record SignalCounts(long Logs, long Traces, long Metrics)
{
    public static readonly SignalCounts Zero = new(0, 0, 0);

    public SignalCounts Add(SignalCounts o) => new(Logs + o.Logs, Traces + o.Traces, Metrics + o.Metrics);

    public override string ToString() => $"{Logs:N0} logs, {Traces:N0} spans, {Metrics:N0} metric points";
}

/// <summary>
/// Proves the seeded dataset is complete before anything is measured. A server that acknowledged
/// events but kept fewer would leave a smaller, easier dataset, and every query would look faster
/// against it; so the expected counts come from the fixture payloads, not from the server, and any
/// shortfall (or surplus) fails the run.
/// </summary>
public static class DatasetCheck
{
    /// <summary>Counts the elements of every array named <paramref name="arrayName"/> anywhere in a JSON payload (e.g. OTLP <c>spans</c> or <c>dataPoints</c>).</summary>
    public static long CountItems(byte[] json, string arrayName)
    {
        using var doc = JsonDocument.Parse(json);
        return Count(doc.RootElement, arrayName);

        static long Count(JsonElement e, string name)
        {
            long n = 0;
            switch (e.ValueKind)
            {
                case JsonValueKind.Object:
                    foreach (var p in e.EnumerateObject())
                    {
                        if (p.NameEquals(name) && p.Value.ValueKind == JsonValueKind.Array)
                        {
                            n += p.Value.GetArrayLength();
                        }
                        n += Count(p.Value, name);
                    }
                    break;
                case JsonValueKind.Array:
                    foreach (var item in e.EnumerateArray())
                    {
                        n += Count(item, name);
                    }
                    break;
            }
            return n;
        }
    }

    /// <summary>Reads <c>eventCounts</c> from <c>GET /api/v1/system/info</c>.</summary>
    public static SignalCounts ParseStored(string systemInfoJson)
    {
        using var doc = JsonDocument.Parse(systemInfoJson);
        var c = doc.RootElement.GetProperty("eventCounts");
        return new SignalCounts(c.GetProperty("logs").GetInt64(), c.GetProperty("traces").GetInt64(), c.GetProperty("metrics").GetInt64());
    }

    /// <summary>
    /// Null when the server acknowledged and stored exactly the fixture; otherwise what is wrong.
    /// </summary>
    public static string? Verify(SignalCounts expected, SignalCounts acknowledged, SignalCounts stored)
    {
        var problems = new List<string>();
        void Check(string what, long want, long ack, long have)
        {
            if (ack != want) problems.Add($"{what}: the server acknowledged {ack:N0} of {want:N0}");
            if (have != want) problems.Add($"{what}: {have:N0} stored, expected {want:N0}");
        }
        Check("logs", expected.Logs, acknowledged.Logs, stored.Logs);
        Check("spans", expected.Traces, acknowledged.Traces, stored.Traces);
        Check("metric points", expected.Metrics, acknowledged.Metrics, stored.Metrics);
        return problems.Count == 0 ? null : "the seeded dataset is incomplete, so no baseline was recorded: " + string.Join("; ", problems);
    }
}
