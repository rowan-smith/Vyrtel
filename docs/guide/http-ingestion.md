# Send logs over HTTP

You don't need an SDK. Post JSON to `/api/v1/events` from a script, a log shipper or your own
code.

```bash
curl -X POST http://localhost:8080/api/v1/events \
  -H 'content-type: application/x-ndjson' \
  --data-binary @app.ndjson
```

## Request body

Any of these shapes works:

- one event object
- an array of events, or `{"events": [...]}`
- NDJSON, one event per line (`Content-Type: application/x-ndjson`)

Add `Content-Encoding: gzip` to compress. The response is `{"accepted": <n>}`, sent once the
events are durable and searchable. A batch is all-or-nothing: if one event is invalid, nothing
is stored and the error gives that event's `index`.

## Event fields

```json
{
  "timestamp": "2026-10-06T10:20:13.485Z",
  "level": "Error",
  "service": "payments",
  "environment": "production",
  "messageTemplate": "Payment failed for {customerId}",
  "traceId": "4bf92f3577b34da6a3ce929d0e0e4736",
  "properties": { "customerId": 481, "http": { "statusCode": 502 } },
  "exception": { "type": "TimeoutException", "message": "Payment provider timed out" }
}
```

| Field | Also accepted as | Notes |
|---|---|---|
| `timestamp` | `@t`, `time`, `ts` | RFC 3339 or Unix time. Defaults to arrival time |
| `level` | `@l`, `severity` | Defaults to `Information` |
| `message` | `@m`, `msg` | Rendered from the template if missing |
| `messageTemplate` | `@mt`, `template` | `{Name}` holes are filled from properties |
| `service` | `serviceName`, `service.name` | |
| `environment` | `env` | |
| `traceId`, `spanId` | `@tr`, `@sp` | Links the log to its trace |
| `properties` | `attributes` | Any JSON, nested up to 32 levels |
| `exception` | `@x`, `error` | An object, or a stack trace string |

Any other top-level field becomes a property. Serilog's
[CLEF](https://clef-json.org/) format (`@t`, `@mt`, `@x` and so on) therefore works as it is.
Nested properties are searchable by path, for example `http.statusCode >= 500`.

## Authentication and limits

If ingestion is protected, send an ingest key as `Authorization: Bearer <key>` or
`X-Api-Key: <key>` ([security](security.md#protect-ingestion)).

A request may be up to 1 MB, both compressed and uncompressed. When Vyrtel is busy it answers
`429` or `503` with `Retry-After`. Retry those, and don't retry other `4xx` responses. Details
are in the [HTTP API reference](../api.md#ingest-limits-and-retries).
