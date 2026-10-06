# DotnetLive

ASP.NET Core demo that pushes live telemetry into Observatory.

## What it sends

| Signal                      | How                                               |
|-----------------------------|---------------------------------------------------|
| Logs + errors + stacktraces | Custom Serilog sink → `POST /api/events/bulk`     |
| Metrics                     | Meter listener → `POST /api/metrics/bulk`         |
| Traces                      | Activity listener → `POST /v1/traces` (OTLP JSON) |

## Run

Start Observatory first (development mode is the default — open ingest):

```bash

cargo run -p app -- --seed --name Observatory --development true

```

Then:

```bash

cd dotnet-live

dotnet run

```

App listens on http://127.0.0.1:5088 and continuously simulates traffic (orders, payments, failures, slow requests).

## Config

`appsettings.json` only needs the Observatory endpoint and optional API key:

```json

{
  "Observatory": {
    "Endpoint": "http://localhost:5341",
    "Api": ""
  }
}

```

- **Service name** comes from the host application name (`IHostEnvironment.ApplicationName`).

- **Environment** comes from the host environment (`Development`, `Production`, …).

- **Api** is optional in Observatory development mode. In production, set it to an API key created in Observatory
  Settings and sent as `X-Api-Key`.

