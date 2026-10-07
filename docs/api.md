# HTTP API

Base URL: `http://<host>:8080`. All bodies are JSON unless stated otherwise.
Times in responses are RFC 3339 strings with up to nanosecond precision.

## Conventions

### Errors

Every error has the same shape:

```json
{ "error": { "code": "invalid_query", "message": "Expected expression after 'and'", "position": 19 } }
```

| Status | `code` | When |
|-------:|--------|------|
| 400 | `invalid_request`, `invalid_json`, `invalid_payload`, `invalid_event`, `invalid_query` | malformed input (`position` for queries, `index` for batch events) |
| 401 | `unauthorized` | missing/invalid API key or session |
| 403 | `forbidden` | an ingest-only key used on a read/admin endpoint |
| 404 | `not_found` | unknown resource or endpoint |
| 413 | `payload_too_large` | body over `ingest.max_request_size`, or a batch larger than the queue |
| 415 | `unsupported_media_type` | content type not accepted |
| 422 | `query_too_expensive` | the query would read more than `query.max_scan_bytes` |
| 429 | `rate_limited` | ingest queue full — retry after `Retry-After` seconds |
| 503 | `unavailable` | storage unavailable / too many concurrent queries |
| 504 | `query_timeout` | the query exceeded `query.timeout` |
| 500 | `internal`, `storage_error` | details are logged server-side, never returned |

### Time inputs

`from` / `to` accept RFC 3339 (`"2026-10-06T10:00:00Z"`), Unix timestamps
(seconds, milliseconds, microseconds or nanoseconds, as numbers or strings),
`"now"`, and offsets from now such as `"-15m"` or `"now-1h"`. Ranges are
half-open: `from <= t < to`. Omitted bounds are unbounded (aggregations
default to the last hour).

## Authentication

Disabled by default (`auth.enabled = false`): everything is open, intended
for local and trusted networks. When enabled:

* **Ingestion** (`/api/v1/events`, `/v1/*`) requires an API key:
  `Authorization: Bearer <key>` or `X-Api-Key: <key>`.
* **Everything else under `/api/v1`** requires either the admin session
  cookie (from `/api/v1/auth/login`) or an API key with scope `admin`.
* `/health`, `/ready`, `/api/v1/auth/*` and the web UI assets are public.

```
POST /api/v1/auth/login    {"username": "admin", "password": "..."}  → sets HttpOnly cookie
POST /api/v1/auth/logout
GET  /api/v1/auth/me       → {"authEnabled": true, "user": "admin" | null}
```

### API keys

```
GET    /api/v1/api-keys
POST   /api/v1/api-keys        {"name": "checkout", "scope": "ingest" | "admin"}
DELETE /api/v1/api-keys/{id}
```

The create response contains the plaintext `key` — it is shown once; only a
SHA-256 hash is stored.

## Native event ingestion

```
POST /api/v1/events
Content-Type: application/json | application/x-ndjson
```

Body: a single event object, an array of events, `{"events": [...]}`, or
NDJSON (one object per line). Response: `{"accepted": <n>}`.

```json
{
  "timestamp": "2026-10-06T10:20:13.485Z",
  "level": "Error",
  "service": "payments",
  "environment": "production",
  "message": "Payment failed",
  "messageTemplate": "Payment failed for {customerId}",
  "traceId": "abc123",
  "spanId": "00f067aa0ba902b7",
  "properties": { "customerId": 481, "paymentProvider": "stripe", "amount": 71.50 },
  "resource": { "host.name": "web-1" },
  "exception": { "type": "TimeoutException", "message": "Payment provider timed out", "stackTrace": "..." }
}
```

| Field | Aliases | Notes |
|-------|---------|-------|
| `timestamp` | `@t`, `time`, `ts` | RFC 3339 or Unix number; default: arrival time |
| `level` | `@l`, `severity`, `severityText` | default `Information`; unknown spellings are kept in `severityText` |
| `message` | `@m`, `msg` | rendered from the template if absent |
| `messageTemplate` | `@mt`, `template` | `{Name}` holes are filled from properties |
| `service` | `serviceName`, `service.name` | falls back to `resource["service.name"]` |
| `environment` | `env` | falls back to `resource["deployment.environment[.name]"]` |
| `traceId`, `spanId` | `@tr`, `@sp`, `trace_id`, `span_id` | any string; hex is lowercased |
| `properties` | `attributes` | object |
| `exception` | `@x`, `error` | object or a stack-trace string |
| *anything else* | | becomes a property (Serilog CLEF style) |

