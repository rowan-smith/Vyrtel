using System.Diagnostics;
using System.Diagnostics.Metrics;
using System.Net.Http.Json;
using System.Text.Json;
using System.Text.Json.Serialization;

namespace DotnetLive.Observatory;

public sealed class ObservatoryMetricsPublisher : BackgroundService
{
    private readonly IHttpClientFactory _httpClientFactory;
    private readonly IConfiguration _config;
    private readonly IHostEnvironment _env;
    private readonly ILogger<ObservatoryMetricsPublisher> _logger;
    private readonly MeterListener _listener;
    private readonly ConcurrentMetricBuffer _buffer = new();

    public ObservatoryMetricsPublisher(
        IHttpClientFactory httpClientFactory,
        IConfiguration config,
        IHostEnvironment env,
        ILogger<ObservatoryMetricsPublisher> logger)
    {
        _httpClientFactory = httpClientFactory;
        _config = config;
        _env = env;
        _logger = logger;
        _listener = new MeterListener
        {
            InstrumentPublished = (instrument, listener) =>
            {
                if (instrument.Meter.Name.StartsWith("DotnetLive", StringComparison.Ordinal))
                {
                    listener.EnableMeasurementEvents(instrument);
                }
            },
        };
        _listener.SetMeasurementEventCallback<int>((i, m, t, s) => OnMeasurement(i, m, t));
        _listener.SetMeasurementEventCallback<long>((i, m, t, s) => OnMeasurement(i, m, t));
        _listener.SetMeasurementEventCallback<double>((i, m, t, s) => OnMeasurement(i, m, t));
        _listener.Start();
    }

    private void OnMeasurement(Instrument instrument, double measurement, ReadOnlySpan<KeyValuePair<string, object?>> tags)
    {
        var attrs = new Dictionary<string, object?>();
        foreach (var tag in tags)
        {
            attrs[tag.Key] = tag.Value;
        }

        _buffer.Add(new MetricPoint
        {
            Name = $"{instrument.Meter.Name}.{instrument.Name}",
            Value = measurement,
            Unit = instrument.Unit,
            Timestamp = DateTime.UtcNow,
            Attributes = attrs,
        });
    }

    protected override async Task ExecuteAsync(CancellationToken stoppingToken)
    {
        var endpoint = _config["Observatory:Endpoint"] ?? "http://localhost:8080";
        var service = _env.ApplicationName;
        var environment = _env.EnvironmentName;

        while (!stoppingToken.IsCancellationRequested)
        {
            try
            {
                var batch = _buffer.Drain();
                if (batch.Count > 0)
                {
                    // OTLP/JSON: one gauge data point per recorded measurement.
                    var payload = new
                    {
                        resourceMetrics = new[]
                        {
                            new
                            {
                                resource = new
                                {
                                    attributes = new object[]
                                    {
                                        new { key = "service.name", value = new { stringValue = service } },
                                        new { key = "deployment.environment.name", value = new { stringValue = environment } },
                                    },
                                },
                                scopeMetrics = new[]
                                {
                                    new
                                    {
                                        metrics = batch.GroupBy(p => (p.Name, p.Unit)).Select(g => new
                                        {
                                            name = g.Key.Name,
                                            unit = g.Key.Unit ?? "",
                                            gauge = new
                                            {
                                                dataPoints = g.Select(p => new
                                                {
                                                    timeUnixNano = ((p.Timestamp.ToUniversalTime() - DateTime.UnixEpoch).Ticks * 100).ToString(),
                                                    asDouble = p.Value,
                                                    attributes = p.Attributes.Select(a => new { key = a.Key, value = new { stringValue = a.Value?.ToString() ?? "" } }).ToArray(),
                                                }).ToArray(),
                                            },
                                        }).ToArray(),
                                    },
                                },
                            },
                        },
                    };

                    var client = _httpClientFactory.CreateClient("observatory");
                    client.BaseAddress ??= new Uri(endpoint.TrimEnd('/') + "/");
                    using var response = await client.PostAsJsonAsync(
                        "v1/metrics",
                        payload,
                        stoppingToken);
                    if (!response.IsSuccessStatusCode)
                    {
                        _logger.LogWarning("Metric publish failed: {Status}", response.StatusCode);
                    }
                }
            }
            catch (Exception ex) when (ex is not OperationCanceledException)
            {
                _logger.LogDebug(ex, "Metric publish error");
            }

            await Task.Delay(TimeSpan.FromSeconds(2), stoppingToken);
        }
    }

