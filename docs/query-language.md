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

## Limitations (v0.1)

* No aggregation, grouping or projection syntax (the API offers counts,
  histograms, facets and metric aggregations separately).
* No regular expressions, wildcards, `in (...)` lists or arithmetic.
* `contains` on log messages always scans (no full-text index).
* Field names must be identifiers: letters, digits, `_ . - @ $`, not starting
  with a digit. Properties with other characters in their names cannot be
  queried yet.
