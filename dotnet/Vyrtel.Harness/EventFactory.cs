using System.Text;
using System.Text.Json.Nodes;

namespace Vyrtel.Harness;

/// <summary>
/// Deterministic telemetry generator that mirrors <c>tools/loadgen</c>, so synthetic
/// benchmark and profile traffic resembles the real load generator (services, error
/// rate, high-cardinality ids, nested HTTP properties and stack traces).
/// </summary>
public sealed class EventFactory
{
    private static readonly string[] ServiceNames =
    [
        "payments", "orders", "auth", "inventory", "search", "shipping",
        "notifications", "gateway", "billing", "catalog", "recommendations", "users",
    ];

    private static readonly string[] Routes = ["/api/orders", "/api/pay", "/api/login", "/api/search", "/api/cart", "/health"];
    private static readonly string[] Regions = ["eu-west-1", "us-east-1", "ap-southeast-2"];
    private static readonly string[] Providers = ["stripe", "adyen", "paypal"];

    private static readonly (string Type, string Message)[] Errors =
    [
        ("TimeoutException", "Payment provider timed out"),
        ("DbException", "deadlock detected while updating order"),
        ("HttpRequestException", "upstream returned 503"),
        ("InvalidOperationException", "inventory reservation conflict"),
    ];

    private readonly Random _rng;
    private readonly int _services;
    private readonly double _errorRate;
    private readonly long _cardinality;

    public EventFactory(int seed = 42, int services = 8, double errorRate = 0.02, long cardinality = 10_000)
    {
        _rng = new Random(seed);
        _services = Math.Clamp(services, 1, ServiceNames.Length);
        _errorRate = errorRate;
        _cardinality = Math.Max(1, cardinality);
    }

    /// <summary>The trace id of the first generated log, handy for an indexed trace-lookup benchmark.</summary>
    public string SampleTraceId { get; private set; } = "";

    public JsonObject NextLog()
    {
        var service = ServiceNames[_rng.Next(_services)];
        var route = Routes[_rng.Next(Routes.Length)];
        var customer = _rng.NextInt64(_cardinality);
        var duration = Math.Round(Math.Pow(_rng.NextDouble(), 3) * 2000.0);
        var isError = _rng.NextDouble() < _errorRate;
        var traceId = NextHex(32);
        if (SampleTraceId.Length == 0)
        {
            SampleTraceId = traceId;
        }

        var properties = new JsonObject
        {
            ["customerId"] = customer,
            ["requestId"] = "req-" + NextHex(16),
            ["durationMs"] = duration,
            ["region"] = Regions[_rng.Next(Regions.Length)],
            ["http"] = new JsonObject
            {
                ["method"] = route == "/api/search" ? "GET" : "POST",
                ["route"] = route,
                ["statusCode"] = isError ? 500 : _rng.NextDouble() < 0.05 ? 404 : 200,
            },
        };

        var log = new JsonObject
        {
            ["timestamp"] = DateTimeOffset.UtcNow.AddSeconds(-_rng.Next(0, 1800)).ToString("o"),
            ["service"] = service,
            ["environment"] = _rng.NextDouble() < 0.9 ? "production" : "staging",
            ["traceId"] = traceId,
            ["properties"] = properties,
        };

        if (isError)
        {
            var (type, message) = Errors[_rng.Next(Errors.Length)];
            log["level"] = "Error";
            log["message"] = $"{message} (customer {customer})";
            log["messageTemplate"] = $"{message} (customer {{customerId}})";
            log["exception"] = new JsonObject
            {
                ["type"] = type,
                ["message"] = message,
                ["stackTrace"] =
                    $"{type}: {message}\n   at {service}.Handler.Handle() in Handler.cs:line {_rng.Next(10, 400)}\n   at {service}.Pipeline.Run()",
            };
            if (service == "payments")
            {
                properties["paymentProvider"] = Providers[_rng.Next(Providers.Length)];
            }
        }
        else
        {
            var method = route == "/api/search" ? "GET" : "POST";
            log["level"] = duration > 1500 ? "Warning" : _rng.NextDouble() < 0.1 ? "Debug" : "Information";
            log["message"] = $"{method} {route} responded in {duration} ms";
            log["messageTemplate"] = "{Method} {Route} responded in {DurationMs} ms";
        }

        return log;
    }

    /// <summary>Encodes <paramref name="count"/> logs as NDJSON, ready for <c>POST /api/v1/events</c>.</summary>
    public byte[] Ndjson(int count)
    {
        var builder = new StringBuilder(count * 400);
        for (var i = 0; i < count; i++)
        {
            builder.Append(NextLog().ToJsonString());
            builder.Append('\n');
        }
        return Encoding.UTF8.GetBytes(builder.ToString());
    }

    /// <summary>A small OTLP/JSON trace (three spans) for <c>POST /v1/traces</c>.</summary>
    public JsonObject NextTrace()
    {
        var traceId = NextHex(32);
        var rootSpan = NextHex(16);
        var childSpan = NextHex(16);
        var dbSpan = NextHex(16);
        var nowNanos = DateTimeOffset.UtcNow.ToUnixTimeMilliseconds() * 1_000_000L;
        var total = _rng.NextInt64(5_000_000, 900_000_000);
        var start = nowNanos - total;
        var error = _rng.NextDouble() < _errorRate;
        var service = ServiceNames[_rng.Next(_services)];

        JsonObject Span(string id, string? parent, string name, long spanStart, long spanEnd, bool isError)
        {
            var span = new JsonObject
            {
                ["traceId"] = traceId,
                ["spanId"] = id,
                ["name"] = name,
                ["kind"] = 2,
                ["startTimeUnixNano"] = spanStart.ToString(),
                ["endTimeUnixNano"] = spanEnd.ToString(),
                ["status"] = new JsonObject { ["code"] = isError ? 2 : 1 },
            };
            if (parent is not null)
            {
                span["parentSpanId"] = parent;
            }
            return span;
        }

        return new JsonObject
        {
            ["resourceSpans"] = new JsonArray
            {
                new JsonObject
                {
                    ["resource"] = new JsonObject
                    {
                        ["attributes"] = new JsonArray
                        {
                            new JsonObject { ["key"] = "service.name", ["value"] = new JsonObject { ["stringValue"] = "gateway" } },
                        },
                    },
                    ["scopeSpans"] = new JsonArray
                    {
                        new JsonObject
                        {
                            ["spans"] = new JsonArray
                            {
                                Span(rootSpan, null, "POST /api", start, nowNanos, error),
                            },
                        },
                    },
                },
                new JsonObject
                {
                    ["resource"] = new JsonObject
                    {
                        ["attributes"] = new JsonArray
                        {
                            new JsonObject { ["key"] = "service.name", ["value"] = new JsonObject { ["stringValue"] = service } },
                        },
                    },
                    ["scopeSpans"] = new JsonArray
                    {
                        new JsonObject
                        {
                            ["spans"] = new JsonArray
                            {
                                Span(childSpan, rootSpan, "handle request", start + total / 10, nowNanos - total / 10, error),
                                Span(dbSpan, childSpan, "SELECT", start + total / 5, start + total / 2, false),
                            },
                        },
                    },
                },
            },
        };
    }

    private string NextHex(int chars)
    {
        var bytes = new byte[chars / 2];
        for (var i = 0; i < bytes.Length; i++)
        {
            bytes[i] = (byte)_rng.Next(256);
        }
        return Convert.ToHexString(bytes).ToLowerInvariant();
    }
}
