# Query language

A deliberately small filter language. It selects events; it does not
aggregate, join or project (the API and UI do counting and charting).

```
level = "Error" and service = "payments"
durationMs > 500
message contains "timeout"
http.statusCode >= 500 and not environment = "staging"
(customerId = 123 or customerId = "c-123") and traceId != null
```

An empty query matches everything.

## Grammar

```
query      := expr? EOF
expr       := or
or         := and ( "or" and )*
and        := unary ( "and" unary )*
unary      := "not" unary | primary
primary    := "(" expr ")" | STRING | comparison
comparison := FIELD op value
op         := "=" | "!=" | ">" | ">=" | "<" | "<=" | "contains"
value      := STRING | NUMBER | "true" | "false" | "null" | WORD
```

* Keywords (`and`, `or`, `not`, `contains`, `true`, `false`, `null`) are
  case-insensitive.
* `==` is accepted for `=`, `<>` for `!=`, `&&` / `||` for `and` / `or`.
* Strings use double or single quotes; `\"`, `\'`, `\\`, `\n`, `\t`, `\r`
  escapes are supported.
* Numbers: `42`, `-7`, `71.5`, `1e3`. A number glued to letters (`5m`) is an
  error.
* An unquoted word on the right-hand side is a string: `level = Error` is
  `level = "Error"`.
* A bare string is shorthand for a message search: `"timeout"` means
  `message contains "timeout"`.
* Nesting is limited to 64 levels.

### Precedence

Highest to lowest: comparison, `not`, `and`, `or`. `and` and `or` are
left-associative. Use parentheses to override:

```
a = 1 or b = 2 and c = 3        →  a = 1 or (b = 2 and c = 3)
not a = 1 and b = 2             →  (not a = 1) and b = 2
```

## Fields

Well-known names (case-insensitive) address envelope fields:

| Field                                                                            | Applies to | Meaning                                                           |
|----------------------------------------------------------------------------------|------------|-------------------------------------------------------------------|
| `timestamp` (`@t`)                                                               | all        | event time; compare with RFC 3339 strings or Unix numbers         |
| `level` (`@l`, `severity`)                                                       | logs       | `Trace` < `Debug` < `Information` < `Warning` < `Error` < `Fatal` |
| `service`, `environment` (`env`)                                                 | all        |                                                                   |
| `message` (`@m`, `msg`)                                                          | all        | log message, span name or metric name                             |
| `messageTemplate` (`@mt`)                                                        | logs       |                                                                   |
| `traceId` (`@tr`), `spanId` (`@sp`)                                              | all        | hex ids compare case-insensitively                                |
| `exception.type`, `exception.message`, `exception.stackTrace` (`stackTrace`)     | logs       |                                                                   |
| `name`, `parentSpanId`, `durationMs` (`duration`), `status`, `spanKind` (`kind`) | traces     | `status` is `Unset`/`Ok`/`Error`                                  |
| `name`, `value`, `unit`, `metricKind`                                            | metrics    |                                                                   |

Everything else is a **structured property**: looked up in the event's
attributes first, then in its resource attributes.

* `attributes.x` / `properties.x` — attributes only.
* `resource.x` — resource attributes only (e.g. `resource.host.name`).

### Nested fields

Dotted paths reach into objects: `http.statusCode = 500` matches both
`{"http": {"statusCode": 500}}` (JSON producers) and a flat key
`"http.statusCode": 500` (OpenTelemetry). Every split point is tried, so
`{"a.b": {"c": 1}}` is reachable as `a.b.c`.

Property names are case-sensitive; well-known field names are not. If a
property is called like a well-known field (e.g. `level` on a span), use
`attributes.level`.

## Types and comparison rules

Telemetry is messy, so comparisons coerce sensibly instead of failing:

| Stored value     | `= "abc"`                           | `= 123`    | `> 100`    | `= true` |
|------------------|-------------------------------------|------------|------------|----------|
| `"abc"`          | ✓ exact, case-sensitive            | ✗         | ✗         | ✗       |
| `"123"`          | ✗ (unless the literal is `"123"`)  | ✓ numeric | ✓ numeric | ✗       |
| `123`, `123.0`   | ✓ if the string literal is numeric | ✓         | ✓         | ✗       |
| `true`, `"TRUE"` | –                                   | ✗         | ✗         | ✓       |
| missing / `null` | ✗                                  | ✗         | ✗         | ✗       |

