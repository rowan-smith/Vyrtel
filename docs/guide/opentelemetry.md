# Send data with OpenTelemetry

Vyrtel accepts OTLP over HTTP for logs, traces and metrics, in protobuf or JSON, optionally
gzip-compressed. Any OpenTelemetry SDK or Collector can send to it.

| Signal | Endpoint |
|---|---|
| Logs | `POST /v1/logs` |
| Traces | `POST /v1/traces` |
| Metrics | `POST /v1/metrics` |

## From an SDK

Most SDKs read the standard environment variables:

```bash
export OTEL_EXPORTER_OTLP_ENDPOINT=http://localhost:8080
export OTEL_EXPORTER_OTLP_PROTOCOL=http/protobuf
export OTEL_SERVICE_NAME=checkout
```

If ingestion is protected, also send an ingest key ([security](security.md#protect-ingestion)):

```bash
export OTEL_EXPORTER_OTLP_HEADERS="Authorization=Bearer <ingest key>"
```

Set the protocol to `http/protobuf` or `http/json`. Vyrtel does not accept gRPC, which is the
default in some SDKs.

## From a Collector

Use the `otlphttp` exporter:

```yaml
exporters:
  otlphttp/vyrtel:
    endpoint: http://vyrtel:8080
    headers:
      Authorization: Bearer ${env:VYRTEL_INGEST_KEY}

service:
  pipelines:
    logs:    { receivers: [otlp], exporters: [otlphttp/vyrtel] }
    traces:  { receivers: [otlp], exporters: [otlphttp/vyrtel] }
    metrics: { receivers: [otlp], exporters: [otlphttp/vyrtel] }
```

## From Serilog

[Serilog.Sinks.OpenTelemetry](https://github.com/serilog/serilog-sinks-opentelemetry) sends
over OTLP/HTTP. Message templates are kept, so you can search by template and by property.

```csharp
Log.Logger = new LoggerConfiguration()
    .WriteTo.OpenTelemetry(options =>
    {
        options.Endpoint = "http://localhost:8080/v1/logs";
        options.Protocol = OtlpProtocol.HttpProtobuf;
        options.ResourceAttributes["service.name"] = "checkout";
    })
    .CreateLogger();
```

## How OTLP maps to Vyrtel

| OTLP | Becomes |
|---|---|
| Resource `service.name` | `service` |
| Resource `deployment.environment` | `environment` |
| Log severity | `level` |
| Log body (string) | `message`; a structured body is kept in the `body` property |
| `exception.*` attributes | `exception` |
| `{OriginalFormat}` | `messageTemplate` |
| Span | An event with `durationMs`, kind, status, events and links |
| Metric data point | One event per point. Exponential histograms and summaries keep count and sum only |

Records that cannot be stored, such as spans without ids, are dropped and reported in
`partialSuccess`. The rest of the request is stored. Size limits, status codes and retry
behaviour are in the [HTTP API reference](../api.md#ingest-limits-and-retries).
