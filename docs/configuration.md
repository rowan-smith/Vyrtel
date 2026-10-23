# Configuration

Vyrtel runs with no configuration at all. When you need to change
something, use any combination of:

1. a TOML file (`--config vyrtel.toml`, or `./vyrtel.toml` if present),
2. `VYRTEL_*` environment variables,
3. command-line flags,

in increasing order of precedence. `vyrtel config` prints the effective
configuration; `GET /api/v1/system/config` returns it (without secrets).
Unknown keys are rejected so typos do not silently do nothing, and so are
unknown [environment variables](#environment-variables) that look like
settings.

Sizes accept `B`, `KB`, `MB`, `GB` (powers of 1024; `MiB` etc. also work).
Durations accept `ms`, `s`, `m`, `h`, `d`, `w`, combinations like `1h30m`,
or a bare number of seconds.

## Example

```toml
[server]
bind = "0.0.0.0:8080"
shutdown_timeout = "8s"

[storage]
path = "./data"
max_memory = "512MB"
segment_target_size = "64MB"
segment_max_age = "5m"
durability = "normal"

[retention]
logs = "30d"
traces = "14d"
metrics = "30d"

[ingest]
max_request_size = "1MB"
queue_capacity = 10000

[auth]
enabled = true
admin_password = "change-me"     # or VYRTEL_AUTH_ADMIN_PASSWORD
```

## Reference

### `[server]`

| Key                 | Default        | Env                               | Description                                                                                                                                                                                                    |
|---------------------|----------------|-----------------------------------|----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| `bind`              | `0.0.0.0:8080` | `VYRTEL_SERVER_BIND`              | Listen address for the UI, API and OTLP/HTTP.                                                                                                                                                                  |
| `shutdown_timeout`  | `8s`           | `VYRTEL_SERVER_SHUTDOWN_TIMEOUT`  | After a stop signal, how long accepted work may take to drain before Vyrtel reports what is unfinished and exits with status 1. Must be positive. See [stopping and upgrading](#stopping-and-upgrading).       |
| `max_connections`   | `256`          | `VYRTEL_SERVER_MAX_CONNECTIONS`   | Connections held open at once, across every listener. More wait up to 2 s for a slot (at most 64 wait), then are refused. 1 to 65536. See [connections and transport limits](#connections-and-transport-limits). |
| `header_timeout`    | `10s`          | `VYRTEL_SERVER_HEADER_TIMEOUT`    | How long a connection may take to send a complete request head, from when it is accepted or finishes a response. Also how long an idle keep-alive connection stays open. Positive, at most `1h`.              |
| `body_timeout`      | `1m`           | `VYRTEL_SERVER_BODY_TIMEOUT`      | How long a request body may take to arrive in full, from its head; then `408`. Positive, at most `1h`.                                                                                                       |
| `write_timeout`     | `30s`          | `VYRTEL_SERVER_WRITE_TIMEOUT`     | How long one write may stay blocked because the client is not reading; then the connection closes. Positive, at most `1h`.                                                                                    |
| `connection_buffer` | `64KB`         | `VYRTEL_SERVER_CONNECTION_BUFFER` | Cap on each connection's read buffer, which holds the request head (larger heads get `431`), and on its write buffer. `8KB` to `408KB`.                                                                       |

### `[storage]`

| Key                   | Default   | Env                                    | Description                                                                                                                                        |
|-----------------------|-----------|----------------------------------------|----------------------------------------------------------------------------------------------------------------------------------------------------|
| `path`                | `./data`  | `VYRTEL_STORAGE_PATH`                | Data directory. Everything persistent lives here.                                                                                                  |
| `max_memory`          | `512MB`   | `VYRTEL_STORAGE_MAX_MEMORY`          | Global memory budget (see below). Minimum 64MB; with a derived ingest limit, at least 219MB for 1MB requests ([ingest memory](#ingest-memory)).    |
| `segment_target_size` | `64MB`    | `VYRTEL_STORAGE_SEGMENT_TARGET_SIZE` | Seal the active segment at this *uncompressed in-memory* size (capped by the memory budget). Compressed segment files are typically 5–15× smaller. |
| `segment_max_age`     | `5m`      | `VYRTEL_STORAGE_SEGMENT_MAX_AGE`     | Seal the active segment after this long, even if small.                                                                                            |
| `durability`          | `normal`  | `VYRTEL_STORAGE_DURABILITY`          | `normal`: acknowledge after writing to the OS, fsync every `fsync_interval`. `strict`: fsync before every acknowledgement (group-committed). See [Crash recovery](#crash-recovery-and-backups). |
| `fsync_interval`      | `1s`      | `VYRTEL_STORAGE_FSYNC_INTERVAL`      | WAL fsync period in `normal` mode.                                                                                                                 |
| `compaction`          | `true`    | `VYRTEL_STORAGE_COMPACTION`          | Merge small segments (e.g. sealed by age on quiet systems).                                                                                        |

`storage.zstd_level`, `block_events` and `segment_max_events` were removed in
0.1.2: segments are always compressed at level 3 in blocks of up to 1024
events, and seal by `segment_target_size` and `segment_max_age`. A
configuration that still sets one is refused at startup, naming it.

### `[retention]`

| Key       | Default | Env                          |
|-----------|---------|------------------------------|
| `logs`    | `30d`   | `VYRTEL_RETENTION_LOGS`    |
| `traces`  | `14d`   | `VYRTEL_RETENTION_TRACES`  |
| `metrics` | `30d`   | `VYRTEL_RETENTION_METRICS` |

Use `"forever"` (or `0`) to keep data indefinitely. Retention deletes whole
segments whose newest event is older than the period; it runs every 30
seconds. Because segments are deleted as a unit, data may outlive the period
by up to one segment's time span.

### `[ingest]`

| Key                | Default | Env                                | Description                                                                                                      |
|--------------------|---------|------------------------------------|------------------------------------------------------------------------------------------------------------------|
| `max_request_size` | `1MB`   | `VYRTEL_INGEST_MAX_REQUEST_SIZE` | Maximum ingest body size, both after gzip decoding and on the wire (inclusive). Larger requests get 413; see [request size and encoding](api.md#request-size-and-encoding). Was `16MB` before 0.1.2. |
| `queue_capacity`   | `10000` | `VYRTEL_INGEST_QUEUE_CAPACITY`   | Events waiting to be written, per signal. When full, requests get 429. Also the most events (OTLP: log records, spans or metric data points) one request may carry: a larger batch gets 413 as soon as the count is known, before it is decoded ([decoding limits](#decoding-limits)). |
| `ack_timeout`      | `30s`   | –                                  | Give up waiting for a write acknowledgement (503).                                                               |
| `max_concurrent`   | `0`     | `VYRTEL_INGEST_MAX_CONCURRENT`   | Ingest requests in progress at once, from reading the body until the write is acknowledged. More get 429 without their body being read. `0` derives it from the memory budget ([ingest memory](#ingest-memory)); an explicit value is always honoured. |

Status codes and retry behaviour are in the
[API reference](api.md#ingest-limits-and-retries).

#### Ingest memory

Admission counts requests, not bytes, so the derived limit budgets every
admitted request as if it were a maximum-size one with a high-amplification
format. With `ingest.max_concurrent = 0`:

```
ingest budget   = 15% of storage.max_memory
per request     = 20 × max_request_size
fixed overhead  = 12.75 MB
max_concurrent  = floor((ingest budget − fixed overhead) ÷ per request), at most 1024
```

The defaults (512MB, 1MB requests) give `(76.8 MB − 12.75 MB) ÷ 20 MB = 3`
requests, an estimated 72.75 MB at most. `GET /api/v1/system/info` reports
the effective limit and the estimate under `ingest`
(`maxConcurrent`, `concurrencySource`, `budgetBytes`,
`estimatedPeakBytes`).

**What is counted.** Per admitted request: the body as it is read and
gzip-decoded, the parse working set and the parsed batch, which stays
admitted in the storage queue until its write is acknowledged. The fixed
overhead covers rejected bodies being drained so their clients read the 429
(at most 32 at once, each through its connection's read buffer; the
overhead assumes the largest `server.connection_buffer`, 408 KB, so it
holds whatever that is set to). The rest of the process is budgeted elsewhere and not counted
here: the WAL record and the active segment a batch is written into (the
active segments budget), batches still queued after their client went away
(bounded by `queue_capacity`), the buffers of connections that are not
draining a rejected body (the
[transport estimate](#connections-and-transport-limits)), and the runtime.

So the ingest budget bounds what admission owns; it is not a ceiling on
all memory used to receive data over the network. Every open connection has
buffers of its own, whether its request is admitted, rejected and waiting
to be drained (up to 256 are kept), or idle between keep-alive requests.
Those are bounded separately, by `server.max_connections` and
`server.connection_buffer`
([connections and transport limits](#connections-and-transport-limits)).

**Why 20× holds.** Decoding is limited so that no request goes past it,
whatever its shape ([decoding limits](#decoding-limits)): the decoders
may hold at most 17 × `max_request_size` of decoded data, and the rest
covers the body itself (held twice while a body read in parts is joined)
and what their accounting does not see. The `amplification` test measures
every fixture's peak heap per byte of decoded body and asserts it is
within 20×, with no exemptions
([details and full table](testing.md#ingest-amplification-and-derived-admission)).
Representative batches of native JSON, NDJSON and CLEF peak at 3.8–4.4×,
OTLP/JSON and OTLP/protobuf logs, traces and metrics at 8.4–11.9×, and the
highest accepted fixture (native events of ~24 bytes) at 15.6×; gzip makes
no measurable difference. Shapes that would decode past the limit are
refused part-way, at no more than 18.1×.

#### Decoding limits

Every ingest request is decoded within three limits, enforced while it is
parsed and never after: what the request has decoded so far is the only
memory at stake when a limit is reached.

| Limit                   | Value                                   | Over it                                                       |
|-------------------------|-----------------------------------------|---------------------------------------------------------------|
| Events per request      | `queue_capacity` (default 10,000)       | `413 payload_too_large`, `"limit": "maxEvents"`               |
| Decoded data per request | 17 × `max_request_size` (default 17 MB) | `413 payload_too_large`, `"limit": "maxDecodedBytes"`         |
| Nesting of values       | 32 levels                               | `400 invalid_payload`, `"limit": "maxDepth"`                  |

* **Events** are native events, OTLP log records, spans or metric data
  points. The count is taken before anything is built: native bodies are
  counted by a scan of their lines or array elements, OTLP/protobuf bodies
  by a scan of the wire format, and OTLP/JSON records and the events of a
  `{"events": [...]}` envelope whose key is escaped or repeated as they are
  reached. A batch the queue could never hold is refused as soon as its
  count is known, never after it has been parsed. An object is a batch
  only if `events` is its only key (repeated, the last one counts); an
  ordinary event with an `events` property is one event whatever the
  order of its keys, and is held to the decoded-data limit like any other.
* **Decoded data** is the decoders' own estimate of the heap they hold:
  the parsed JSON tree or OTLP structures, and the events built from them,
  each charged before or as it is allocated and credited back once freed.
  OTLP/protobuf bodies are measured by the same wire scan before `prost`
  decodes anything. It is what tells shapes apart that cost far more than
  their size: a 1 MB body of empty objects (`{}`), of tiny OTLP/protobuf
  log records, of properties nested several levels deep or of arrays of
  small numbers is refused, while ordinary batches use a fraction of it.
* **Nesting** counts objects and arrays inside one native event (the event
  is level 0), or levels of OTLP `arrayValue` / `kvlistValue` inside one
  attribute or body. OTLP key/value lists cost several levels each of the
  JSON parser's (128) or protobuf decoder's (100) own nesting limits, which
  are reached at about 30 levels and reported the same way.

OTLP resources are shared by all of their records, so a large resource
costs memory once per request rather than once per record.

A refused request stores nothing: native batches are all-or-nothing as
always, and an OTLP request over a limit is refused as a whole; it never
gets a partial success response, which stays for records that cannot be
represented. Its admission slot is released at once. The response names
the limit and how to split the batch
([API reference](api.md#ingest-limits-and-retries)); retrying the same
request cannot succeed.

Both 413 limits scale with your settings: raising `queue_capacity` admits
larger batches by count, and raising `max_request_size` raises the decoded
data allowance with it (and the per-request estimate, so fewer requests
are admitted at once from the same budget).

**Too little memory for one request.** If not even one maximum-size request
fits, Vyrtel refuses to start rather than admit more than the budget. For
example `vyrtel --memory-limit 128MB` exits with status 2 and:

```
vyrtel: configuration error: ingest budget too small: one 1 MB request needs an estimated 32.8 MB (20× the request size, plus 12.8 MB fixed ingest overhead) but the ingest share of storage.max_memory is 19.2 MB; increase storage.max_memory to at least 219MB, reduce ingest.max_request_size to at most 330KB, or set ingest.max_concurrent explicitly (see docs/configuration.md#ingest-memory)
```

At the 64MB minimum `max_memory` the ingest share is smaller than the fixed
overhead alone, so a derived limit is never valid there; set
`max_concurrent` explicitly.

**Explicit limits.** A positive `max_concurrent` is used as given, even
above 1024 or above the budget. When `max_concurrent × per request + fixed
overhead` exceeds the ingest budget, startup logs one warning with the
numbers, for example `VYRTEL_INGEST_MAX_CONCURRENT=8`:

```
WARN server::app: ingest.max_concurrent = 8 with ingest.max_request_size = 1 MB may need an estimated 172.8 MB of ingest memory under load, more than the 76.8 MB ingest budget; the limit is honoured. Lower ingest.max_concurrent or ingest.max_request_size, raise storage.max_memory, or set ingest.max_concurrent = 0 to derive it (see docs/configuration.md#ingest-memory) max_concurrent=8 max_request_bytes=1048576 ingest_budget_bytes=80530635 estimated_peak_bytes=181141504
```

Values above 2,305,843,009,213,693,951 (the admission semaphore's limit) are
rejected.

**Common configurations.** Derived limits before 0.1.2
(`ingest budget ÷ max_request_size`, at least 2) and now:

| `max_memory` | `max_request_size` | Ingest budget | Before | Now     | Estimated peak now |
|-------------:|-------------------:|--------------:|-------:|:--------|-------------------:|
|        64 MB |               1 MB |        9.6 MB |      9 | invalid |                  — |
|       128 MB |               1 MB |       19.2 MB |     19 | invalid |                  — |
|       256 MB |               1 MB |       38.4 MB |     38 | 1       |            32.8 MB |
|   **512 MB** |          **16 MB** |       76.8 MB |  **4** | invalid |                  — |
|       512 MB |               4 MB |       76.8 MB |     19 | invalid |                  — |
|   **512 MB** |           **1 MB** |       76.8 MB |     76 | **3**   |            72.8 MB |
|       512 MB |             256 KB |       76.8 MB |    307 | 12      |            72.8 MB |
|         1 GB |               4 MB |      153.6 MB |     38 | 1       |            92.8 MB |
|         1 GB |               1 MB |      153.6 MB |    153 | 7       |           152.8 MB |
|         2 GB |               4 MB |      307.2 MB |     76 | 3       |           252.8 MB |
|         2 GB |               1 MB |      307.2 MB |    307 | 14      |           292.8 MB |
|         4 GB |              16 MB |      614.4 MB |     38 | 1       |           332.8 MB |
|         8 GB |              16 MB |        1.2 GB |     76 | 3       |           972.8 MB |

Bold rows are the old and new defaults. The old limits ignored parsing
entirely; under the same overload test the old defaults peaked at 495–715
MB of ingest heap against the 76.8 MB budget
([results](testing.md#ingest-amplification-and-derived-admission)).

#### Upgrading to 0.1.2

* `max_request_size` now defaults to `1MB` (was `16MB`). Clients that send
  larger batches get `413 payload_too_large`: lower their batch size (for
  example the OpenTelemetry Collector's `send_batch_max_size`, or a Serilog
  sink's batch limit), or raise `max_request_size` together with
  `max_memory` (16MB requests need at least 2219MB for one derived slot).
* With `max_concurrent = 0`, fewer requests are admitted at once (3 instead
  of 4 at the defaults; see the table), so busy producers may see more
  `429`s. They are retryable with `Retry-After`.
* Configurations whose ingest budget cannot hold one request now fail at
  startup with the message above, instead of starting with 2 slots. This
  includes `max_memory` below 219MB with 1MB requests, and a `vyrtel.toml`
  copied from an older example that sets `max_request_size = "16MB"` with
  512MB. Fix it by raising `max_memory`, lowering `max_request_size`, or
  setting `max_concurrent` explicitly (which also restores the old
  behaviour exactly, with a startup warning if it is over budget).
* Explicit `max_concurrent` values behave exactly as before.
* Requests are now decoded within [decoding limits](#decoding-limits).
  Batches with more events than `queue_capacity` were already refused with
  413; they are now refused before they are parsed, and the error names
  the limit (`"limit": "maxEvents"`). Bodies that would decode into more
  than 17 × `max_request_size` (for example tens of thousands of empty or
  near-empty events, or deeply nested values) get `413` with
  `"limit": "maxDecodedBytes"`, and values nested more than 32 levels deep
  get `400` with `"limit": "maxDepth"`; both used to be accepted. Ordinary
  application, Serilog/CLEF and OpenTelemetry SDK and Collector batches are
  unaffected. The earlier advice to keep producers that might send such
  batches behind a limit in front of ingestion no longer applies.

### `[query]`

| Key                | Default | Env                           | Description                                                                                       |
|--------------------|---------|-------------------------------|---------------------------------------------------------------------------------------------------|
| `timeout`          | `30s`   | `VYRTEL_QUERY_TIMEOUT`        | Execution time of one request, not counting the wait for a slot (504 `query_timeout`). Must be positive. |
| `max_concurrent`   | `4`     | `VYRTEL_QUERY_MAX_CONCURRENT` | Requests executing at once. A cancelled query keeps its slot until it has stopped.                |
| `queue_timeout`    | `10s`   | –                             | How long a request waits for a free slot, then 503 `unavailable`. `0s` refuses at once when busy. |
| `max_scan_bytes`   | `4GB`   | –                             | Compressed segment bytes one request may read, across all its scans (422 `query_too_expensive`).  |
| `max_results`      | `1000`  | –                             | Maximum page size (`limit`). Must be positive.                                                    |
| `max_trace_spans`  | `50000` | –                             | Spans one trace may return from `GET /api/v1/traces/{id}` (422 `query_result_limit`). Must be positive. |
| `max_metric_names` | `10000` | –                             | Names `GET /api/v1/metrics` returns: the first by name, with `truncated: true` beyond. Must be positive. |
| `max_live_streams` | `64`    | –                             | Concurrent live-tail connections.                                                                 |

How the limits are enforced, where execution checks them, and what happens
when a client disconnects mid-query: [Execution limits and
cancellation](query-language.md#execution-limits-and-cancellation).
`/api/v1/system/info` reports `query.inFlight` and how many queries were
`rejected`, `cancelled`, `timedOut`, `tooExpensive` or `resultLimited`
since the process started.

#### Query budget changes in 0.1.2

* New keys `query.queue_timeout` (the previously fixed 10 s wait) and
  `query.max_trace_spans` (the previously fixed 50,000-span cap). The
  defaults keep the old values.
* `query.timeout` and `query.max_scan_bytes` now cover the whole request.
  Trace search pages and the facet sample each had their own budget before,
  so a trace search could run for up to 20 × `timeout` before failing.
* A scan that reads past `max_scan_bytes` now fails at that block. Before,
  the check ran only before each read, so the last block could go over.
* Queries are checked every 256 rows as well as at each block, including
  recent (unsealed) data, which had no checks before; a client
  disconnecting now stops its query (before, it ran to the end).
* Metric discovery (`GET /api/v1/metrics`) and facet counting now check
  the budget too: both stop on disconnect and at `timeout`. Discovery keeps
  at most the new `query.max_metric_names` (default 10,000) names, the
  first by name, and its response gains `truncated`. Before, it listed
  every distinct name with no limit on time or memory.
* A trace with more than `max_trace_spans` spans gets `422
  query_result_limit` instead of the first 50,000 spans presented as the
  whole trace. Trace search summaries gain `truncated`.
* A metric query needing more than 10,000 points per series gets `422
  query_result_limit` instead of series that silently stopped at the
  10,000th point.
* `query.timeout = 0`, `query.max_results = 0` and
  `query.max_trace_spans = 0` (and the new `max_metric_names = 0`) are
  refused at startup; before, they made
  every query fail.
* `504`, `422` and the new limit errors carry the limit (`limitMs`,
  `limitBytes`, `limit`). Codes and statuses of existing errors are
  unchanged; the 503 message for busy query slots now names the slot count.

### `[auth]`

| Key              | Default | Env                            | Description                                                                                                                                       |
|------------------|---------|--------------------------------|---------------------------------------------------------------------------------------------------------------------------------------------------|
| `ingest_api_key` | `false` | `VYRTEL_AUTH_INGEST_API_KEY` | Require an API key for ingestion (`/api/v1/events`, `/v1/*`). The UI and the rest of the API always require a login or an admin API key.            |
| `admin_username` | `admin` | `VYRTEL_AUTH_ADMIN_USERNAME` |                                                                                                                                                   |
| `admin_password` | –       | `VYRTEL_AUTH_ADMIN_PASSWORD` | Hashed (Argon2id) into the metadata store at startup. If unset on first start, a random password is generated and printed once.                  |
| `session_ttl`    | `7d`    | –                              | Login session lifetime.                                                                                                                           |

> **Security note.** The UI and API always need a login, but by default
> (`ingest_api_key = false`) anyone who can reach the port can write data.
> Set `ingest_api_key = true` before exposing Vyrtel beyond a trusted
> network, and put it behind TLS (a reverse proxy) — Vyrtel itself serves
> plain HTTP.

#### Authentication changes in 0.1.2

* `auth.enabled` is gone: the UI and API always require a login or an admin
  API key, and a fresh install generates and prints an admin password once.
  A configuration that still sets `auth.enabled` (or `VYRTEL_AUTH_ENABLED`)
  is refused at startup with what to do instead, so an upgrade never
  silently changes what it meant.
* Ingestion is configured on its own: `auth.ingest_api_key` (default
  `false`, open). An install that had `auth.enabled = true` must set it to
  `true` to keep requiring keys for ingestion.
* `GET /api/v1/auth/me` still returns `authEnabled`, now always `true`;
  `GET /api/v1/system/info` adds `ingestApiKey`.

### `[alerts]`

| Key               | Default | Env                       | Description                         |
|-------------------|---------|---------------------------|-------------------------------------|
| `enabled`         | `true`  | `VYRTEL_ALERTS_ENABLED` | Run the background alert evaluator. |
| `webhook_timeout` | `10s`   | –                         | Per-attempt webhook timeout.        |

### `[log]` — Vyrtel's own logs

| Key      | Default | Env                   | Description                                                               |
|----------|---------|-----------------------|---------------------------------------------------------------------------|
| `level`  | `info`  | `VYRTEL_LOG_LEVEL`  | `tracing` filter, e.g. `debug` or `info,storage=debug`.                   |
| `format` | `text`  | `VYRTEL_LOG_FORMAT` | `text` or `json`. Written to stderr; never ingested into Vyrtel itself. |

## Command line

```
vyrtel [OPTIONS] [COMMAND]

Commands:
  serve           Run the server (default)
  healthcheck     Exit 0 if the server answers /health (used by Docker)
  config          Print the effective configuration
  check-data-dir  Check, reading only, whether this version can start the
                  data directory and what starting it would change
                  (see Upgrading between pre-1.0 releases)

Options:
  -c, --config <PATH>         TOML config file
      --data-dir <PATH>       storage.path
      --bind <ADDR>           server.bind
      --memory-limit <SIZE>   storage.max_memory
      --log-level <FILTER>    log.level
      --log-format <FORMAT>   text | json
      --accept-incomplete-restore
                              start without missing parts of a restored data
                              directory (see Backup and restore)
  -h, --help
  -V, --version
```

## Environment variables

The `Env` column of each table above lists every setting Vyrtel reads from
the environment; every other setting is
set in the TOML file. A variable under a section's prefix
(`VYRTEL_SERVER_`, `VYRTEL_STORAGE_`, `VYRTEL_RETENTION_`,
`VYRTEL_INGEST_`, `VYRTEL_QUERY_`, `VYRTEL_AUTH_`, `VYRTEL_ALERTS_`,
`VYRTEL_LOG_`) that is not one of them stops Vyrtel at startup (exit
status 1) instead of being ignored, so a misspelt `VYRTEL_AUTH_INGEST_API_KEY`
never starts a server with a setting other than the one meant:

```text
vyrtel: unknown environment variable VYRTEL_AUTH_ENABLE: not a setting Vyrtel reads from the environment. Remove it, or set the option in the configuration file instead (see docs/configuration.md#environment-variables for the 28 supported variables)
```

Other `VYRTEL_*` names, such as those the test and release scripts use,
are not settings and are ignored. `OBSERVER_*` variables (Observer 0.1.0)
have not been read since 0.1.1.

## Memory budget

`max_memory` is split between the consumers that hold data in memory. Each
budget is enforced by its owner; nothing grows without bound.

| Consumer        | Share | Enforcement                                                                                                                                                                                                                                         |
|-----------------|------:|-----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| Active segments |   40% | Split logs 50% / traces 30% / metrics 20%. Each signal seals at the smaller of `segment_target_size` and half its share (the active buffer plus one buffer being sealed must fit). If sealing falls behind, writers wait and ingestion returns 429. |
| Index cache     |   15% | LRU of segment index regions (Bloom filters, zone maps); evicts when full. Segment summaries are always resident and small.                                                                                                                         |
| Ingest          |   15% | Unless `ingest.max_concurrent` is set, admits `(share − 12.75 MB) ÷ (20 × max_request_size)` requests at once (at most 1024; startup fails if that is 0), each from body read to acknowledgement; more get 429 ([ingest memory](#ingest-memory)). Event queues are bounded by `queue_capacity`. |
| Queries         |   20% | `max_concurrent` queries; each holds at most one page of results plus one decoded block at a time.                                                                                                                                                  |
| Background      |   10% | Compaction input is capped at a third of this share.                                                                                                                                                                                                |

Live tail keeps a ring of the last 64 committed batches for slow clients;
clients that fall further behind are told how many batches they missed.

Connections are budgeted outside `max_memory`, by their own limits; see
[connections and transport limits](#connections-and-transport-limits).

## Connections and transport limits

Vyrtel bounds what clients can make it hold before any request is routed,
authenticated or admitted, so idle, slow and excessive connections cannot
exhaust memory, tasks or file descriptors ahead of ingest and query
admission. These limits are enforced by the server itself; a reverse proxy
in front is optional extra protection, not a requirement.

| Setting                    | Default | Bounds                                                                                                                         |
|----------------------------|--------:|--------------------------------------------------------------------------------------------------------------------------------|
| `server.max_connections`   |   `256` | Connections holding a slot at once. 1 to 65536.                                                                                |
| (fixed) waiting            |    `64` | Connections waiting for a slot at once, each for at most 2 s.                                                                  |
| `server.header_timeout`    |   `10s` | From accept, or from the end of a response, until the next request head is complete. Ends idle keep-alive connections too.     |
| `server.body_timeout`      |    `1m` | From a request head until the end of its body.                                                                                 |
| `server.write_timeout`     |   `30s` | One write blocked because the client is not reading.                                                                           |
| `server.connection_buffer` |  `64KB` | Bytes in each connection's read buffer (request head plus any pipelined requests) and in its write buffer. `8KB` to `408KB`. |
| (fixed) headers            |   `100` | Headers in one request head.                                                                                                   |

Every value is finite: zero, values out of range and values that overflow
(for example a duration of `18446744073709551615w`) are refused at startup,
so no setting turns a limit off. Durations need a unit or are seconds;
`1h` is the longest. Settings are read at startup only; there is no reload.

**One budget per process.** Vyrtel listens on one address, `server.bind`.
The limits are owned by the process, not the listener: if a build serves
more than one listener, all of them share the same slots and waiting places,
so the aggregate is bounded by the same numbers. `GET /api/v1/system/info`
reports them under `connections`
([fields](api.md#system)); the startup banner shows `max_connections` and
the buffer estimate.

### What happens to a connection

1. **Accepted.** The kernel completes the TCP handshake and queues the
   connection in the listen backlog; Vyrtel accepts from it continuously,
   whatever the connection's state. The backlog is 1024 (also capped by the
   kernel's `net.core.somaxconn`, 4096 by default on Linux 5.4 and later).
   It is the kernel's, not Vyrtel's: a burst of connections faster than
   Vyrtel accepts (1,000 at once on 2 CPUs, in the Linux runs) or running
   out of file descriptors overflows it. Linux then drops the handshake
   (`ListenOverflows` in `/proc/net/netstat`) unless
   `net.ipv4.tcp_abort_on_overflow = 1`. With SYN cookies, the default, the
   client may believe it is connected while the server has no record of it:
   a client that sends data gets a reset, an idle one waits until its own
   timeout. Vyrtel holds nothing for such connections. Clients should use
   connect and response timeouts, as the OpenTelemetry exporters do.
2. **Admitted or waiting.** With a slot free, the connection takes it before
   any task is spawned for it or any buffer is allocated. Otherwise it waits
   for one, holding only its socket (no buffers), at most 64 at a time and
   each for at most 2 s, first come first served. A slot frees as soon as a
   connection ends.
3. **Refused.** With no slot after 2 s, or 64 already waiting, Vyrtel tries
   one non-blocking write of a fixed `503 server_busy` response
   ([format](api.md#connections)) and closes the socket. It reads nothing,
   allocates nothing and does not wait. If the client had already sent its
   request, closing with unread data makes the kernel send a reset, which
   can arrive before the 503. So a client may see the 503, a closed
   connection or a reset, and should treat all three alike: retry with
   backoff. No readable 503 is promised before HTTP is parsed.
4. **Served.** HTTP/1.1, one request at a time. Pipelined requests are
   answered in order; the read buffer holds at most `connection_buffer`
   bytes of them and the rest wait in the kernel (TCP flow control). Ingest
   admission (`ingest.max_concurrent`), query admission
   (`query.max_concurrent`) and live-tail streams (`query.max_live_streams`)
   apply on top, unchanged.
5. **Closed.** At EOF, a reset, a protocol error, a deadline below, the end
   of a `Connection: close` exchange, or shutdown. Its slot, buffers, task
   and file descriptor are released whichever way it ends.

| Deadline or limit                        | Outcome                                                                                       |
|------------------------------------------|-----------------------------------------------------------------------------------------------|
| No complete head within `header_timeout` | Closed without a response (counted in `headerTimeouts`). Idle keep-alive connections end so. |
| Head over `connection_buffer`            | `431 Request Header Fields Too Large`, closed (`protocolErrors`).                             |
| More than 100 headers                    | `431`, closed (`protocolErrors`).                                                             |
| Body not complete within `body_timeout`  | `408 request_timeout`, closed (`bodyTimeouts`). Any ingest slot it held is released.          |
| A write blocked for `write_timeout`      | Closed, the response cut off (`writeTimeouts`).                                               |
| Malformed head, HTTP/2 preface           | `400` or closed (`protocolErrors`).                                                           |

A request that is being handled (a query running, an ingest write waiting
for its acknowledgement) is bounded by its own limits (`query.timeout`,
`ingest.ack_timeout`), not by these deadlines.

**Protocols.** HTTP/1.1 only. HTTP/2 (prior knowledge or `h2c` upgrade),
WebSockets and other upgrades are not served, so HTTP/2 stream multiplexing
does not apply. TLS is not terminated by Vyrtel.

**Live tail.** A live-tail stream (Server-Sent Events) holds its connection
slot for as long as it is open; there is no maximum duration. At most
`query.max_live_streams` (64) are open at once, so keep that below
`max_connections` (Vyrtel warns at startup otherwise) to leave slots for
ingest and queries. Streams send a keep-alive comment every 15 s, so
`header_timeout` and `write_timeout` do not end a stream whose client
reads. A client that stops reading is closed once a write stays blocked for
`write_timeout`; that can take a while, because the kernel's socket buffers
fill first. Until then the server holds at most `connection_buffer` of
events for it; it falls behind and is told how many batches it missed
(`lagged`) rather than having them queued. At shutdown a stream sends what
it has already matched, then `shutdown`.

### Memory accounting

Measured with the counting allocator in `tests/overload/transport.rs`
(release build, Linux x86_64 and Windows 11 alike, within 1%):

| Connection state                                      | Heap per connection (64 KB buffer) |
|-------------------------------------------------------|-----------------------------------:|
| Idle (accepted, nothing sent)                         |                              18 KB |
| Request head filling the read buffer, never finished |                             122 KB |
| Pipelining requests, never reading responses          |                              48 KB |
| Waiting for a slot (estimated; too small to separate) |                          under 2 KB |
| Closed                                                |                                  0 |

A read buffer that grows to its limit can have up to twice the limit
allocated, so the estimate Vyrtel reports (`estimatedPeakBytes`, and the
banner) is:

```
per connection = 3 × connection_buffer + 16 KB
transport peak = max_connections × per connection + 64 × 2 KB
```

That is 52.1 MB at the defaults (256 × 208 KB) and 4.1 MB with
`max_connections = 64` and `connection_buffer = "16KB"`. It is on top of
`storage.max_memory`, and is reached only if every slot holds a full read
buffer at once; typical idle connections cost 18 KB each. Request and
response bodies are not part of it: ingest admission and the query budgets
bound those.

**Kernel memory.** Each socket also has kernel send and receive buffers,
which are neither heap nor counted in RSS, but are charged to a container's
memory limit (the `sock` line of its cgroup `memory.stat`). Linux grows them
up to `net.ipv4.tcp_rmem` and `net.ipv4.tcp_wmem` (6 MB and 4 MB at most by
default) for connections that move data quickly, so many connections
uploading or downloading at once can hold far more kernel memory than
heap: in the overload test, 200 connections that sent requests without
reading responses held about 1 GB of socket buffers, client and server
sides together, against 24 MB of server heap. On a small host, lower the
maximums (for example `sysctl -w net.ipv4.tcp_rmem="4096 131072 1048576"
net.ipv4.tcp_wmem="4096 16384 1048576"`) and leave headroom in the container
limit, or lower `max_connections`.

**File descriptors.** Vyrtel needs one per connection (slots and waiting
places, `max_connections + 64`), about 25 for the listener, the runtime, the
WAL and the metadata database, and one per segment file it has open, which
grows with the data kept (storage keeps each sealed segment open for reads).
In the 30-minute Linux soaks, segment files added about 120 (normal) and
190 (constrained, smaller segments) descriptors. So allow
`max_connections + 64 + 25 + segments`, with room for growth: 1024 covers
the defaults for a modest data directory; size it from
`ls /proc/$(pidof vyrtel)/fd | wc -l` on a running instance. Raise
`ulimit -n` (systemd `LimitNOFILE=`, Docker `--ulimit nofile=`) if you raise
`max_connections`, and check it where Vyrtel actually runs: many systems
default to 1024.
Running out shows as `acceptErrors` and a `cannot accept connections`
warning; Vyrtel pauses accepting for 250 ms and retries.

### Choosing the limits

The defaults come from the measurements above and the Linux runs in
[`docs/benchmarks/transport`](benchmarks/transport/2026-10-17-wsl2/README.md):

* **`max_connections = 256`.** OpenTelemetry SDKs and Collectors, log
  shippers and browsers reuse a handful of keep-alive connections each, so
  256 serves dozens of producers and UI users. At 18 KB each, 256 idle
  connections cost 4.5 MB; the worst case is the 52.1 MB above.
* **`connection_buffer = 64KB`.** Larger than the request heads common
  proxies accept (8 to 32 KB), and ingest throughput is unchanged from the
  previous 408 KB limit (see the steady-load comparison in the report). A
  smaller buffer lowers the worst case linearly.
* **`header_timeout = 10s`.** Ends idle keep-alive connections and slow
  heads quickly enough that a flood of idle connections turns over every
  10 s, while still allowing clients on slow links.
* **`body_timeout = 1m`.** A 1 MB ingest body (the default
  `max_request_size`) at 17 KB/s. An admitted ingest body holds its ingest
  slot until it arrives or this passes: in the Linux floods, bodies stalled
  on purpose held all 3 default ingest slots for the full minute while other
  producers got `429`. Where untrusted clients can reach ingest and uploads
  are fast, lower it (for example `10s`).
* **`write_timeout = 30s`.** A client that has not read for 30 s while its
  socket buffers are full has gone away.

**A low-memory example.** A 384 MiB container:

```toml
[server]
max_connections = 64          # 64 × (3 × 16 KB + 16 KB) + 128 KB = 4.1 MB at most
connection_buffer = "16KB"    # heads up to 16 KB

[storage]
max_memory = "256MB"          # ingest derives 1 slot for 1 MB requests (see ingest memory)

[query]
max_live_streams = 8          # below max_connections
```

The same limits from the environment (`max_live_streams` has no variable,
so Vyrtel warns that it is not below `max_connections`):

```bash
docker run --memory 384m --ulimit nofile=1024 -p 8080:8080 \
  -e VYRTEL_STORAGE_MAX_MEMORY=256MB -e VYRTEL_SERVER_MAX_CONNECTIONS=64 \
  -e VYRTEL_SERVER_CONNECTION_BUFFER=16KB ghcr.io/rowan-smith/vyrtel
```

With waves of 1,000 simultaneous connection attempts every minute for 30
minutes, alongside steady ingest and queries, this configuration held at
most 64 connections and 64 waiting, peaked at 92 MiB RSS (130 MiB for the
whole container, kernel buffers and page cache included) and settled at
77 MiB afterwards, with every acknowledged event stored
([report](benchmarks/transport/2026-10-17-wsl2/README.md)).

**Behind a reverse proxy or load balancer.** Vyrtel then sees the proxy's
upstream connections, not the clients': set `max_connections` at least as
large as the proxy's upstream connection pool, and the proxy's upstream
idle timeout below `header_timeout` (nginx: `keepalive_timeout 5s;` in the
`upstream` block), or raise `header_timeout` above it. Otherwise Vyrtel may
close an idle upstream connection just as the proxy reuses it, and the
proxy answers `502`. Proxy limits on clients (rate, per-IP connections,
TLS) apply first and complement these.

**Not covered.** Fairness between clients or tenants (one client can take
every slot, each for up to `header_timeout` at a time), bandwidth limits,
and defence against distributed floods. Use network-level controls for
those. Rejected ingest bodies are drained with their own limits
([ingest limits](api.md#ingest-limits-and-retries)).

#### Connection limit changes in 0.1.2

* Connections are now limited (`server.max_connections`, default 256);
  before, every connection was accepted and kept. Beyond the limit,
  connections wait up to 2 s, then are refused with `503 server_busy` or
  closed. Deployments with more than 256 concurrent clients that each hold
  a connection open, or a proxy with a larger upstream pool, should raise it.
* Idle keep-alive connections close after `server.header_timeout` (10 s);
  before, they stayed open until the client closed them. Clients reconnect
  transparently; a proxy whose upstream idle timeout is longer may log
  occasional `502`s until it is set below 10 s.
* Request heads larger than 64 KB get `431` (before: about 408 KB), slow
  bodies get `408 request_timeout` after 1 minute (before: no limit), and
  connections whose client stops reading for 30 s close (before: no limit).
* A live-tail client that is not reading is buffered for less (64 KB rather
  than about 408 KB), so it skips more events; it reconnects as before.
* `GET /api/v1/system/info` has a new `connections` object, and the startup
  banner a `Connections` line.

## Docker and bind mounts

The image (`ghcr.io/rowan-smith/vyrtel`, `linux/amd64` and `linux/arm64`)
runs as the unprivileged `nonroot` user, uid/gid 65532, and keeps all data
in `/data` (`VYRTEL_STORAGE_PATH=/data`, declared as a volume). Each image
carries its version in the `org.opencontainers.image.version` label:

```bash
docker image inspect --format '{{.Config.User}} {{index .Config.Labels "org.opencontainers.image.version"}}' ghcr.io/rowan-smith/vyrtel:0.1.2
# nonroot:nonroot 0.1.2
```

Pin a version tag (`0.1.2`, or `0.1` for patch updates) in production
rather than `latest`, so a recreated container never upgrades by surprise
([upgrading](#stopping-and-upgrading)).

**Named volumes** work out of the box: a new volume copies the image's
`/data`, owned by 65532. The data lives in the volume, not the container,
so stopping, removing and recreating the container (for example to change
an environment variable or upgrade the image) keeps every acknowledged
event, dashboard, alert and API key:

```bash
docker run -d --name vyrtel -p 8080:8080 -v vyrtel-data:/data ghcr.io/rowan-smith/vyrtel:0.1.2
docker stop vyrtel && docker rm vyrtel
docker run -d --name vyrtel -p 8080:8080 -v vyrtel-data:/data ghcr.io/rowan-smith/vyrtel:0.1.2
```

Without `-v`, Docker creates an anonymous volume per container, and
recreating the container starts with an empty one.

**Bind mounts on Linux** keep the host directory's owner and mode, and a
directory that `docker run` creates for a missing path is owned by root.
Give the directory to uid/gid 65532 first:

```bash
mkdir -p vyrtel-data && sudo chown -R 65532:65532 vyrtel-data
docker run -d --name vyrtel -p 8080:8080 -v ./vyrtel-data:/data ghcr.io/rowan-smith/vyrtel:0.1.2
```

If the directory is not writable, Vyrtel exits with status 1 before
creating anything and says why:

```text
vyrtel: locking the data directory: I/O error on /data/.lock: Permission denied (os error 13): Permission denied (os error 13)
hint: the data directory /data must be writable by the user running Vyrtel.
      Vyrtel runs as uid 65532 (gid 65532); /data is owned by uid 0 (gid 0) with mode 555.
      The Docker image runs as uid/gid 65532. Named volumes get that owner automatically;
      for a bind mount, give the host directory to that user first, e.g.
        sudo chown -R 65532:65532 ./vyrtel-data
      See docs/configuration.md#docker-and-bind-mounts.
```

Run the `chown` on the host directory, then start the container again.
`-R` matters when the directory already holds files written by another
user, for example by an older container run as root. The same hint is
printed outside Docker, naming the user Vyrtel runs as, and by
`vyrtel check-data-dir` when it cannot read the directory
(`check-data-dir` only reads, so it does not detect a directory that is
readable but not writable). A read-only volume or mount (`:ro`) is refused
too, with `mount it read-write (no :ro)` in place of the `chown` advice.

Docker Desktop on macOS and Windows does not enforce host ownership on bind
mounts, so no `chown` is needed there. On SELinux hosts, also add `:Z` to
the mount (`-v ./vyrtel-data:/data:Z`). Running the image as another user
(`--user`) is possible but the directory must then be writable by that
user. Rootless Docker, user-namespace remapping, Kubernetes and Windows
containers are not tested.

Every release runs these steps on amd64 and arm64 before the image is
published ([container image checks](releasing.md#container-image-checks)).

#### Docker changes in 0.1.2

* Images carry `org.opencontainers.image.version` (and title, description
  and licence labels). Images you build yourself are labelled `dev` unless
  you pass `--build-arg VYRTEL_VERSION=<version>`.
* The permission hint names the directory, its owner and mode, the uid
  Vyrtel runs as and the exact `chown -R 65532:65532` fix, and a read-only
  mount gets its own advice. Before, it was one line pointing at this page.
  The exit status (1) is unchanged.
* Nothing else changes for existing deployments: the user (65532), `/data`,
  the port and the healthcheck are as before, so existing named volumes and
  correctly owned bind mounts keep working with the new image.

## Stopping and upgrading

Stop Vyrtel with a stop signal, then start the new version on the same data
directory. Nothing has to be flushed or exported by hand.

| Platform | Stop signals                                                                 | Sent by                                                  |
|----------|------------------------------------------------------------------------------|----------------------------------------------------------|
| Linux, macOS | `SIGTERM`, `SIGINT` (Ctrl+C)                                             | `docker stop`, `systemctl stop`, Kubernetes, `kill`      |
| Windows  | Ctrl+C, Ctrl+Break, closing the console window, system shutdown              | the console, service wrappers such as NSSM or WinSW      |

### What happens

From the first stop signal, Vyrtel:

1. **Refuses new work.** The listener closes, so new connections are
   refused. A request that still arrives on an open keep-alive connection
   gets `503 shutting_down` with `Retry-After: 1` and `Connection: close`
   (ingest, queries and live tail; the body is not read). `/ready` returns
   `503 {"status": "draining"}` so a load balancer stops routing to it;
   `/health` stays `200` until exit.
2. **Ends live-tail streams.** Each stream sends the events it has already
   matched, then a `shutdown` event with `retry: 2000`, and ends normally.
   `EventSource` clients reconnect after two seconds. Browsers give up if a
   reconnect is answered with an HTTP error (a proxy's `503`, say), so the
   web UI then opens a new stream itself, backing off from 1 to 30 seconds;
   it shows *connecting* meanwhile and *live* again once reconnected.
3. **Drains accepted work.** Requests already admitted run to their
   response: each ingest request is written to the WAL, acknowledged and
   answered (`200`), and queries in progress complete.
4. **Flushes storage.** Each signal writes out its queue and fsyncs its
   WAL. Unsealed data is not sealed into segments; the next start replays
   it from the WAL. If this final fsync fails, the shutdown is not clean
   (see below).
5. **Stops background jobs** (alert evaluation) and saves query
   statistics, then lets a seal or compaction already in progress finish.
6. **Records the inventory** of WAL and segment files in `vyrtel.json`,
   which a [restored backup](#backup-and-restore) is checked against.
7. **Exits with status 0**, after logging `shutdown complete`.

All of this shares one deadline, `server.shutdown_timeout` (default `8s`),
counted from the first signal. An idle server stops in milliseconds; a
request waiting for its acknowledgement holds the drain for at most
`ingest.ack_timeout`, and a slow upload for as long as the client takes to
send it.

### When the deadline passes, the signal is repeated, or the flush fails

Status 0 means every step above finished. Otherwise Vyrtel logs an
`ERROR`, prints the same text to stderr and exits with status **1**, in
every step alike: when the deadline passes, at once on a second stop
signal (Ctrl+C twice, `kill -TERM` twice), when the final WAL fsync
fails, and when the inventory cannot be written:

```
vyrtel: shutdown incomplete: server.shutdown_timeout (1s) passed while waiting for in-flight requests; unfinished: 1 ingest request not acknowledged (the client gets no response and should retry). Acknowledged events are already in the WAL and are replayed on the next start; raise server.shutdown_timeout if draining regularly takes longer
```

The message names the step that did not finish: `waiting for in-flight
requests`, `storage was writing queued batches and flushing the WAL`,
`background jobs were stopping and query statistics were being saved`,
`a seal or compaction was finishing`, or `the data directory inventory was
being recorded`. The last three say every accepted request was already
written and flushed; an interrupted seal is redone from the WAL on the next
start, and without an inventory a backup of the directory cannot be checked
for missing files (start and stop Vyrtel again before backing it up).
A failed inventory write is reported the same way, with the error:

```
vyrtel: shutdown incomplete: every accepted request was written and the WAL flushed, but the data directory inventory could not be recorded (No space left on device (os error 28)). The directory starts normally, but a backup of it cannot be checked for missing or truncated files when it is restored; fix the storage problem, then start and stop Vyrtel again before backing it up (docs/configuration.md#backup-and-restore)
```
 The message then lists only the kinds of work
that were unfinished:

| Unfinished work                   | What it means                                                                                                                                                                                                      |
|-----------------------------------|--------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| `N ingest requests not acknowledged` | Their clients get no response, only a closed connection. Retry them: each batch is all-or-nothing, but one already written to the WAL just before the exit is replayed, so a retry can duplicate it (as after a `503 unavailable`). |
| `N queued events not written (logs N, …)` | Events of those requests that were still waiting for the writer, per signal.                                                                                                                                 |
| `N live-tail streams still open`   | A client stopped reading and its connection's buffers are full, so its stream could not end. Nothing is lost.                                                                                                     |

**Acknowledged events are never affected.** A request is acknowledged only
after its batch is in the WAL, so the next start replays it whether the
drain finished or not (in `normal` durability an OS crash or power loss
straight after exiting can still lose up to `fsync_interval`, exactly as
for a running server; see [crash recovery](#crash-recovery-and-backups)).
After an incomplete shutdown, start Vyrtel as usual; recovery may report a
`walTail` quarantine if the process exited while writing a record.

**A failed final flush** (full disk, I/O error) is reported with the fault
for each affected signal, as `GET /api/v1/system/storage` would show it:

```
vyrtel: shutdown incomplete: the final WAL flush failed (logs: walSync noSpace on wal/logs/000000000001.wal: No space left on device). Events acknowledged since the last successful fsync reached the operating system but may not be on disk: the next start replays them, but an OS crash or power loss before the OS writes them out can lose them; fix the storage problem (docs/configuration.md#storage-failures)
```

This only matters in `normal` durability (`strict` fsyncs before every
acknowledgement). The process stopped normally, so nothing is lost unless
the host crashes before the OS writes its cache out; fix the volume as for
any [storage failure](#storage-failures) before relying on the data.

If drains regularly hit the deadline, raise `server.shutdown_timeout` (and
the orchestrator's grace period with it), or look for clients holding
uploads or live-tail streams open.

### Grace periods

The orchestrator must wait longer than `server.shutdown_timeout` before it
force-kills the process, or the diagnostic is never printed. Leave at least
two seconds of margin:

| Runs under                 | Default grace period                 | With a longer `shutdown_timeout`                                      |
|----------------------------|--------------------------------------|-----------------------------------------------------------------------|
| `docker run` / `docker stop` | 10 s                                | `docker run --stop-timeout 35 …` or `docker stop -t 35 vyrtel`        |
| Docker Compose             | 10 s                                 | `stop_grace_period: 35s` on the service                               |
| Kubernetes                 | 30 s (`terminationGracePeriodSeconds`) | raise `terminationGracePeriodSeconds`                               |
| systemd                    | 90 s (`TimeoutStopSec`)              | usually enough                                                        |

The default of `8s` fits inside Docker's 10 seconds. On Windows, closing
the console window or shutting the system down gives a process only a few
seconds (about 5 for a closed console) before Windows terminates it
whatever it is doing; acknowledged events are still safe, but prefer Ctrl+C
or Ctrl+Break, and configure service wrappers to send one of them and wait
longer than `shutdown_timeout`.

### Upgrade runbook

1. Stop the running version: `docker stop vyrtel`, `systemctl stop vyrtel`,
   or Ctrl+C. If you run more than one instance behind a load balancer,
   `/ready` turns `503` as soon as the drain starts.
2. Check the last lines it logged: `shutdown complete` and exit status 0
   means everything accepted was acknowledged. `shutdown incomplete` (exit
   status 1) lists what was not; see above.
3. Check that the new version can read the data directory, with the new
   binary or image and before starting it:
   `vyrtel check-data-dir --data-dir /var/lib/vyrtel/data`, or
   `docker run --rm -v vyrtel-data:/data ghcr.io/rowan-smith/vyrtel:0.1.2 check-data-dir`.
   It changes nothing. Exit status 1 means the new version cannot start it:
   before 1.0 that is the case whenever the new release has another layout
   (0.1.2 reads no directory an earlier release wrote). Stay on the current
   version, or follow
   [Upgrading between pre-1.0 releases](#upgrading-between-pre-10-releases)
   to start the new one on a new data directory. Take a
   [backup](#back-up-a-stopped-server) of the stopped directory either way.
4. Replace the binary or image (`docker pull`, new package) and read the
   release notes for configuration changes, such as
   [Upgrading to 0.1.2](#upgrading-to-012).
5. Start it on the same data directory (or, for a new layout, on the new
   one). Each signal logs a `recovered stream` line with the WAL records it
   replayed. After a clean stop, `GET /api/v1/system/storage` shows an
   empty `recovery.quarantined` list.
6. Check `GET /ready` is `200` and that recent data is queryable.

Zero-downtime upgrades of a single process are not supported: between the
old process exiting and the new one listening, connections are refused.
Producers should buffer and retry (the OpenTelemetry SDKs and Collector do).

**Example** (Linux, from the repository root, using the shared fixture):

```bash
vyrtel --data-dir ./data &
curl -s -X POST localhost:8080/api/v1/events \
  -H 'Content-Type: application/x-ndjson' --data-binary @tests/fixtures/logs.ndjson
# {"accepted":8}
kill -TERM $!; wait $!; echo "exit $?"
# exit 0   (stderr: "stop requested; draining accepted work for up to 8s …", "shutdown complete")
vyrtel --data-dir ./data &
curl -s -X POST localhost:8080/api/v1/query/logs/count \
  -H 'Content-Type: application/json' -d '{"query": ""}'
# {"count":8,"diagnostics":{…}}
```

The integration tests in `tests/integration/shutdown.rs` reproduce the
deadline and repeated-signal cases by holding an upload open.

#### Shutdown changes in 0.1.2

* Shutdown now has a deadline, `server.shutdown_timeout` (default `8s`).
  Before, Vyrtel waited for in-flight requests without limit, so a stuck
  upload ended only when the orchestrator killed the process, with no
  record of what was unfinished. If your drains need longer, raise it
  together with the grace period.
* A shutdown that does not finish in time, is interrupted by a second
  stop signal, or whose final WAL fsync fails exits with status **1** and
  an `ERROR` line starting `shutdown incomplete:`. Before, a failed final
  fsync was only logged and the exit status was 0. Supervisors that treat
  a non-zero exit as a crash may restart or alert on it; that is intended.
* Ingest, query and live-tail requests that reach a stopping server get
  `503 shutting_down`, and `/ready` returns `503 {"status": "draining"}`.
* Live-tail streams end with a `shutdown` event (`retry: 2000`) instead of
  just closing. Clients that ignore unknown event types are unaffected.
* A second stop signal now ends the drain at once; before, it was ignored.
* On Windows, Ctrl+Break, closing the console and system shutdown now stop
  Vyrtel gracefully too; before, only Ctrl+C did.

## Crash recovery and backups

Vyrtel needs no repair step after a crash: start it again on the same data
directory. Startup replays the write-ahead log, finishes any interrupted
sealing or compaction and logs one `recovered stream` line per signal.

**What survives** (full table in the
[acknowledgement contract](storage-format.md#acknowledgement-contract)):

| Durability | Process killed / crashed            | OS crash or power loss                              |
|------------|-------------------------------------|-----------------------------------------------------|
| `normal`   | every acknowledged request survives | up to `fsync_interval` (1 s) of acknowledged requests may be lost |
| `strict`   | every acknowledged request survives | every acknowledged request survives                 |

Either way each surviving event is visible exactly once with its original
id, and a request is never half-applied. Choose `strict` when a lost second
of telemetry on power failure is unacceptable; it costs one fsync per group
of concurrent requests.

**Checking a recovery.** After an unclean stop, look for these at startup:

* `WARN recovery found damaged files; see the quarantine directory` — damage
  was found. Each damaged item has its own `ERROR`/`WARN` line with
  `signal`, `kind`, `reason` and `quarantined_to`.
* `GET /api/v1/system/storage` → `signals.<signal>.recovery` lists the same
  items (`quarantined`) and how much was replayed.
* A startup that **fails** with an I/O error (permission denied, device
  errors) is deliberate: Vyrtel refuses to set aside files it merely could
  not read. Fix the permissions or disk and start again; nothing has been
  changed.

**Quarantine.** Damaged files are moved to `data/quarantine/<signal>/` and
never deleted by Vyrtel. A torn final WAL record after a crash (`walTail`)
is normal after power loss in `normal` mode and loses only that record;
whole `segment` or `wal` items mean the disk or something outside Vyrtel
altered the files. See [Quarantine](storage-format.md#quarantine) for what
each kind lost. Copy the files somewhere for diagnosis if needed, then
delete them; the directory's size is reported as `quarantineBytes`.

**Backups.** See [Backup and restore](#backup-and-restore).

### Recovery health

Every start reports, per signal, what recovery found and what is in
quarantine. Signed-in users see it in three places:

* **Settings → Storage** shows a warning above the storage totals when any
  signal is not `clean`: which signals may be incomplete, how many bytes of
  WAL were discarded, how many items were quarantined this start and how
  many files are in `quarantine/<signal>/` now (or why it could not be
  read), with each item's kind, reason and quarantine file name. After a
  clean start nothing is shown.
* `GET /api/v1/system/storage` → top-level `recovery` (overall status and
  the affected signals) and `signals.<signal>.recovery` (counts, discarded
  and quarantined bytes, the quarantine directory and each item). See
  [System](api.md#system).
* `GET /api/v1/system/info` → `recovery`, the same overall status.

| Status               | Meaning                                                                                                   | What to do                                                                 |
|----------------------|-----------------------------------------------------------------------------------------------------------|----------------------------------------------------------------------------|
| `clean`              | Nothing was discarded or quarantined, and `quarantine/<signal>/` is empty. Replaying the WAL is normal and does not change this. | Nothing.                                                      |
| `damaged`            | This start quarantined a segment, a WAL file or a WAL tail, or discarded WAL bytes.                       | Read the reasons; see [Quarantine](storage-format.md#quarantine) for what each kind lost. |
| `unknown`            | This start found nothing new, but `quarantine/<signal>/` (or something in it) could not be read, so whether earlier evidence is there is not known. `quarantineDir.error` gives the reason. | Make the directory readable by the user Vyrtel runs as; the status updates on the next request, no restart needed. |
| `quarantineRetained` | This start found nothing new, but `quarantine/<signal>/` still holds files from an earlier start, including one whose recovery was itself interrupted after setting a file aside. | Copy the files off if you need them, then delete them.                     |

The overall status is the worst of the three signals (`damaged`, then
`unknown`, then `quarantineRetained`, then `clean`). Not being able to look
is never reported as `clean`. A missing `quarantine/<signal>/` is `clean`:
it is created only when recovery first sets something aside.

**Limitations.** Vyrtel does not repair quarantined data or guess what it
held. Events in a quarantined segment come back only if the WAL it was
sealed from still existed; events in a quarantined WAL file or tail are not
queryable. A `damaged` signal stays `damaged` until the next start, even
after you delete the files, because it describes what this start found.
The per-item list covers only this start; earlier items show up as the file
count and size in `quarantineDir`.

**What is never shown.** Quarantine file names are relative to the data
directory, reasons name the damage (`incomplete record at offset 48213 (torn
write)`) and never file contents, and credentials never appear. `/health`
and unauthenticated `/ready` say nothing about recovery: quarantine does not
make an instance unready, since every readable event is served.

**Example.** This is what `tests/integration/recovery_health.rs` does: tear
the last WAL record of a stopped server as a crash mid-write would, then
start it again.

```bash
vyrtel --data-dir /tmp/vyrtel-rel11 &
for m in committed torn; do
  curl -s -H 'content-type: application/json' -d "{\"message\":\"$m\"}" localhost:8080/api/v1/events
done                                                             # {"accepted":1} twice
kill -9 %1
wal=$(ls /tmp/vyrtel-rel11/wal/logs/*.wal | tail -1)
truncate -s -5 "$wal"                                            # tear the final record
vyrtel --data-dir /tmp/vyrtel-rel11 &
curl -s localhost:8080/api/v1/system/storage | jq -c '.recovery, .signals.logs.recovery.status'
# {"status":"damaged","signals":["logs"]}
# "damaged"
```

Only `committed` is queryable afterwards. Restart once more and the status
is `quarantineRetained`; stop, delete `/tmp/vyrtel-rel11/quarantine`, start
again, and it is `clean`. Sign in first as in
[Verification queries](#verification-queries).

#### Recovery health changes in 0.1.2

* `signals.<signal>.recovery` in `/api/v1/system/storage` gains `status`,
  `walFilesReplayed`, `segmentsQuarantined`, `walFilesQuarantined`,
  `walTailsQuarantined`, `quarantinedBytes` and `quarantineDir`; the
  response and `/api/v1/system/info` gain a top-level `recovery` summary.
  Existing fields are unchanged.
* Settings → Storage warns when a signal is `damaged` or
  `quarantineRetained`.
* Files left in quarantine by an earlier or interrupted recovery are now
  reported. Before, only items quarantined by the current start were listed.
* A quarantine directory that cannot be read is reported as `unknown`, with
  the reason, never as `clean`.
* `quarantineBytes` is now the sum of the `quarantine/<signal>/`
  directories (one scan per request instead of two); files placed directly
  in `quarantine/` by hand are no longer counted.

## Backup and restore

A backup is a copy of the whole data directory taken while Vyrtel is
stopped. Restored on any host, it brings back every log, trace and metric,
and every dashboard, alert, saved query, API key, user and session. Vyrtel
has no export or import step.

### What the data directory holds

| Entry                  | Contents                                                                                       | In a backup  |
|------------------------|------------------------------------------------------------------------------------------------|--------------|
| `vyrtel.json`          | Manifest: the `layout` identity, the data-directory `format`, the Vyrtel version that last wrote it and, while stopped after a clean shutdown, the `inventory` of WAL and segment files with their sizes | required |
| `metadata/metadata.db` | Dashboards, alerts and their state, saved queries, API keys (hashed), users (Argon2id), sessions (hashed), query statistics | required |
| `metadata/metadata.db-wal`, `-shm` | SQLite journal; present only while running or after a crash                        | if present   |
| `wal/<signal>/`        | Write-ahead logs: the newest telemetry, not yet sealed                                         | required     |
| `segments/<signal>/`   | Sealed telemetry                                                                               | required     |
| `quarantine/`          | Damaged files recovery set aside ([quarantine](#crash-recovery-and-backups))                   | optional     |
| `tmp/`, `indexes/`, `.lock` | Scratch space and the process lock; recreated at start                                    | not needed   |

The configuration file and `VYRTEL_*` environment variables are not in the
data directory. Back them up separately; in particular keep
`auth.ingest_api_key` the same on the restored host.

**Protect backups like credentials.** They hold the password and key
hashes, session tokens (hashed) and alert webhook URLs in plain text. A
session or API key in a restored backup works again until it expires or is
revoked, so after restoring a backup that may have leaked, revoke the API
keys and change the admin password.

### Back up a stopped server

1. **Stop Vyrtel** (`docker stop vyrtel`, `systemctl stop vyrtel`, Ctrl+C)
   and check that it logged `shutdown complete` and exited with status 0.
   A clean shutdown records every WAL and segment file and its size in
   `vyrtel.json` (`inventory`), so the restored copy is checked for missing
   or truncated files when it starts. After an
   [incomplete shutdown](#when-the-deadline-passes-the-signal-is-repeated-or-the-flush-fails)
   (status 1, including an inventory that could not be written or did not
   finish before `server.shutdown_timeout`) or a crash there is no
   inventory: the backup is still consistent and restores like one taken
   after a crash, but a file left out of it cannot be detected. Start and
   stop Vyrtel cleanly once before backing up if you can.
2. **Copy the whole data directory**, for example:

   ```bash
   tar -C /var/lib/vyrtel/data -czf vyrtel-backup-$(date +%F).tar.gz .
   ```

   For a Docker named volume:
   `docker run --rm -v vyrtel-data:/data -v "$PWD":/backup busybox tar -C /data -czf /backup/vyrtel-backup.tar.gz .`
3. Start Vyrtel again. Producers that buffer and retry (the OpenTelemetry
   SDKs and Collector do) resend what they could not deliver meanwhile.

**Without stopping**, back up from an atomic filesystem snapshot (LVM, ZFS,
btrfs, cloud volume snapshots). It restores like a power loss: recovery
applies and the [contract](#crash-recovery-and-backups) holds. Do not copy
the directory file by file while Vyrtel runs: sealing can move events from
a WAL into a segment between two copies, so the copy can miss or duplicate
them. Scheduling backups and online snapshots taken by Vyrtel itself are not
provided.

### Restore

1. Install the Vyrtel version that took the backup, or a later one of the
   same layout (`vyrtel check-data-dir` on the restored copy says whether it
   can start it). Before 1.0 a release reads only data directories of its
   own layout and refuses any other, unchanged (see
   [Upgrading between pre-1.0 releases](#upgrading-between-pre-10-releases)).
2. Stop Vyrtel if it is running on the target directory, and move any
   existing data directory aside.
3. Unpack the backup as the data directory (`storage.path`), and make it
   writable by the user Vyrtel runs as (uid 65532 in the Docker image, see
   [bind mounts](#docker-and-bind-mounts)):

   ```bash
   mkdir -p /var/lib/vyrtel/data
   tar -C /var/lib/vyrtel/data -xzf vyrtel-backup-2026-10-11.tar.gz
   ```

4. Start Vyrtel with the same `[auth]` settings. `auth.admin_password` may
   be left out: the restored admin user keeps its password, and no new one
   is generated. If it is set, it replaces the stored password, exactly as
   on any start.
5. Check the startup log: one `recovered stream` line per signal, and no
   `recovery found damaged files` warning for a backup of a stopped server.
6. Run the verification queries below.

### Verification queries

Run these before taking the backup and after restoring it; the answers
must match. Log in first and pass the
session cookie (`-b cookies` below), or replace it with an admin API key
(`-H "Authorization: Bearer $ADMIN_KEY"`).

```bash
curl -s -c cookies -H 'Content-Type: application/json' \
  -d '{"username":"admin","password":"…"}' localhost:8080/api/v1/auth/login
for s in logs traces metrics; do                          # event counts per signal
  curl -s -b cookies -H 'Content-Type: application/json' \
    -d '{"query":""}' localhost:8080/api/v1/query/$s/count
done
curl -s -b cookies localhost:8080/api/v1/traces/<trace id>   # a known trace, span by span
curl -s -b cookies localhost:8080/api/v1/metrics             # metric names
curl -s -b cookies localhost:8080/api/v1/dashboards          # dashboards and panels
curl -s -b cookies localhost:8080/api/v1/alerts              # alert definitions
curl -s -b cookies localhost:8080/api/v1/saved-queries
curl -s -b cookies localhost:8080/api/v1/api-keys            # names, prefixes, scopes
curl -s -b cookies localhost:8080/api/v1/system/storage      # recovery.quarantined is [] per signal
```

Then check authentication: an ingest request with a restored API key
returns `200`, one without a key returns `401`, and the admin login works
with the password the backed-up instance had.

Counts cover every stored event whatever its age, but retention keeps
running after the restore: segments older than `[retention]` are deleted as
on any start, so restore with the same retention, and compare counts soon
after starting. An alert's evaluation state (`OK`, `FIRING`) is restored and
then updated by its next evaluation.

### What startup checks

Before Vyrtel creates, locks or changes anything in the data directory, it
checks that the directory is complete and that this version can read it.
If not, it exits with status 1, prints why and changes nothing, so you can
fix the restore and start again:

| Found                                                                            | Message (after `vyrtel: data directory <path>`)                         | What to do |
|----------------------------------------------------------------------------------|-------------------------------------------------------------------------|------------|
| A WAL or segment file in the inventory is missing                                | `is incomplete: wal/logs/000000000003.wal is missing`                   | Restore the whole directory again |
| A WAL or segment file in the inventory has another size (a truncated copy)      | `is incomplete: segments/logs/000000000002.seg has 4096 bytes, 81920 at the last clean shutdown` | Restore the whole directory again |
| A required entry is missing (`vyrtel.json` present)                             | `is incomplete: wal/metrics is missing`                                 | Restore the whole directory again |
| WAL or segment files but no `metadata/metadata.db` (`vyrtel.json` present)       | `is incomplete: metadata/metadata.db is missing`                        | Restore the whole directory again |
| WAL or segment files and `metadata/metadata.db` of this layout, but no `vyrtel.json` | `is incomplete: vyrtel.json is missing`                             | Restore `vyrtel.json` from the same backup |
| `metadata/metadata.db` is empty (no schema) next to `vyrtel.json`                | `is incomplete: metadata/metadata.db is empty (it has no schema)`       | Restore it from the same backup |
| `metadata/metadata.db` but a signal's `wal/` or `segments/` directory is missing (no `vyrtel.json`) | `is incomplete: wal/logs is missing; …`                  | Restore the whole directory again |
| `vyrtel.json` is not valid JSON, has an unknown key or no `layout`/`format` it can use, or its inventory lists something other than WAL and segment files | `has an unusable vyrtel.json: …` | Restore `vyrtel.json` from the same backup |
| A directory of another layout: no `vyrtel.json` and a metadata database or telemetry without this layout's identity (0.1.1, Observer 0.1.0), a `vyrtel.json` with another `layout` or none, or a WAL, segment or metadata file of another layout mixed in | `is not in the layout Vyrtel 0.1.2 reads: there is no vyrtel.json, and metadata/metadata.db has no layout identity (…)`, `… vyrtel.json has layout 0.1.3, written by Vyrtel 0.1.3`, `… wal/logs/000000000004.wal is a WAL file with no layout identity …` | See [Upgrading between pre-1.0 releases](#upgrading-between-pre-10-releases) |
| A file of this layout in a format version it does not have (its header or footer, any checksummed WAL record or event batch, the metadata schema) | `cannot be read by Vyrtel 0.1.2: segments/logs/000000000004.seg uses segment format version 2, …` | Start it with the build that wrote it |
| `metadata/metadata.db` is not an SQLite database                                 | `has an unreadable metadata/metadata.db: …`                             | Restore it from the same backup |

An incomplete directory lists every problem found (the first ten, then how
many more), separated by `;`. For example, a backup that left out one WAL
file:

```
vyrtel: data directory /var/lib/vyrtel/data is incomplete: wal/logs/000000000003.wal is missing. Vyrtel did not start and changed nothing in it. If it is a restored backup, restore the whole data directory again; to start without the missing parts on purpose, start once with --accept-incomplete-restore (see docs/configuration.md#backup-and-restore)
```

**Continuing without the missing parts.** If the rest of the backup is all
there is, start once with `--accept-incomplete-restore`
(`vyrtel --accept-incomplete-restore`, or append it to the container's
command). Vyrtel logs one `WARN starting without part of the data
directory` line per problem and starts without them: events in missing WAL
and segment files are gone, missing directories are created, and a missing
or empty metadata database starts with no dashboards, alerts, keys or
users (with no `auth.admin_password`, a new admin password is generated
and printed), and a missing `vyrtel.json` is
written again. The flag only covers missing, truncated and empty parts;
another layout or format is always refused. It is not a
configuration setting on purpose, so it is not left on by accident: once
started, the directory is complete as it is, and later starts need no flag.

A directory without any of Vyrtel's entries (empty, or a new volume with
only `lost+found`) is a new data directory. One from before 0.1.2 (no
`vyrtel.json`) is refused, unchanged: see
[Upgrading between pre-1.0 releases](#upgrading-between-pre-10-releases).

Damaged files are not refused: recovery
[quarantines](#crash-recovery-and-backups) them as after a crash, and the
rest starts. A file of another layout or format is never mistaken for
damage, so it is never quarantined. (A file without Vyrtel's magic bytes at
all, such as one Observer 0.1.0 wrote, mixed into a directory of this
layout, cannot be told from damage and is quarantined: moved, never
deleted.)

**Limitations.**

* Missing WAL and segment files are detected only through the inventory a
  clean shutdown records. A directory without one — a backup taken after a
  crash or an [incomplete shutdown](#when-the-deadline-passes-the-signal-is-repeated-or-the-flush-fails),
  or a filesystem snapshot of a running server — is checked for its
  directories and metadata only, so a file left
  out of such a backup starts without its events. Startup logs `INFO data
  directory has no inventory from a clean shutdown` in that case.
* The inventory holds names and sizes, not checksums: a file with the right
  size but wrong bytes is found by recovery's own checksums instead (every
  WAL record and segment chunk has one) and quarantined, not refused.
  Checksumming every file at every start would read the whole data
  directory.
* The metadata database is not in the inventory (SQLite changes its files
  as it closes). A missing or empty database is refused; one replaced by a
  different, valid database is not detected.
* The inventory describes the directory as the last clean shutdown left it,
  and the next start removes it before changing anything.
* The check reads every version recovery reads but not the data: per WAL
  record a few bytes, per segment its header, footer and summary. It runs
  twice per start (before and after taking the lock), so it adds a little
  startup time on directories with large WALs; see
  [measurements](testing.md#startup-format-check).
* `vyrtel.json`'s `writtenBy` is updated by every version that starts the
  directory, so it names the newest version that has used it.

### Restore drill

[`scripts/backup/restore-drill.sh`](../scripts/backup/restore-drill.sh)
runs this procedure end to end against a built binary on Linux or macOS:
it populates an instance with the [shared fixtures](../tests/fixtures/README.md)
(all three signals in sealed segments and in WALs, a dashboard with a
panel, an alert, a saved query, an API key), stops it, takes a `tar`
backup, deletes the original, restores the backup into a new directory,
starts it without an admin password configured and compares every
verification query. It then checks the API key, anonymous refusal and
login; that copies without `metadata/metadata.db`, with an empty one,
without one WAL file and without one segment file each exit with status 1
and are left unchanged; and that `--accept-incomplete-restore` starts the
copy without the WAL file, after which it starts without the flag. CI runs
it on every change:

```bash
cargo build --release -p server
scripts/backup/restore-drill.sh target/release/vyrtel
```

#### Backup and restore changes in 0.1.2

* Data directories get a manifest, `vyrtel.json`, with the layout
  identity. Include it in backups. 0.1.2 does not start directories that
  earlier versions wrote; see
  [Upgrading between pre-1.0 releases](#upgrading-between-pre-10-releases).
* A clean shutdown records the WAL and segment files and their sizes in
  `vyrtel.json`; a start refuses the directory if any of them is missing
  or has another size.
* A data directory with WAL or segment files, or a manifest, but no
  metadata database of its layout or an empty one, now refuses to start. Before, Vyrtel
  created an empty metadata database: dashboards, alerts, saved queries
  and API keys were lost without a message, and with authentication
  enabled and no `auth.admin_password`, a new admin password was generated.
* New `--accept-incomplete-restore` flag to start such a directory
  anyway.
* A segment or WAL file of another layout, or in a format version this
  release cannot read, now stops startup and is left in place. Before, it
  was moved to `quarantine/` as if damaged, and its events were not
  visible.
* A metadata database of another layout now stops startup. Before, it was
  opened as is.

## Upgrading between pre-1.0 releases

Before 1.0, Vyrtel makes no promise that a release reads data an earlier
release wrote. Each data directory carries a **layout identity**
(`"layout"` in `vyrtel.json`, and the same identity in every WAL, segment
and metadata file; see [storage format](storage-format.md#versioning)). A
release reads only directories of its own layout. Any other directory is
refused with exit status 1 **before anything in it changes**: it is not
migrated, converted, quarantined, reset or deleted, and it stays readable
by the release that wrote it. Releases that keep a layout read each other's
directories; a release that changes one says so in its notes. This policy
is reassessed before 1.0. The 0.1.2 audit that introduced it is in
[docs/compatibility/0.1.2.md](compatibility/0.1.2.md).

| Release           | Writes layout                     | Starts data directories written by                          |
|-------------------|-----------------------------------|-------------------------------------------------------------|
| 0.1.0 (Observer)  | Observer's own (no identity)      | 0.1.0                                                       |
| 0.1.1             | none (no identity, no `vyrtel.json`) | 0.1.1 (it checks nothing: never start it on newer data)  |
| 0.1.2             | `0.1.2`                           | layout `0.1.2` only; refuses every directory above unchanged |

Builds of 0.1.2 made before its layout reset (a `vyrtel.json` with
`"format": 1` or `2` and no `"layout"`) are not 0.1.2 releases and their
directories are refused the same way.

### Before switching: `vyrtel check-data-dir`

Run the **new** version's `check-data-dir` on the stopped data directory.
It reads only (no lock file, no journal, nothing created, safe even if it
is the wrong directory) and runs the same checks as a start:

* Exit status **0**: the new version can start it. It prints what it found
  and what the first start changes.
* Exit status **1**: it cannot. It prints the same message a start would
  (see [What startup checks](#what-startup-checks)) and changes nothing.
* Exit status **2**: the configuration could not be loaded.

`--data-dir`, `-c` and `VYRTEL_STORAGE_PATH` choose the directory as they do
for a start, and `--accept-incomplete-restore` shows what such a start
would accept. For a data directory written by 0.1.1:

```text
$ vyrtel check-data-dir --data-dir /var/lib/vyrtel/data
vyrtel: data directory /var/lib/vyrtel/data is not in the layout Vyrtel 0.1.2 reads: there is no vyrtel.json, and metadata/metadata.db has no layout identity (it has schema_migrations: Vyrtel 0.1.1 or earlier, or Observer, wrote it). Vyrtel 0.1.2 reads layout 0.1.2 only, and before 1.0 a release reads no data directory of another layout. Vyrtel did not start and changed nothing in it. Keep running the Vyrtel version that wrote it, or start Vyrtel 0.1.2 on a new, empty data directory (see docs/configuration.md#upgrading-between-pre-10-releases)
$ echo $?
1
```

A directory 0.1.2 wrote, once stopped cleanly:

```text
data directory /var/lib/vyrtel/data
  vyrtel.json: layout 0.1.2, format 1, last written by Vyrtel 0.1.2
  clean-shutdown inventory: every WAL and segment file is present with its size
  metadata/metadata.db: layout 0.1.2, schema version 1
Vyrtel 0.1.2 (layout 0.1.2, data-directory format 1, metadata schema 1) can start it.
Starting it changes no file formats.
```

### Moving to a release with another layout

There is no in-place upgrade, export or import between layouts. The old
directory stays exactly as it was, so this is always reversible:

1. Stop the old version cleanly (exit status 0) and take a
   [backup of the stopped data directory](#back-up-a-stopped-server). Keep
   it until you no longer need its data.
2. Decide what to keep from the old instance:
   * **Telemetry**: keep the old version running on the old directory (on
     another port, read-only use) until its retention has passed, and query
     it there. It cannot be moved into the new layout.
   * **Dashboards, alerts and saved queries**: read them from the old
     instance (`GET /api/v1/dashboards`, `/api/v1/alerts`,
     `/api/v1/saved-queries`) and create them again through the new one's
     API or UI.
   * **API keys and users**: only their hashes are stored, so they cannot
     be carried over. Create new keys on the new instance and update the
     producers that use them; set `auth.admin_password` (or note the
     generated one) on its first start.
3. Start the new version on a **new, empty** data directory (another
   `storage.path` or volume). `check-data-dir` on it reports `Starting it
   will create a new data directory in this layout.`
4. Point producers at the new instance. Producers that buffer and retry
   (the OpenTelemetry SDKs and Collector) resend what they could not
   deliver meanwhile.
5. Delete the old directory yourself once nothing needs it. No Vyrtel
   release deletes, resets or overwrites a data directory it refused.

**Going back** is the same procedure in reverse: stop the new version and
start the old one on its own, untouched directory (or its backup). Never
start 0.1.1 or Observer 0.1.0 on a directory a newer release wrote: they do
not check layout identities, and may take its files for damage and move
them to `quarantine/`, or fail part-way through their own metadata
migration.

### Interrupted first start

A first start creates the signal directories, then the metadata database
(its schema and identity in one transaction), then `vyrtel.json`, then the
first WAL. Interrupted at any point, it is finished by the next start of the
same version: a directory without `vyrtel.json` but with no telemetry, and a
metadata database that is absent, empty or of this layout, is accepted and
recorded (`check-data-dir`: `Starting it will finish setting it up and
record the layout in vyrtel.json.`). A creation that fails rather than
stops, for example on a full disk, exits with status 1:

```text
vyrtel: opening metadata store: creating the metadata schema failed (nothing was created): database or disk is full
```

Fix the cause and start again.

### Reproducing the refusal

`tests/fixtures/datadir/v0.1.1/data` and `v0.1.0/data` are data
directories the 0.1.1 and Observer 0.1.0 releases wrote and stopped
cleanly ([fixtures](../tests/fixtures/datadir/README.md)). From the
repository root (Linux, macOS):

```bash
cp -R tests/fixtures/datadir/v0.1.1/data /tmp/vyrtel-old
# git keeps no empty directories; the release left these in place:
mkdir -p /tmp/vyrtel-old/{wal,segments}/{logs,traces,metrics}
find /tmp/vyrtel-old -type f -exec sha256sum {} + | sort > /tmp/before
vyrtel check-data-dir --data-dir /tmp/vyrtel-old     # exit 1, as above
vyrtel --data-dir /tmp/vyrtel-old                     # exit 1, the same message
find /tmp/vyrtel-old -type f -exec sha256sum {} + | sort | diff /tmp/before - && echo unchanged
vyrtel --data-dir /tmp/vyrtel-new                     # the documented way forward
```

`tests/integration/layouts.rs` runs this for every committed fixture,
compares every file's SHA-256 before and after, and also refuses WAL,
segment and metadata files of 0.1.1 mixed into a directory of this layout.

#### Compatibility changes in 0.1.2

Breaking changes; see [docs/compatibility/0.1.2.md](compatibility/0.1.2.md)
for the full inventory.

* **Data directories from earlier versions are refused.** 0.1.2 starts no
  directory that 0.1.1, Observer 0.1.0 or an earlier 0.1.2 build wrote; it
  exits with status 1 and changes nothing. Start it on a new directory as
  described above. Before, 0.1.2 builds recorded a manifest in such a
  directory and kept reading its old files.
* Every persisted format is at version 1 of layout `0.1.2`, and every file
  carries the layout identity: the WAL header grew to 32 bytes, the segment
  header to 40 and its footer to 64, the event codec of WAL batches is 1
  (with the per-batch resource table), and the metadata database has an
  `application_id` and a `vyrtel_layout` table instead of
  `schema_migrations`. Readers of the earlier layouts are gone.
* `vyrtel.json` has a `"layout"` key, and a manifest with an unknown key is
  refused instead of ignored.
* `vyrtel check-data-dir` no longer prints migration steps or which
  releases can read a directory: there are no migrations.
* `--accept-incomplete-restore` no longer starts WAL and segment files that
  neither a manifest nor a metadata database of this layout identifies.
* A WAL or segment file of Observer 0.1.0 is no longer recognised by its
  magic bytes. A directory Observer wrote is refused as a whole; such a
  file copied into a directory of this layout is quarantined as damage
  (moved, never deleted).
* `GET /api/v1/live` (an undocumented duplicate of `/api/v1/live/logs`) is
  removed; it answers `404 not_found`. The `spans` spelling of the
  `traces` signal in `/api/v1/query/{signal}/…` and `?signal=` is removed;
  it answers `404` (`unknown signal: use logs, traces or metrics`) or `400`.
* An environment variable under a settings prefix that is not a setting
  (for example `VYRTEL_AUTH_ENABLE`) stops startup instead of being
  ignored ([Environment variables](#environment-variables)).

## Storage failures

When the data volume fills up, turns read-only or denies access, Vyrtel
stops acknowledging writes instead of pretending they were stored. There is
nothing to configure; this section covers what operators see and do.
Details are in [Storage write failures](storage-format.md#storage-write-failures).

**What clients see.** Ingest requests get `503` with `Retry-After: 1`:

```json
{
  "error": {
    "code": "storage_write_failed",
    "reason": "noSpace",
    "outcome": "notStored",
  "message": "the request was not stored: no space left on the storage device. Nothing in it was acknowledged; retry once storage is writable"}}
```

With `"outcome": "notStored"` nothing in the request is stored, so retrying it is safe. `"outcome": "unknown"` is rare:
the fsync failed, then so did the fsync that would have made removing the request durable. The request is not
acknowledged and not visible, but after a power loss it may come back, and a retry can store it twice. Requests that got
a 2xx before the failure stay stored.

**How to notice.**

* `GET /ready` returns `503 {"status": "degraded", "storageWritable": false, ...}`
  when a test write to `data/tmp/` fails (full or read-only volume), and
  lists the affected `signals` while a signal's WAL or sealing keeps
  failing. Point your orchestrator's readiness probe at it. `GET /health`
  stays 200: the process is alive and queries still work.
* `GET /api/v1/system/storage` shows `writeProbe` and, per signal,
  `ingestHealthy` and `faults`. Each fault has the operation, the reason,
  the file relative to the data directory, the OS error text, when it was
  first and last seen, and how often it occurred. Host paths and secrets
  are never included.
* The log has one `ERROR storage write failed` line per new fault, with
  `signal`, `operation`, `reason` and `path`, and an `INFO storage write
  path recovered` line when it clears.

**What to do.** Free space (shorten `[retention]`, remove files that are
not Vyrtel's, grow the volume), or fix the mount and permissions so the user Vyrtel runs as can write the whole data
directory. Vyrtel retries by itself: within about a second of storage working again the fault clears and readiness
returns, even if no requests arrive. A restart is not needed but is safe, and
recovery brings back every acknowledged event. Do not delete files under
`wal/` or `segments/` to make room; they hold acknowledged telemetry. If
the disk fills up during a backup or restore, start that backup or restore
again; the rules above still apply.
