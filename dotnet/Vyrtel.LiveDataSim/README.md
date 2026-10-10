# Vyrtel.LiveDataSim

ASP.NET Core demo that pushes live telemetry into Vyrtel. It is a sample
producer, not part of the Vyrtel build. Benchmarks and latency profiling live in
the sibling projects [`Vyrtel.Benchmark`](../Vyrtel.Benchmark) and
[`Vyrtel.Profiling`](../Vyrtel.Profiling).

## What it sends

| Signal                      | How                                                     |
|-----------------------------|---------------------------------------------------------|
| Logs + errors + stacktraces | Custom Serilog sink → `POST /api/v1/events` (JSON batch) |
| Metrics                     | Meter listener → `POST /v1/metrics` (OTLP/JSON gauges)  |
| Traces                      | Activity listener → `POST /v1/traces` (OTLP/JSON)       |

Traces include the demo's own spans plus the ASP.NET Core server spans they nest
under, so each exported trace shows the full request path.

## Run

Start Vyrtel first (authentication is off by default):

```bash
cargo run -p server
```

Then:

```bash
dotnet run --project dotnet/Vyrtel.LiveDataSim
```

The app listens on http://127.0.0.1:5088 and continuously simulates traffic
(orders, payments, failures, slow requests).

## Config

`appsettings.json` only needs the Vyrtel endpoint and an optional API key:

```json
{
  "Vyrtel": {
    "Endpoint": "http://localhost:8080",
    "ServeUrl": "http://127.0.0.1:5088",
    "Api": ""
  }
}
```

- **Endpoint** is where telemetry is sent.
- **ServeUrl** is the demo's own listen address; the traffic simulator calls it
  too, so it must match the bound URL.
- **Service name** comes from the host application name.
- **Environment** comes from the host environment (`Development`, `Production`, …).
- **Api** is needed only when Vyrtel runs with `auth.enabled = true`; create
  an ingest key under Settings → API keys. It is sent as `X-Api-Key`.
