using System.Diagnostics;
using System.Text;
using Vyrtel.Harness;

namespace Vyrtel.Baseline;

/// <summary>A cold or warm query got no successful answer, so the run cannot produce a baseline.</summary>
public sealed class QueryFailedException(string message) : InvalidOperationException(message);

/// <summary>
/// Times one query cold, then warm. Every request must succeed: a failed request has no
/// latency worth comparing, and timing only the successes would let a broken endpoint (no
/// successes, so empty statistics of 0 ms) look like an improvement. The first failure, an HTTP
/// error or no response at all (a timeout, a refused connection), stops the run.
/// </summary>
public static class QueryMeasurer
{
    private const int BodyExcerpt = 200;

    public static async Task<QueryMeasurement> MeasureAsync(
        BaselineQuery query,
        Func<Task<RawResponse>> send,
        int warmIterations,
        double restartMs,
        int run,
        List<RawSample> samples)
    {
        var sw = Stopwatch.StartNew();
        var cold = await TrySend(send);
        var coldMs = sw.Elapsed.TotalMilliseconds;
        samples.Add(new RawSample(run, "query.cold", query.Name, coldMs, cold.Status));
        Require(cold, query, run, "its cold request");

        var warm = new LatencyRecorder();
        RawResponse? last = null;
        for (var i = 1; i <= warmIterations; i++)
        {
            sw.Restart();
            var response = await TrySend(send);
            var ms = sw.Elapsed.TotalMilliseconds;
            samples.Add(new RawSample(run, "query.warm", query.Name, ms, response.Status));
            Require(response, query, run, $"warm request {i} of {warmIterations}");
            warm.Record(ms);
            last = response;
        }

        return new QueryMeasurement(
            query.Name,
            query.Label,
            query.Category,
            coldMs,
            Workload.ParseDiagnostics(cold.Body),
            warm.Snapshot(),
            last is null ? null : Workload.ParseDiagnostics(last.Body),
            restartMs);
    }

    private static void Require(RawResponse response, BaselineQuery query, int run, string which)
    {
        if (response.IsSuccess)
        {
            return;
        }
        var body = Encoding.UTF8.GetString(response.Body);
        if (body.Length > BodyExcerpt)
        {
            body = body[..BodyExcerpt] + "…";
        }
        var what = response.Status == RequestTally.TransportErrorStatus
            ? $"got no HTTP response (timeout or connection error: {body})"
            : $"failed with HTTP {response.Status}: {body}";
        throw new QueryFailedException(
            $"run {run}: query {query.Name} ({query.Label}) {what} on {which}; " +
            "a query that cannot answer has no latency to compare, so no baseline was recorded");
    }

    /// <summary>Runs a request; a transport failure or timeout becomes status 0 instead of an exception.</summary>
    public static async Task<RawResponse> TrySend(Func<Task<RawResponse>> send)
    {
        try
        {
            return await send();
        }
        catch (Exception ex) when (ex is HttpRequestException or TaskCanceledException or IOException)
        {
            return new RawResponse(RequestTally.TransportErrorStatus, Encoding.UTF8.GetBytes(ex.Message));
        }
    }
}
