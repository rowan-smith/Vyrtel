# Configuration

Vyrtel runs with no configuration at all. When you need to change
something, use any combination of:

1. a TOML file (`--config vyrtel.toml`, or `./vyrtel.toml` if present),
2. `VYRTEL_*` environment variables,
3. command-line flags,

in increasing order of precedence. `vyrtel config` prints the effective
configuration; `GET /api/v1/system/config` returns it (without secrets).
Unknown keys are rejected so typos do not silently do nothing.

Sizes accept `B`, `KB`, `MB`, `GB` (powers of 1024; `MiB` etc. also work).
Durations accept `ms`, `s`, `m`, `h`, `d`, `w`, combinations like `1h30m`,
or a bare number of seconds.

## Example

```toml
[server]
bind = "0.0.0.0:8080"

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

| Key    | Default        | Env                    | Description                                   |
|--------|----------------|------------------------|-----------------------------------------------|
| `bind` | `0.0.0.0:8080` | `VYRTEL_SERVER_BIND` | Listen address for the UI, API and OTLP/HTTP. |

### `[storage]`

| Key                   | Default   | Env                                    | Description                                                                                                                                        |
|-----------------------|-----------|----------------------------------------|----------------------------------------------------------------------------------------------------------------------------------------------------|
| `path`                | `./data`  | `VYRTEL_STORAGE_PATH`                | Data directory. Everything persistent lives here.                                                                                                  |
| `max_memory`          | `512MB`   | `VYRTEL_STORAGE_MAX_MEMORY`          | Global memory budget (see below). Minimum 64MB; with a derived ingest limit, at least 219MB for 1MB requests ([ingest memory](#ingest-memory)).    |
| `segment_target_size` | `64MB`    | `VYRTEL_STORAGE_SEGMENT_TARGET_SIZE` | Seal the active segment at this *uncompressed in-memory* size (capped by the memory budget). Compressed segment files are typically 5–15× smaller. |
| `segment_max_age`     | `5m`      | `VYRTEL_STORAGE_SEGMENT_MAX_AGE`     | Seal the active segment after this long, even if small.                                                                                            |
| `segment_max_events`  | `2000000` | –                                      | Seal after this many events.                                                                                                                       |
| `durability`          | `normal`  | `VYRTEL_STORAGE_DURABILITY`          | `normal`: acknowledge after writing to the OS, fsync every `fsync_interval`. `strict`: fsync before every acknowledgement (group-committed). See [Crash recovery](#crash-recovery-and-backups). |
| `fsync_interval`      | `1s`      | `VYRTEL_STORAGE_FSYNC_INTERVAL`      | WAL fsync period in `normal` mode.                                                                                                                 |
| `compaction`          | `true`    | `VYRTEL_STORAGE_COMPACTION`          | Merge small segments (e.g. sealed by age on quiet systems).                                                                                        |
| `zstd_level`          | `3`       | –                                      | Compression level 1–22. Higher is smaller and slower to write.                                                                                     |
| `block_events`        | `1024`    | –                                      | Events per compressed block.                                                                                                                       |

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
| `queue_capacity`   | `10000` | `VYRTEL_INGEST_QUEUE_CAPACITY`   | Events waiting to be written, per signal. When full, requests get 429. A single batch larger than this gets 413. |
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
(at most 32 at once, each through its connection's read buffer of up to
408 KB). The rest of the process is budgeted elsewhere and not counted
here: the WAL record and the active segment a batch is written into (the
active segments budget), batches still queued after their client went away
(bounded by `queue_capacity`), the read buffers of connections that are not
draining a rejected body, and the runtime.

**Where 20× comes from.** The `amplification` test measures each format's
peak heap per byte of decoded body
([details and full table](testing.md#ingest-amplification-and-derived-admission)).
Representative batches of native JSON, NDJSON and CLEF peak at 7.0–9.4×;
OTLP/JSON and OTLP/protobuf logs, traces and metrics at 12.5–16.0×; gzip
makes no measurable difference. The highest supported fixture, OTLP/JSON
logs of ~20-byte records, peaks at 17.0×.

**Limitations.** This is an estimate from finite fixtures, not a hard
memory bound. Event shapes that amplify more than 20× are outside it:
very small native events (~32× for `{"message":"…"}` lines), deeply
nested properties (~30×), very small OTLP/protobuf records (~92×), OTLP
batches that share a large resource across many tiny records (the resource
is copied into every event; over 100×), and empty objects (`{}`, ~210×). If
your producers send such batches, keep `max_request_size` well below the
limit you would otherwise use, or set `max_concurrent` explicitly from your
own measurements.

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

### `[query]`

| Key                | Default | Env                             | Description                                                |
|--------------------|---------|---------------------------------|------------------------------------------------------------|
| `timeout`          | `30s`   | `VYRTEL_QUERY_TIMEOUT`        | Per-query time limit (504).                                |
| `max_concurrent`   | `4`     | `VYRTEL_QUERY_MAX_CONCURRENT` | Queries executing at once; more wait up to 10 s, then 503. |
| `max_scan_bytes`   | `4GB`   | –                               | Compressed bytes one query may read (422 beyond).          |
| `max_results`      | `1000`  | –                               | Maximum page size.                                         |
| `max_live_streams` | `64`    | –                               | Concurrent live-tail connections.                          |

### `[auth]`

| Key              | Default | Env                            | Description                                                                                                                                       |
|------------------|---------|--------------------------------|---------------------------------------------------------------------------------------------------------------------------------------------------|
| `enabled`        | `false` | `VYRTEL_AUTH_ENABLED`        | Require API keys for ingestion and an admin login for the UI/API.                                                                                 |
| `admin_username` | `admin` | `VYRTEL_AUTH_ADMIN_USERNAME` |                                                                                                                                                   |
| `admin_password` | –       | `VYRTEL_AUTH_ADMIN_PASSWORD` | Hashed (Argon2id) into the metadata store at startup. If unset on first start with auth enabled, a random password is generated and printed once. |
| `session_ttl`    | `7d`    | –                              | Login session lifetime.                                                                                                                           |

> **Security note.** With `enabled = false` (the default, for zero-setup
> local use) anyone who can reach the port can read and write data. Enable
> authentication before exposing Vyrtel beyond a trusted network, and put
> it behind TLS (a reverse proxy) — Vyrtel itself serves plain HTTP.

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
  serve        Run the server (default)
  healthcheck  Exit 0 if the server answers /health (used by Docker)
  config       Print the effective configuration

Options:
  -c, --config <PATH>         TOML config file
      --data-dir <PATH>       storage.path
      --bind <ADDR>           server.bind
      --memory-limit <SIZE>   storage.max_memory
      --log-level <FILTER>    log.level
      --log-format <FORMAT>   text | json
  -h, --help
  -V, --version
```

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

## Docker and bind mounts

The image runs as the unprivileged `nonroot` user (uid/gid 65532). Named
volumes work out of the box. With a bind mount on Linux, make the host
directory writable for that user first:

```bash
mkdir -p vyrtel-data && sudo chown 65532:65532 vyrtel-data
docker run -p 8080:8080 -v ./vyrtel-data:/data ghcr.io/rowan-smith/vyrtel:latest
```

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

**Backups.** Back up the whole data directory, either with the server
stopped or from an atomic filesystem snapshot (LVM, ZFS, btrfs, cloud
volume snapshots). A snapshot taken while running restores like a power
loss: recovery applies and the contract above holds. Do not copy the
directory file by file while Vyrtel runs — sealing can move events from a
WAL into a segment between the two copies. To restore, stop Vyrtel, replace
the data directory with the backup and start it. The on-disk format is
unchanged in this release; no migration is needed.

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