* **Equality**: strings compare exactly; numbers numerically (`1 = 1.0`);
  numeric strings coerce to numbers; `"true"`/`"false"` (any case) compare
  equal to booleans.
* **Ordering** (`> >= < <=`): with a number literal, values that coerce to
  numbers compare numerically and others never match; with a string literal,
  strings compare lexicographically (numerically if both sides are numeric).
* **`contains`**: case-insensitive substring match on the text form.
* **Missing and `null`**: only `field = null` and `field != <value>` match.
  `field != null` means "present and not null".
* **Arrays** match if any element matches (`tags = "red"`). `tags != "red"`
  means no element equals `"red"`.
* **Levels** understand names and aliases (`err`, `warn`, `info`, `crit`...)
  and order, so `level >= Warning` selects warnings, errors and fatals. A
  level literal that is not a level matches nothing.
* **Timestamps**: `timestamp > "2026-10-06T10:00:00Z"` or a Unix number in
  seconds, milliseconds, microseconds or nanoseconds.

## Indexes and performance

Queries are always correct; indexes only make them faster. You can see what
was used in the result's `diagnostics` (and the UI's diagnostics panel):

| Predicate                                                                                                   | Index                         |
|-------------------------------------------------------------------------------------------------------------|-------------------------------|
| time range, `timestamp` comparisons                                                                         | segment/block time ranges     |
| `level`, `service`, `environment`                                                                           | bitmap indexes                |
| `field = value` on properties, `traceId`, `spanId`, span/metric `name`, `exception.type`, `messageTemplate` | Bloom filters                 |
| numeric `=`, `>`, `>=`, `<`, `<=`                                                                           | zone maps (per-block min/max) |
| `contains`, `!=`, `not`, `null`                                                                             | none — evaluated by scanning  |

`and` intersects candidate blocks, `or` unions them, `not` disables pruning
for its operand. Results are returned newest first and the scan stops early
once the page is full and no older segment could contain a newer event.

## Execution limits and cancellation

Every query request runs within a budget, so one expensive query cannot
starve ingestion or other users. A request that goes over a limit stops and
returns an error that names the limit. It never returns the rows it had
gathered, so a partial answer is never presented as complete.

| Limit                         | Default | Applies to                                                       | Over the limit                    |
|-------------------------------|---------|------------------------------------------------------------------|-----------------------------------|
| `query.timeout`               | `30s`   | execution time of one request, from its first checkpoint          | `504 query_timeout` (`limitMs`)   |
| `query.max_scan_bytes`        | `4GB`   | compressed segment bytes one request reads                        | `422 query_too_expensive` (`limitBytes`) |
| `query.max_results`           | `1000`  | `limit` (page size) of a search                                   | `400 invalid_request`, before execution |
| `query.max_trace_spans`       | `50000` | spans of one trace (`GET /api/v1/traces/{id}`)                    | `422 query_result_limit` (`limit`) |
| metric points (fixed)         | `10000` | points per series: time range ÷ `stepMs`                          | `422 query_result_limit` (`limit`), before execution |
| `query.max_metric_names`      | `10000` | names `GET /api/v1/metrics` returns                               | the first names by name, `truncated: true` |
| `query.max_concurrent`        | `4`     | requests executing at once                                        | waits, then `503 unavailable`     |
| `query.queue_timeout`         | `10s`   | how long a request waits for a free slot                          | `503 unavailable` + `Retry-After` |

Notes:

* A **request** is everything one API call runs. Trace search runs up to 20
  pages and then gathers the spans of the traces found; facets sample
  events and then count them. All of it shares one clock and one byte
  count, so splitting the work never resets a limit.
* **Boundaries**: reading exactly `max_scan_bytes` succeeds, one byte more
  fails. A trace with exactly `max_trace_spans` spans is returned in full.
  `max_scan_bytes` counts compressed bytes read from sealed segments only:
  unsealed (recent, in-memory) events and counts answered from block
  summaries read nothing. Time spent waiting for a slot does not count
  towards `timeout`.
* **Trace search summaries** keep at most 10,000 spans per trace. A summary
  with more has `truncated: true`, and its `spanCount`, `services`,
  `errorCount` and `durationMs` cover the kept spans only (the UI shows the
  span count as `10000+`).
