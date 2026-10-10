using System.Diagnostics;
using System.Diagnostics.Metrics;
using Vyrtel.LiveDataSim.Vyrtel;
using Vyrtel.LiveDataSim.Services;
using OpenTelemetry.Resources;
using OpenTelemetry.Trace;
using Serilog;

var builder = WebApplication.CreateBuilder(args);

var vyrtelEndpoint = builder.Configuration["Vyrtel:Endpoint"] ?? "http://localhost:8080";
var vyrtelApi = builder.Configuration["Vyrtel:Api"] ?? "";
var serviceName = builder.Environment.ApplicationName;
var environmentName = builder.Environment.EnvironmentName;

Log.Logger = new LoggerConfiguration()
    .MinimumLevel.Debug()
    .MinimumLevel.Override("Microsoft.AspNetCore", Serilog.Events.LogEventLevel.Warning)
    .Enrich.FromLogContext()
    .Enrich.WithProperty("Application", serviceName)
    .Enrich.WithProperty("Environment", environmentName)
    .WriteTo.Console()
    .WriteTo.Vyrtel(vyrtelEndpoint, serviceName, environmentName, vyrtelApi)
    .CreateLogger();

builder.Host.UseSerilog();

builder.Services.AddHttpClient("vyrtel", client =>
{
    client.BaseAddress = new Uri(vyrtelEndpoint.TrimEnd('/') + "/");
    client.Timeout = TimeSpan.FromSeconds(5);
    if (!string.IsNullOrWhiteSpace(vyrtelApi))
    {
        client.DefaultRequestHeaders.Remove("X-Api-Key");
        client.DefaultRequestHeaders.Add("X-Api-Key", vyrtelApi);
    }
});
builder.Services.AddHttpClient();

builder.Services.AddSingleton<InventoryService>();
builder.Services.AddSingleton<PaymentService>();
builder.Services.AddSingleton<OrderService>();
builder.Services.AddSingleton<NotificationService>();
builder.Services.AddSingleton<AuthService>();

builder.Services.AddOpenTelemetry()
    .ConfigureResource(r => r.AddService(serviceName: serviceName, serviceVersion: "0.1.0"))
    .WithTracing(t => t
        .AddSource("Vyrtel.LiveDataSim")
        .AddAspNetCoreInstrumentation()
        .AddHttpClientInstrumentation());

builder.Services.AddHostedService<VyrtelMetricsPublisher>();
builder.Services.AddHostedService<TrafficSimulator>();
builder.Services.AddHostedService<TraceJsonExporter>();

builder.WebHost.UseUrls("http://127.0.0.1:5088");

var app = builder.Build();

var activitySource = new ActivitySource("Vyrtel.LiveDataSim");
var meter = new Meter("Vyrtel.LiveDataSim");
var requestCounter = meter.CreateCounter<long>("http.server.requests");
var duration = meter.CreateHistogram<double>("http.server.request_duration_ms", unit: "ms");
var orderCounter = meter.CreateCounter<long>("orders.created");
var paymentFailCounter = meter.CreateCounter<long>("payments.failed");

app.Use(async (ctx, next) =>
{
    var log = ctx.RequestServices.GetRequiredService<ILoggerFactory>().CreateLogger("HttpPipeline");
    var sw = Stopwatch.StartNew();
    var path = ctx.Request.Path.Value ?? "/";
    using var scope = log.BeginScope(new Dictionary<string, object?>
    {
        ["RequestId"] = ctx.TraceIdentifier,
        ["HttpMethod"] = ctx.Request.Method,
        ["RequestPath"] = path,
        ["ClientIp"] = ctx.Connection.RemoteIpAddress?.ToString(),
    });

    log.LogInformation("HTTP {Method} {Path} started", ctx.Request.Method, path);
    try
    {
        await next();
        sw.Stop();
        requestCounter.Add(1,
            KeyValuePair.Create<string, object?>("route", path),
            KeyValuePair.Create<string, object?>("status", ctx.Response.StatusCode));
        duration.Record(sw.Elapsed.TotalMilliseconds, KeyValuePair.Create<string, object?>("route", path));

        if (ctx.Response.StatusCode >= 500)
        {
            log.LogError(
                "HTTP {Method} {Path} completed {StatusCode} in {ElapsedMs:0.0}ms",
                ctx.Request.Method,
                path,
                ctx.Response.StatusCode,
                sw.Elapsed.TotalMilliseconds);
        }
        else if (ctx.Response.StatusCode >= 400)
        {
            log.LogWarning(
                "HTTP {Method} {Path} completed {StatusCode} in {ElapsedMs:0.0}ms",
                ctx.Request.Method,
                path,
                ctx.Response.StatusCode,
                sw.Elapsed.TotalMilliseconds);
        }
        else
        {
            log.LogInformation(
                "HTTP {Method} {Path} completed {StatusCode} in {ElapsedMs:0.0}ms",
                ctx.Request.Method,
                path,
                ctx.Response.StatusCode,
                sw.Elapsed.TotalMilliseconds);
        }
    }
    catch (Exception ex)
    {
        sw.Stop();
        log.LogError(
            ex,
            "HTTP {Method} {Path} failed after {ElapsedMs:0.0}ms",
            ctx.Request.Method,
            path,
            sw.Elapsed.TotalMilliseconds);
        throw;
    }
});