    public override void Dispose()
    {
        _listener.Dispose();
        base.Dispose();
    }
}

internal sealed class ConcurrentMetricBuffer
{
    private readonly object _gate = new();
    private List<MetricPoint> _items = new();

    public void Add(MetricPoint point)
    {
        lock (_gate)
        {
            _items.Add(point);
        }
    }

    public List<MetricPoint> Drain()
    {
        lock (_gate)
        {
            var current = _items;
            _items = new List<MetricPoint>();
            return current;
        }
    }
}

internal sealed class MetricPoint
{
    public required string Name { get; init; }
    public double Value { get; init; }
    public string? Unit { get; init; }
    public DateTime Timestamp { get; init; }
    public Dictionary<string, object?> Attributes { get; init; } = new();
}

public sealed class TrafficSimulator : BackgroundService
{
    private readonly IHttpClientFactory _httpClientFactory;
    private readonly ILogger<TrafficSimulator> _logger;
    private static readonly ActivitySource ActivitySource = new("DotnetLive");
    private static readonly Meter Meter = new("DotnetLive");
    private static readonly Counter<long> Requests = Meter.CreateCounter<long>("http.server.requests");
    private static readonly Histogram<double> DurationMs = Meter.CreateHistogram<double>("http.server.request_duration_ms", "ms");
    private static readonly Counter<long> Orders = Meter.CreateCounter<long>("orders.created");
    private static readonly Counter<long> PaymentsFailed = Meter.CreateCounter<long>("payments.failed");

    public TrafficSimulator(IHttpClientFactory httpClientFactory, ILogger<TrafficSimulator> logger)
    {
        _httpClientFactory = httpClientFactory;
        _logger = logger;
    }

    protected override async Task ExecuteAsync(CancellationToken stoppingToken)
    {
        // Give the host a moment to start listening.
        await Task.Delay(1500, stoppingToken);
        var client = _httpClientFactory.CreateClient();
        client.BaseAddress = new Uri("http://127.0.0.1:5088/");
        var rng = Random.Shared;

        while (!stoppingToken.IsCancellationRequested)
        {
            var paths = new[]
            {
                "orders",
                "orders/42",
                "orders/0",
                "payments",
                "payments/fail",
                "health",
                "slow",
                "inventory/WIDGET",
                "inventory/MISSING",
            };
            var path = paths[rng.Next(paths.Length)];
            var sw = Stopwatch.StartNew();
            try
            {
                using var activity = ActivitySource.StartActivity("simulate.request", ActivityKind.Client);
                activity?.SetTag("http.route", path);
                using var response = await client.GetAsync(path, stoppingToken);

                // Mix in POST traffic for richer logs + stacktraces.
                if (rng.NextDouble() < 0.25)
                {
                    using var checkout = await client.PostAsJsonAsync(
                        "checkout",
                        new { customerId = rng.Next(1, 20) == 13 ? 13 : 42, sku = rng.NextDouble() < 0.15 ? "MISSING" : "WIDGET", qty = rng.Next(1, 60) },
                        stoppingToken);
                }
                if (rng.NextDouble() < 0.15)
                {
                    using var login = await client.PostAsJsonAsync(
                        "auth/login",
                        new { username = "demo", password = rng.NextDouble() < 0.4 ? "bad" : "secret" },
                        stoppingToken);
                }
                if (rng.NextDouble() < 0.1)
                {
                    using var mail = await client.PostAsJsonAsync(
                        "notifications/email",
                        new { orderId = 42, email = rng.NextDouble() < 0.5 ? "fail@example.com" : "ok@example.com" },
                        stoppingToken);
                }

                sw.Stop();
                Requests.Add(1, new KeyValuePair<string, object?>("route", path), new KeyValuePair<string, object?>("status", (int)response.StatusCode));
                DurationMs.Record(sw.Elapsed.TotalMilliseconds, new KeyValuePair<string, object?>("route", path));
                if (path == "orders") Orders.Add(1);
                if (path.Contains("fail", StringComparison.Ordinal) || !response.IsSuccessStatusCode)
                {
                    PaymentsFailed.Add(1);
                }
            }
            catch (Exception ex) when (ex is not OperationCanceledException)
            {
                _logger.LogWarning(ex, "Traffic simulator request failed for {Path}", path);
            }

            await Task.Delay(TimeSpan.FromMilliseconds(rng.Next(400, 1200)), stoppingToken);
        }
    }
}