Property values may be any JSON. The same property can change type between
events; nothing is rejected for that. Batches are all-or-nothing: if one
event is invalid the request fails with `invalid_event` and its `index`.
The response is sent only after the batch is in the write-ahead log (and
fsynced with `durability = "strict"`) and visible to queries.

## OTLP/HTTP

```
POST /v1/logs
POST /v1/traces
POST /v1/metrics
Content-Type: application/x-protobuf | application/json
Content-Encoding: gzip (optional)
```

Point any OpenTelemetry SDK or Collector `otlphttp` exporter at
`http://<host>:8080`. Responses use the request's encoding; records that
cannot be stored (e.g. spans without ids) are reported via `partialSuccess`.

Mapping:

* Resource attributes are kept as `resource`; `service.name` and
  `deployment.environment(.name)` also populate `service` / `environment`.
* Logs: severity number (or text) → `level`; string body → `message`
  (structured bodies are kept in the `body` property); `exception.*`
  attributes → `exception`; `{OriginalFormat}` → `messageTemplate`;
  instrumentation scope name → `otel.scope.name` property.
* Spans: start time → `timestamp`; `durationMs`, kind, status, events and
  links are preserved.
* Metrics: every data point is one event. Gauges and sums store `value`;
  histograms store count/sum/min/max/buckets; exponential histograms and
  summaries keep count/sum (bucket detail dropped).

gRPC is not supported in v0.1.

## Querying logs

```
POST /api/v1/query/logs
{
  "query": "level = \"Error\" and service = \"payments\"",
  "from": "-1h",
  "to": null,
  "limit": 200,
  "direction": "backward",
  "continuationToken": null
}
```

Response:

```json
{
  "events": [ { "id": "000000000000002a", "signal": "log", "timestamp": "...", "level": "Error", "message": "...", "properties": {}, "resource": {} } ],
  "diagnostics": {
    "elapsedMs": 41.2, "segmentsConsidered": 23, "segmentsSkipped": 19,
    "segmentsSkippedByTime": 12, "segmentsSkippedByIndex": 7, "segmentsNotNeeded": 0,
    "blocksConsidered": 400, "blocksSkipped": 383, "blocksRead": 17,
    "eventsExamined": 4812, "eventsMatched": 200, "bytesRead": 1843210,
    "unsealedEventsScanned": 950,
    "indexes": [ { "field": "timestamp", "kind": "time" }, { "field": "level", "kind": "bitmap" }, { "field": "customerId", "kind": "none" } ]
  },
  "continuationToken": "v1.18bd3c…"
}
```

* `direction`: `backward` (newest first, default) or `forward`.
* `limit`: 1–`query.max_results` (default max 1000).
* Pass `continuationToken` back to get the next page. Pagination is
  cursor-based (`(timestamp, id)`), never offset-based.

Aggregations (all take `query`, `from`, `to`):

```
POST /api/v1/query/{signal}/count       → {"count": n, "diagnostics": {...}}
POST /api/v1/query/{signal}/histogram   {"buckets": 60} → {"stepMs", "total", "buckets": [{"start", "count", "levels"}]}
POST /api/v1/query/{signal}/facets      {"sample": 2000} → {"sampled", "fields": [{"field", "count", "values": [{"value", "count"}]}]}
POST /api/v1/query/{signal}/events      same body/response as /query/logs
```

`{signal}` is `logs`, `traces` (spans) or `metrics` (data points).
`levels` in histogram buckets counts `[none, Trace, Debug, Information,
Warning, Error, Fatal]`. Facets are computed over the most recent `sample`
matching events.

### Live tail

```
GET /api/v1/live/logs?query=level%20%3D%20Error     (Server-Sent Events)
```

Events: `ready` once subscribed, then `event` (data: one event as JSON) for
each new matching event. `dropped` (data: count) when more than 200 events
matched in one batch, `lagged` (data: skipped batches) when the client fell
behind — the server never buffers unboundedly for a slow client. Use
`?signal=traces` for spans.

## Traces

```
POST /api/v1/query/traces   {"query": "status = Error", "from": "-1h", "limit": 50}
GET  /api/v1/traces?query=...&from=...&limit=...
```

Returns recent traces with at least one span matching the query:

```json
{ "traces": [ { "traceId": "4bf9…", "rootName": "POST /checkout", "rootService": "checkout",
                "start": "...", "durationMs": 800.0, "spanCount": 3,
                "services": ["checkout", "payments"], "errorCount": 2 } ],
  "diagnostics": {} }
```

```
GET /api/v1/traces/{traceId}[?from=...&to=...]
→ {"traceId", "summary": {...}, "spans": [...], "diagnostics": {...}}
```

Spans are ordered by start time. Correlated logs: query logs with
`traceId = "<id>"`.

## Metrics