app.MapGet("/health", (ILogger<Program> log) =>
{
    log.LogDebug("Health probe");
    return Results.Ok(new { status = "ok", service = serviceName, environment = environmentName });
});

app.MapPost("/auth/login", async (LoginRequest body, AuthService auth, CancellationToken ct) =>
{
    try
    {
        var result = await auth.LoginAsync(body.Username ?? "guest", body.Password ?? "", ct);
        return Results.Ok(result);
    }
    catch (UnauthorizedAccessException ex)
    {
        return Results.Problem(title: ex.Message, statusCode: 401, detail: ExceptionFormatter.Format(ex));
    }
});

app.MapGet("/orders", async (OrderService orders, CancellationToken ct) =>
{
    using var activity = activitySource.StartActivity("orders.list", ActivityKind.Server);
    var result = await orders.ListAsync(42, ct);
    orderCounter.Add(1);
    return Results.Ok(result);
});

app.MapGet("/orders/{id:int}", async (int id, OrderService orders, CancellationToken ct) =>
{
    using var activity = activitySource.StartActivity("orders.get", ActivityKind.Server);
    activity?.SetTag("order.id", id);
    try
    {
        return Results.Ok(await orders.GetAsync(id, ct));
    }
    catch (Exception ex)
    {
        activity?.SetStatus(ActivityStatusCode.Error, ex.Message);
        return Results.Problem(title: "Order lookup failed", statusCode: 404, detail: ExceptionFormatter.Format(ex));
    }
});

app.MapPost("/checkout", async (CheckoutRequest body, OrderService orders, CancellationToken ct) =>
{
    using var activity = activitySource.StartActivity("checkout", ActivityKind.Server);
    try
    {
        var result = await orders.CheckoutAsync(body.CustomerId, body.Sku ?? "WIDGET", body.Qty, ct);
        orderCounter.Add(1);
        return Results.Ok(result);
    }
    catch (Exception ex)
    {
        paymentFailCounter.Add(1);
        activity?.SetStatus(ActivityStatusCode.Error, ex.Message);
        return Results.Problem(title: "Checkout failed", statusCode: 502, detail: ExceptionFormatter.Format(ex));
    }
});

app.MapGet("/payments", async (PaymentService payments, CancellationToken ct) =>
{
    using var activity = activitySource.StartActivity("payments.authorize", ActivityKind.Server);
    await payments.ChargeAsync(42, "WIDGET", 19.99m, ct);
    return Results.Ok(new { ok = true });
});

app.MapGet("/payments/fail", (PaymentService payments) =>
{
    using var activity = activitySource.StartActivity("payments.fail", ActivityKind.Server);
    paymentFailCounter.Add(1);
    try
    {
        payments.FailDemo(42);
        return Results.Ok();
    }
    catch (Exception ex)
    {
        activity?.SetStatus(ActivityStatusCode.Error, ex.Message);
        return Results.Problem(title: "Payment failed", statusCode: 502, detail: ExceptionFormatter.Format(ex));
    }
});

app.MapPost("/notifications/email", async (EmailRequest body, NotificationService notifications, CancellationToken ct) =>
{
    try
    {
        await notifications.SendOrderEmailAsync(body.OrderId, body.Email ?? "user@example.com", ct);
        return Results.Accepted();
    }
    catch (Exception ex)
    {
        return Results.Problem(title: "Notification failed", statusCode: 500, detail: ExceptionFormatter.Format(ex));
    }
});

app.MapGet("/slow", async (ILogger<Program> log) =>
{
    using var activity = activitySource.StartActivity("orders.slow", ActivityKind.Server);
    var delay = Random.Shared.Next(250, 600);
    await Task.Delay(delay);
    log.LogWarning("Slow request completed in {DurationMs} ms", delay);
    return Results.Ok(new { delayedMs = delay });
});

app.MapGet("/inventory/{sku}", async (string sku, InventoryService inventory, CancellationToken ct) =>
{
    var available = await inventory.CheckSkuAsync(sku, ct);
    return Results.Ok(new { sku, available });
});

