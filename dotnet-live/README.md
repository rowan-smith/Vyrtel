# DotnetLive

ASP.NET Core demo that pushes live telemetry into Observer. It is a sample
producer, not part of the Observer build.

## What it sends

| Signal                      | How                                                     |
|-----------------------------|---------------------------------------------------------|
| Logs + errors + stacktraces | Custom Serilog sink → `POST /api/v1/events` (JSON batch) |
| Metrics                     | Meter listener → `POST /v1/metrics` (OTLP/JSON gauges)  |
| Traces                      | Activity listener → `POST /v1/traces` (OTLP/JSON)       |

## Run

Start Observer first (authentication is off by default):

```bash
cargo run -p server
```

Then:

```bash
cd dotnet-live
dotnet run
```

The app listens on http://127.0.0.1:5088 and continuously simulates traffic
(orders, payments, failures, slow requests).

## Config

`appsettings.json` only needs the Observer endpoint and an optional API key:

```json
{
  "Observatory": {
    "Endpoint": "http://localhost:8080",
    "Api": ""
  }
}
```

- **Service name** comes from the host application name.
- **Environment** comes from the host environment (`Development`, `Production`, …).
- **Api** is needed only when Observer runs with `auth.enabled = true`; create
  an ingest key under Settings → API keys. It is sent as `X-Api-Key`.