```
GET  /api/v1/metrics      → {"metrics": [{"name": "http.server.requests"}, ...]}
POST /api/v1/query/metrics
{
  "name": "http.server.requests",
  "query": "route = \"/checkout\"",
  "from": "-1h",
  "agg": "sum",
  "stepMs": 60000,
  "groupBy": "route"
}
```

`agg`: `sum`, `count`, `min`, `max`, `avg` (default), `last`. For histogram
points, `sum`/`count`/`avg` use the histogram's sum and count. Response:
`{"name", "agg", "stepMs", "unit", "kind", "series": [{"group", "points": [{"ts", "value"}]}]}`
(at most 20 groups; the rest are merged into `(other)`).

## Dashboards

```
GET    /api/v1/dashboards
POST   /api/v1/dashboards                     {"name": "Payments", "description": null}
GET    /api/v1/dashboards/{id}
PUT    /api/v1/dashboards/{id}                {"name": "Payments (prod)"}
DELETE /api/v1/dashboards/{id}
POST   /api/v1/dashboards/{id}/panels         {"title", "kind", "config", "width", "position"}
PUT    /api/v1/dashboards/{id}/panels/{panelId}
DELETE /api/v1/dashboards/{id}/panels/{panelId}
```

Panel kinds and their `config`:

| `kind` | `config` |
|--------|----------|
| `log_count` | `{"query": "level = Error"}` — count over time |
| `metric_line` | `{"metric": "...", "agg": "avg", "groupBy"?: "...", "query"?: "..."}` |
| `single_stat` | `{"query": "..."}` (log count) or `{"metric": "...", "agg": "..."}` (latest value) |
| `saved_query` | `{"query": "..."}` — latest matching logs |

`width` is in grid columns (1–12). Dashboards take the time range from the
UI; panels have no hidden state.

## Saved queries

```
GET    /api/v1/saved-queries
POST   /api/v1/saved-queries   {"name": "Payment errors", "signal": "logs", "query": "..."}
DELETE /api/v1/saved-queries/{id}
```

## Alerts

```
GET    /api/v1/alerts
POST   /api/v1/alerts
GET    /api/v1/alerts/{id}            (includes recent "history")
PUT    /api/v1/alerts/{id}
DELETE /api/v1/alerts/{id}
POST   /api/v1/alerts/{id}/evaluate   evaluate immediately
```

```json
{
  "name": "Payment error spike",
  "query": "level = \"Error\" and service = \"payments\"",
  "op": ">",
  "threshold": 100,
  "windowSecs": 300,
  "intervalSecs": 60,
  "webhookUrl": "https://example.com/hooks/vyrtel",
  "enabled": true
}
```

v0.1 supports log-count threshold alerts: count matching logs over the last
`windowSecs` and compare with `op threshold` (`>`, `>=`, `<`, `<=`, `=`,
`!=`). Status is `OK`, `FIRING`, or `ERROR` (query failed). Each status
change is recorded and POSTed to the webhook (3 attempts, 10 s timeout):

```json
{
  "source": "vyrtel",
  "alert": { "id": 1, "name": "Payment error spike", "query": "...", "condition": "count > 100", "windowSeconds": 300 },
  "status": "FIRING",
  "previousStatus": "OK",
  "value": 150,
  "threshold": 100,
  "error": null,
  "evaluatedAt": "2026-10-06T10:21:00Z"
}
```

## System

```
GET /health                      → 200 {"status": "ok"} while the process runs
GET /ready                       → 200 {"status": "ready"} or 503 {"status": "degraded"}
GET /api/v1/system/info          version, uptime, event counts, queue depths, active segments, memory budgets
GET /api/v1/system/storage       raw/stored bytes, compression ratio, index overhead, per-signal detail
GET /api/v1/system/query-stats   per-field query statistics (input for future adaptive indexing)
GET /api/v1/system/config        effective configuration (secrets omitted)
```

`/api/v1/system/storage`:

```json
{
  "rawBytes": 13743895347, "storedBytes": 2576980378, "segmentBytes": 2280000000,
  "indexBytes": 210000000, "walBytes": 40000000, "metadataBytes": 98304,
  "eventCount": 18400000, "segmentCount": 241, "compressionRatio": 5.3, "indexOverhead": 0.092,
  "signals": { "logs": { ... }, "traces": { ... }, "metrics": { ... } },
  "indexCache": { "budgetBytes": 80530636, "usedBytes": 1234567, "entries": 12, "hits": 900, "misses": 12 }
}
```

`rawBytes` is the uncompressed size of events in Vyrtel's binary encoding
(sealed segments plus WAL); `compressionRatio` is raw ÷ (data + index) for
sealed segments.