app.MapGet("/", () => Results.Ok(new
{
    service = serviceName,
    environment = environmentName,
    vyrtel = vyrtelEndpoint,
    endpoints = new[]
    {
        "/orders", "/orders/{id}", "/checkout", "/payments", "/payments/fail",
        "/notifications/email", "/auth/login", "/inventory/{sku}", "/slow", "/health",
    },
}));

try
{
    Log.Information(
        "Vyrtel.LiveDataSim starting → Vyrtel {Endpoint} as {Service}/{Environment}",
        vyrtelEndpoint,
        serviceName,
        environmentName);
    app.Run();
}
finally
{
    Log.CloseAndFlush();
}

public record LoginRequest(string? Username, string? Password);
public record CheckoutRequest(int CustomerId, string? Sku, int Qty);
public record EmailRequest(int OrderId, string? Email);

/// Periodically exports in-memory completed activities as OTLP JSON traces.
public sealed class TraceJsonExporter : BackgroundService
{
    private readonly IHttpClientFactory _httpClientFactory;
    private readonly IHostEnvironment _env;
    private readonly IConfiguration _config;
    private readonly ILogger<TraceJsonExporter> _logger;
    private readonly ActivityListener _listener;
    private readonly List<object> _completed = new();
    private readonly object _gate = new();

    public TraceJsonExporter(
        IHttpClientFactory httpClientFactory,
        IHostEnvironment env,
        IConfiguration config,
        ILogger<TraceJsonExporter> logger)
    {
        _httpClientFactory = httpClientFactory;
        _env = env;
        _config = config;
        _logger = logger;
        _listener = new ActivityListener
        {
            ShouldListenTo = source => source.Name == "Vyrtel.LiveDataSim",
            Sample = (ref ActivityCreationOptions<ActivityContext> _) => ActivitySamplingResult.AllDataAndRecorded,
            ActivityStopped = activity =>
            {
                lock (_gate)
                {
                    _completed.Add(new
                    {
                        traceId = activity.TraceId.ToHexString(),
                        spanId = activity.SpanId.ToHexString(),
                        parentSpanId = activity.ParentSpanId == default
                            ? ""
                            : activity.ParentSpanId.ToHexString(),
                        name = activity.DisplayName,
                        startTimeUnixNano = ToUnixNano(activity.StartTimeUtc).ToString(),
                        endTimeUnixNano = ToUnixNano(activity.StartTimeUtc + activity.Duration).ToString(),
                        attributes = activity.TagObjects.Select(t => new
                        {
                            key = t.Key,
                            value = new { stringValue = t.Value?.ToString() },
                        }).ToArray(),
                        status = activity.Status == ActivityStatusCode.Error
                            ? new { code = 2, message = activity.StatusDescription }
                            : new { code = 0, message = (string?)null },
                    });
                }
            },
        };
        ActivitySource.AddActivityListener(_listener);
    }

    protected override async Task ExecuteAsync(CancellationToken stoppingToken)
    {
        var endpoint = _config["Vyrtel:Endpoint"] ?? "http://localhost:8080";
        var service = _env.ApplicationName;
        var environment = _env.EnvironmentName;

        while (!stoppingToken.IsCancellationRequested)
        {
            List<object> batch;
            lock (_gate)
            {
                batch = _completed.ToList();
                _completed.Clear();
            }

            if (batch.Count > 0)
            {
                var payload = new
                {
                    resourceSpans = new[]
                    {
                        new
                        {
                            resource = new
                            {
                                attributes = new[]
                                {
                                    new { key = "service.name", value = new { stringValue = service } },
                                    new
                                    {
                                        key = "deployment.environment",
                                        value = new { stringValue = environment },
                                    },
                                },
                            },
                            scopeSpans = new[]
                            {
                                new { spans = batch },
                            },
                        },
                    },
                };

                try
                {
                    var client = _httpClientFactory.CreateClient("vyrtel");
                    client.BaseAddress ??= new Uri(endpoint.TrimEnd('/') + "/");
                    using var response = await client.PostAsJsonAsync("v1/traces", payload, stoppingToken);
                    if (!response.IsSuccessStatusCode)
                    {
                        _logger.LogDebug("Trace export status {Status}", response.StatusCode);
                    }
                }
                catch (Exception ex) when (ex is not OperationCanceledException)
                {
                    _logger.LogDebug(ex, "Trace export failed");
                }
            }

            await Task.Delay(1000, stoppingToken);
        }
    }

    private static long ToUnixNano(DateTime utc) =>
        new DateTimeOffset(DateTime.SpecifyKind(utc, DateTimeKind.Utc)).ToUnixTimeMilliseconds() * 1_000_000L;

    public override void Dispose()
    {
        _listener.Dispose();
        base.Dispose();
    }
}
