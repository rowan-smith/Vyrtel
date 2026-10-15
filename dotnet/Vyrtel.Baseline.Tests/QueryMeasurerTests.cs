using System.Net;
using System.Net.Sockets;
using System.Text;
using Vyrtel.Harness;

namespace Vyrtel.Baseline.Tests;

/// <summary>
/// Review finding: timing only successful requests let a broken query endpoint record 0 ms and
/// look like an improvement. Any failed cold or warm request must stop the run instead.
/// </summary>
public class QueryMeasurerTests
{
    private static readonly BaselineQuery Query = Workload.Queries(new("trace-a", "trace-b", EventFactory.DefaultMetricName))[0];

    private static readonly byte[] Ok =
        """{"events":[],"diagnostics":{"elapsedMs":1.0,"segmentsConsidered":4,"segmentsSkipped":3,"blocksRead":2,"eventsExamined":10,"eventsMatched":1,"bytesRead":100,"indexes":[]}}"""u8.ToArray();

    private static Func<Task<RawResponse>> Responses(Func<int, RawResponse> byCall)
    {
        var call = 0;
        return () => Task.FromResult(byCall(++call));
    }

    [Fact]
    public async Task A_healthy_endpoint_is_measured_cold_and_warm()
    {
        var samples = new List<RawSample>();

        var m = await QueryMeasurer.MeasureAsync(Query, Responses(_ => new RawResponse(200, Ok)), 20, 510, 1, samples);

        Assert.Equal(20, m.Warm.Count);
        Assert.NotNull(m.ColdDiagnostics);
        Assert.Equal(100, m.WarmDiagnostics!.BytesRead);
        Assert.Equal(21, samples.Count);
    }

    [Fact]
    public async Task Every_warm_request_failing_cannot_record_zero_latency()
    {
        // The review's scenario: the cold request works, then all 20 warm ones return 500.
        var send = Responses(call => call == 1 ? new RawResponse(200, Ok) : new RawResponse(500, "boom"u8.ToArray()));

        var ex = await Assert.ThrowsAsync<QueryFailedException>(() => QueryMeasurer.MeasureAsync(Query, send, 20, 510, 1, []));

        Assert.Contains("failed with HTTP 500: boom on warm request 1 of 20", ex.Message);
        Assert.Contains("no baseline was recorded", ex.Message);
    }

    [Fact]
    public async Task A_failing_cold_request_stops_the_run()
    {
        var ex = await Assert.ThrowsAsync<QueryFailedException>(() =>
            QueryMeasurer.MeasureAsync(Query, Responses(_ => new RawResponse(503, "busy"u8.ToArray())), 20, 510, 1, []));

        Assert.Contains($"query {Query.Name}", ex.Message);
        Assert.Contains("failed with HTTP 503: busy on its cold request", ex.Message);
    }

    [Fact]
    public async Task One_failure_among_warm_requests_stops_the_run()
    {
        var send = Responses(call => call == 8 ? new RawResponse(500, []) : new RawResponse(200, Ok));

        var ex = await Assert.ThrowsAsync<QueryFailedException>(() => QueryMeasurer.MeasureAsync(Query, send, 20, 510, 1, []));

        Assert.Contains("on warm request 7 of 20", ex.Message);
    }

    [Fact]
    public async Task Repeated_timeouts_stop_the_run()
    {
        // HttpClient reports its timeout as TaskCanceledException.
        Func<Task<RawResponse>> send = () => throw new TaskCanceledException("The request was canceled due to the configured HttpClient.Timeout of 120 seconds elapsing.");

        var ex = await Assert.ThrowsAsync<QueryFailedException>(() => QueryMeasurer.MeasureAsync(Query, send, 20, 510, 1, []));

        Assert.Contains("got no HTTP response (timeout or connection error", ex.Message);
        Assert.Contains("on its cold request", ex.Message);
    }

    [Fact]
    public async Task A_long_error_body_is_trimmed_in_the_message()
    {
        var body = Encoding.UTF8.GetBytes(new string('x', 5000));

        var ex = await Assert.ThrowsAsync<QueryFailedException>(() =>
            QueryMeasurer.MeasureAsync(Query, Responses(_ => new RawResponse(500, body)), 20, 510, 1, []));

        Assert.True(ex.Message.Length < 600);
    }

    /// <summary>Through the real HTTP client: a server answering every query with 500 yields no measurement.</summary>
    [Fact]
    public async Task An_endpoint_returning_http_500_over_the_wire_cannot_produce_a_baseline()
    {
        using var listener = new TcpListener(IPAddress.Loopback, 0);
        listener.Start();
        var port = ((IPEndPoint)listener.LocalEndpoint).Port;
        using var stop = new CancellationTokenSource();
        var server = Task.Run(async () =>
        {
            while (!stop.IsCancellationRequested)
            {
                using var client = await listener.AcceptTcpClientAsync(stop.Token);
                var stream = client.GetStream();
                var buffer = new byte[64 * 1024];
                await stream.ReadAsync(buffer, stop.Token); // request line, headers and the small JSON body
                const string body = """{"error":{"code":"internal","message":"query engine failed"}}""";
                var response = $"HTTP/1.1 500 Internal Server Error\r\nContent-Type: application/json\r\nContent-Length: {body.Length}\r\nConnection: close\r\n\r\n{body}";
                await stream.WriteAsync(Encoding.ASCII.GetBytes(response), stop.Token);
            }
        });

        using var api = new ApiClient($"http://127.0.0.1:{port}");
        var ex = await Assert.ThrowsAsync<QueryFailedException>(() =>
            QueryMeasurer.MeasureAsync(Query, () => Query.Run(api), 20, 510, 1, []));

        Assert.Contains("failed with HTTP 500", ex.Message);
        Assert.Contains("query engine failed", ex.Message);
        await stop.CancelAsync();
        listener.Stop();
        try { await server; } catch (OperationCanceledException) { } catch (SocketException) { } catch (ObjectDisposedException) { }
    }
}
