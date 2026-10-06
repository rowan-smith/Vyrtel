using System.Collections.Concurrent;
using System.Net.Http.Json;
using System.Text.Json;
using System.Text.Json.Serialization;
using Serilog.Core;
using Serilog.Events;

namespace DotnetLive.Observatory;

public sealed class ObservatorySink : ILogEventSink, IDisposable
{
    private readonly HttpClient _http;
    private readonly string _service;
    private readonly string _environment;
    private readonly ConcurrentQueue<ObservatoryEvent> _queue = new();
    private readonly CancellationTokenSource _cts = new();
    private readonly Task _worker;
    private readonly int _batchSize;
    private readonly TimeSpan _flushInterval;

    public ObservatorySink(
        HttpClient http,
        string endpoint,
        string service,
        string environment,
        string? apiKey = null,
        int batchSize = 50,
        int flushMs = 200)
    {
        _http = http;
        _http.BaseAddress = new Uri(endpoint.TrimEnd('/') + "/");
        if (!string.IsNullOrWhiteSpace(apiKey))
        {
            _http.DefaultRequestHeaders.Remove("X-Api-Key");
            _http.DefaultRequestHeaders.Add("X-Api-Key", apiKey);
        }
        _service = service;
        _environment = environment;
        _batchSize = batchSize;
        _flushInterval = TimeSpan.FromMilliseconds(flushMs);
        _worker = Task.Run(RunAsync);
    }

    public void Emit(LogEvent logEvent)
    {
        _queue.Enqueue(Map(logEvent));
    }

    private ObservatoryEvent Map(LogEvent logEvent)
    {
        var attributes = new Dictionary<string, object?>();
        foreach (var (key, value) in logEvent.Properties)
        {
            attributes[key] = Simplify(value);
        }

        ObservatoryException? exception = null;
        if (logEvent.Exception is not null)
        {
            // Native Exception.ToString() — same text Serilog gets from LogError(ex, "…")
            exception = new ObservatoryException
            {
                Type = logEvent.Exception.GetType().FullName,
                Message = logEvent.Exception.Message,
                StackTrace = logEvent.Exception.ToString(),
            };
        }

        var activity = System.Diagnostics.Activity.Current;
        return new ObservatoryEvent
        {
            Timestamp = logEvent.Timestamp.UtcDateTime,
            Level = MapLevel(logEvent.Level),
            Message = logEvent.RenderMessage(),
            MessageTemplate = logEvent.MessageTemplate.Text,
            Service = _service,
            Environment = _environment,
            TraceId = activity?.TraceId.ToHexString(),
            SpanId = activity?.SpanId.ToHexString(),
            Exception = exception,
            Properties = attributes,
        };
    }

    private static object? Simplify(LogEventPropertyValue value) =>
        value switch
        {
            ScalarValue { Value: not null } s => s.Value,
            SequenceValue seq => seq.Elements.Select(Simplify).ToArray(),
            StructureValue st => st.Properties.ToDictionary(p => p.Name, p => Simplify(p.Value)),
            DictionaryValue dict => dict.Elements.ToDictionary(
                kv => kv.Key.Value?.ToString() ?? "",
                kv => Simplify(kv.Value)),
            _ => value.ToString(),
        };

    private static string MapLevel(LogEventLevel level) =>
        level switch
        {
            LogEventLevel.Verbose => "trace",
            LogEventLevel.Debug => "debug",
            LogEventLevel.Information => "information",
            LogEventLevel.Warning => "warning",
            LogEventLevel.Error => "error",
            LogEventLevel.Fatal => "fatal",
            _ => "information",
        };

    private async Task RunAsync()
    {
        var buffer = new List<ObservatoryEvent>(_batchSize);
        while (!_cts.IsCancellationRequested)
        {
            try
            {
                while (buffer.Count < _batchSize && _queue.TryDequeue(out var item))
                {
                    buffer.Add(item);
                }

                if (buffer.Count > 0)
                {
                    await FlushAsync(buffer);
                    buffer.Clear();
                }
                else
                {
                    await Task.Delay(_flushInterval, _cts.Token);
                }
            }
            catch (OperationCanceledException) when (_cts.IsCancellationRequested)
            {
                break;
            }
            catch
            {
                await Task.Delay(500);
            }
        }

        while (_queue.TryDequeue(out var leftover))
        {
            buffer.Add(leftover);
        }

        if (buffer.Count > 0)
        {
            try { await FlushAsync(buffer); } catch { /* ignore on shutdown */ }
        }
    }

    private async Task FlushAsync(List<ObservatoryEvent> batch)
    {
        using var response = await _http.PostAsJsonAsync(
            "api/v1/events",
            batch,
            ObservatoryJson.Options,
            _cts.Token);
        response.EnsureSuccessStatusCode();
    }

    public void Dispose()
    {
        _cts.Cancel();
        try { _worker.Wait(TimeSpan.FromSeconds(2)); } catch { /* ignore */ }
        _cts.Dispose();
    }
}

public static class ObservatorySinkExtensions
{
    public static Serilog.LoggerConfiguration Observatory(
        this Serilog.Configuration.LoggerSinkConfiguration sinkConfiguration,
        string endpoint,
        string service,
        string environment,
        string? apiKey = null)
    {
        var http = new HttpClient { Timeout = TimeSpan.FromSeconds(5) };
        var sink = new ObservatorySink(http, endpoint, service, environment, apiKey);
        return sinkConfiguration.Sink(sink);
    }
}

internal sealed class ObservatoryEvent
{
    public DateTime Timestamp { get; set; }
    public string Level { get; set; } = "information";
    public string? Message { get; set; }
    public string? Service { get; set; }
    public string? Environment { get; set; }
    public string? TraceId { get; set; }
    public string? SpanId { get; set; }
    public string? MessageTemplate { get; set; }
    public ObservatoryException? Exception { get; set; }
    public Dictionary<string, object?> Properties { get; set; } = new();
}

internal sealed class ObservatoryException
{
    public string? Type { get; set; }
    public string? Message { get; set; }
    public string? StackTrace { get; set; }
}

internal static class ObservatoryJson
{
    public static readonly JsonSerializerOptions Options = new()
    {
        PropertyNamingPolicy = JsonNamingPolicy.CamelCase,
        DefaultIgnoreCondition = JsonIgnoreCondition.WhenWritingNull,
    };
}
