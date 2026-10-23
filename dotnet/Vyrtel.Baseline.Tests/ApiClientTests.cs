using System.Net;
using System.Net.Sockets;
using System.Text;
using Vyrtel.Harness;

namespace Vyrtel.Baseline.Tests;

/// <summary>
/// Review finding: a session cookie does not authorize ingestion on a server with
/// <c>auth.ingest_api_key = true</c>, so the harness sends a separate ingest key, and only on
/// ingest requests.
/// </summary>
public class ApiClientTests
{
    [Fact]
    public async Task The_ingest_key_goes_on_ingest_requests_only()
    {
        var heads = await Capture(async api =>
        {
            await api.IngestNdjson("{}"u8.ToArray());
            await api.IngestRaw("v1/traces", "{}"u8.ToArray(), "application/json");
            await api.CountLogs("");
        }, ingestKey: "vyr_ingest");

        Assert.Equal(4, heads.Count);
        Assert.StartsWith("POST /api/v1/auth/login ", heads[0]);
        Assert.DoesNotContain("Authorization:", heads[0], StringComparison.OrdinalIgnoreCase);
        Assert.StartsWith("POST /api/v1/events ", heads[1]);
        Assert.Contains("Authorization: Bearer vyr_ingest", heads[1], StringComparison.OrdinalIgnoreCase);
        Assert.StartsWith("POST /v1/traces ", heads[2]);
        Assert.Contains("Authorization: Bearer vyr_ingest", heads[2], StringComparison.OrdinalIgnoreCase);
        Assert.StartsWith("POST /api/v1/query/logs/count ", heads[3]);
        Assert.DoesNotContain("Authorization:", heads[3], StringComparison.OrdinalIgnoreCase);
    }

    [Fact]
    public async Task Without_a_key_ingest_requests_carry_no_authorization()
    {
        var heads = await Capture(api => api.IngestNdjson("{}"u8.ToArray()), ingestKey: null);

        Assert.StartsWith("POST /api/v1/events ", heads[1]);
        Assert.DoesNotContain("Authorization:", heads[1], StringComparison.OrdinalIgnoreCase);
    }

    /// <summary>Runs <paramref name="act"/> against a loopback server answering 200; returns each request head.</summary>
    private static async Task<List<string>> Capture(Func<ApiClient, Task> act, string? ingestKey)
    {
        using var listener = new TcpListener(IPAddress.Loopback, 0);
        listener.Start();
        var port = ((IPEndPoint)listener.LocalEndpoint).Port;
        var heads = new List<string>();
        using var stop = new CancellationTokenSource();
        var server = Task.Run(async () =>
        {
            while (!stop.IsCancellationRequested)
            {
                using var client = await listener.AcceptTcpClientAsync(stop.Token);
                var stream = client.GetStream();
                var text = new StringBuilder();
                var buffer = new byte[64 * 1024];
                while (!text.ToString().Contains("\r\n\r\n"))
                {
                    var n = await stream.ReadAsync(buffer, stop.Token);
                    if (n == 0) break;
                    text.Append(Encoding.ASCII.GetString(buffer, 0, n));
                }
                var all = text.ToString();
                lock (heads) heads.Add(all[..Math.Max(0, all.IndexOf("\r\n\r\n", StringComparison.Ordinal))]);
                const string response = "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}";
                await stream.WriteAsync(Encoding.ASCII.GetBytes(response), stop.Token);
            }
        });

        using (var api = new ApiClient($"http://127.0.0.1:{port}", ingestKey))
        {
            await act(api);
        }
        await stop.CancelAsync();
        listener.Stop();
        try { await server; } catch (OperationCanceledException) { } catch (SocketException) { } catch (ObjectDisposedException) { }
        return heads;
    }
}