* **Metric discovery** (`GET /api/v1/metrics`) is a request too: it takes a
  query slot, runs within `timeout`, stops on disconnect, and keeps at most
  `max_metric_names` names in memory, so its cost is bounded however many
  distinct names are stored. Beyond the limit it lists the first names
  alphabetically and sets `truncated: true` (the UI's metric picker then
  ends with "… more names not listed").
* **Live tail** is not a query in this sense: it filters new batches as they
  are written and has its own limits (see [API](api.md#live-tail)).

### Checkpoints

Execution checks its budget at these points, so a cancelled or over-budget
query stops after at most one block read or 256 evaluated rows:

| Checkpoint | Where                                                         |
|------------|---------------------------------------------------------------|
| start      | before any data is read                                       |
| segment    | before each segment is planned (index lookups)                |
| block      | before and after each block read                              |
| rows       | every 256 rows evaluated, sealed or unsealed                  |
| assemble   | between the phases of a multi-phase query (trace search, facets) |
| aggregate  | every 256 items of in-memory work: facet values counted (property leaves included) and the final ordering; metric names collected |

### Client disconnects

When the client goes away while its query runs (closed tab, `curl` killed,
proxy timeout), the server cancels the query; it stops at its next
checkpoint and frees its slot. Nothing is sent, since nobody is listening;
cancelled queries are counted in `/api/v1/system/info` (`query.cancelled`)
and logged at `debug` level. The server notices a disconnect when the
client's connection closes.

A query keeps its slot until it has actually stopped, so abandoned queries
always count against `query.max_concurrent`: at most that many queries are
ever executing, however many clients connect, disconnect or retry.

### Example

The trace from `tests/fixtures/otlp-traces.json` (three spans), with a
deliberately small span budget; the same fixture and limits as the
`scan_and_result_budgets_are_explicit_errors` integration test:

```bash
printf '[query]\nmax_trace_spans = 2\n' > budgets.toml
vyrtel --config budgets.toml --data-dir ./budgets-data &

now=$(date +%s)000000000
sed "s/{{now[^}]*}}/$now/g" tests/fixtures/otlp-traces.json |
  curl -s localhost:8080/v1/traces -H 'Content-Type: application/json' --data-binary @-
# {}

curl -s localhost:8080/api/v1/traces/4bf92f3577b34da6a3ce929d0e0e4736
# {"error":{"code":"query_result_limit","message":"query result would exceed 2 spans in one trace; narrow the time range with from/to, or raise query.max_trace_spans","limit":2}}

# A metric series over 3 hours at 1 s steps needs 10,800 points:
curl -s localhost:8080/api/v1/query/metrics -H 'Content-Type: application/json' \
  -d '{"name": "http.server.duration", "from": "-3h", "stepMs": 1000}'
# {"error":{"code":"query_result_limit","message":"query result would exceed 10000 points per series; use a larger stepMs or a shorter time range","limit":10000}}

# The server stays usable: a request inside the budgets answers in full.
curl -s localhost:8080/api/v1/traces | jq '.traces[] | {spanCount, truncated}'
# {"spanCount": 3, "truncated": false}

curl -s localhost:8080/api/v1/system/info | jq -c .query
# {"maxConcurrent":4,"inFlight":0,"rejected":0,"cancelled":0,"timedOut":0,"tooExpensive":0,"resultLimited":2}
```

`query_timeout` (`504`, `limitMs`) and `query_too_expensive` (`422`,
`limitBytes`) look the same; they depend on the data and the host, so the
tests produce them with a checkpoint hook rather than a large dataset.

To recover from a limit error, narrow the time range, add an indexed filter
(`level`, `service`, `traceId`, a property equality) instead of `contains`,
or raise the limit in [`[query]`](configuration.md#query). Limits are
checked per request; nothing is left behind by a stopped query, and the
next request runs with its full budget.

## Limitations (v0.1)

* No aggregation, grouping or projection syntax (the API offers counts,
  histograms, facets and metric aggregations separately).
* No regular expressions, wildcards, `in (...)` lists or arithmetic.
* `contains` on log messages always scans (no full-text index).
* Field names must be identifiers: letters, digits, `_ . - @ $`, not starting
  with a digit. Properties with other characters in their names cannot be
  queried yet.
