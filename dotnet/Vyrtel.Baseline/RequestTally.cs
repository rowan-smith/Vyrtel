using System.Collections.Concurrent;
using System.Globalization;

namespace Vyrtel.Baseline;

public enum Outcome
{
    Accepted,

    /// <summary>429 (ingest slots or queue full) or 503 (query slots busy, draining): retryable.</summary>
    Backpressure,

    /// <summary>413: the request is over <c>ingest.max_request_size</c>. Never expected from the fixed workload.</summary>
    TooLarge,

    /// <summary>Any other non-2xx status.</summary>
    Rejected,

    /// <summary>No HTTP response (connection refused, reset, timeout).</summary>
    TransportError,
}

/// <summary>Thread-safe count of requests by outcome and status code.</summary>
public sealed class RequestTally
{
    /// <summary>The status recorded for a request that got no HTTP response.</summary>
    public const int TransportErrorStatus = 0;

    private readonly ConcurrentDictionary<int, long> _byStatus = new();

    public static Outcome Classify(int status) => status switch
    {
        TransportErrorStatus => Outcome.TransportError,
        >= 200 and < 300 => Outcome.Accepted,
        429 or 503 => Outcome.Backpressure,
        413 => Outcome.TooLarge,
        _ => Outcome.Rejected,
    };

    public void Add(int status) => _byStatus.AddOrUpdate(status, 1, (_, n) => n + 1);

    public RequestCounts Snapshot() => Summarize(_byStatus);

    public static RequestCounts Summarize(IEnumerable<KeyValuePair<int, long>> byStatus)
    {
        long accepted = 0, backpressure = 0, tooLarge = 0, other = 0, transport = 0;
        var statuses = new SortedDictionary<string, long>(StringComparer.Ordinal);
        foreach (var (status, count) in byStatus)
        {
            switch (Classify(status))
            {
                case Outcome.Accepted: accepted += count; break;
                case Outcome.Backpressure: backpressure += count; break;
                case Outcome.TooLarge: tooLarge += count; break;
                case Outcome.Rejected: other += count; break;
                case Outcome.TransportError: transport += count; break;
            }
            var key = status == TransportErrorStatus ? "transport" : status.ToString(CultureInfo.InvariantCulture);
            statuses[key] = statuses.GetValueOrDefault(key) + count;
        }
        return new RequestCounts(
            accepted + backpressure + tooLarge + other + transport,
            accepted,
            backpressure,
            tooLarge,
            other,
            transport,
            statuses);
    }

    /// <summary>Adds request counts together (e.g. every operation of the mixed phase).</summary>
    public static RequestCounts Sum(IEnumerable<RequestCounts> counts)
    {
        var merged = new Dictionary<int, long>();
        foreach (var c in counts)
        {
            foreach (var (key, n) in c.ByStatus)
            {
                var status = key == "transport" ? TransportErrorStatus : int.Parse(key, CultureInfo.InvariantCulture);
                merged[status] = merged.GetValueOrDefault(status) + n;
            }
        }
        return Summarize(merged);
    }
}
