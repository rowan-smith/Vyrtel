# Observatory

Simple structured logging and trace server. One binary, one data directory.

## Quick start

```bash
# Build the UI (embedded into the binary)
cd web && npm install && npm run build && cd ..
```

```bash
# Run with sample telemetry
cargo run -p app -- --seed
```

Open http://localhost:5341

## Ingest a log

```bash
curl -X POST http://localhost:5341/api/events \
  -H "Content-Type: application/json" \
  -d "{
    \"level\": \"error\",
    \"message\": \"Payment failed\",
    \"service\": \"billing\",
    \"attributes\": {
      \"customerId\": 42,
      \"provider\": \"stripe\"
    }
  }"
```

Then search:

```text
service = "billing"
customerId = 42
```

Click a property → Include / Exclude to refine the query.

## Query language

```text
service = "api" and level >= warning
message contains "timeout"
database timeout
level = error | count
level = error | count by service
level = error | count by time(5m)
event_type = span | avg(duration_ns)
```

## OTLP

```text
POST /v1/logs
POST /v1/traces
```

JSON encoding only (no gRPC in v0.1).

## Config

Optional `config.toml`:

```toml
[server]
host = "0.0.0.0"
port = 5341

[storage]
path = "./data"

[retention]
days = 30
```

Environment overrides:

- `OBSERVATORY_SERVER_HOST`
- `OBSERVATORY_SERVER_PORT`
- `OBSERVATORY_STORAGE_PATH`
- `OBSERVATORY_RETENTION_DAYS`

## Workspace layout

```text
crates/
  app/        Axum server + static UI
  event/      shared event model
  ingest/     buffered writer
  storage/    Tantivy index
  query/      lexer / parser / planner
  metadata/   SQLite (filters, dashboards, settings)
  otlp/       OTLP JSON → Event
web/          React UI
```

## Tests

```bash
cargo test --workspace
```

Tests use temporary directories for the Tantivy index and SQLite database, so they never
touch `./data`. API tests in `crates/app/src/api/tests.rs` drive the real router in-process.

## Upgrading

If the event index on disk was written with an incompatible schema (for example, by an
older build), it is deleted and recreated empty on startup, and a warning is logged. Metadata
(accounts, dashboards, settings) lives in SQLite and is kept.
