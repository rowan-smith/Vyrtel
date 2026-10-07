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
max_request_size = "16MB"
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
| `max_memory`          | `512MB`   | `VYRTEL_STORAGE_MAX_MEMORY`          | Global memory budget (see below). Minimum 64MB.                                                                                                    |
| `segment_target_size` | `64MB`    | `VYRTEL_STORAGE_SEGMENT_TARGET_SIZE` | Seal the active segment at this *uncompressed in-memory* size (capped by the memory budget). Compressed segment files are typically 5–15× smaller. |
| `segment_max_age`     | `5m`      | `VYRTEL_STORAGE_SEGMENT_MAX_AGE`     | Seal the active segment after this long, even if small.                                                                                            |
| `segment_max_events`  | `2000000` | –                                      | Seal after this many events.                                                                                                                       |
| `durability`          | `normal`  | `VYRTEL_STORAGE_DURABILITY`          | `normal`: acknowledge after writing to the OS, fsync every `fsync_interval`. `strict`: fsync before every acknowledgement (group-committed).       |
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
| `max_request_size` | `16MB`  | `VYRTEL_INGEST_MAX_REQUEST_SIZE` | Maximum body size (after gzip decoding). Larger requests get 413.                                                |
| `queue_capacity`   | `10000` | `VYRTEL_INGEST_QUEUE_CAPACITY`   | Events waiting to be written, per signal. When full, requests get 429. A single batch larger than this gets 413. |
| `ack_timeout`      | `30s`   | –                                  | Give up waiting for a write acknowledgement (503).                                                               |

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
| Ingest          |   15% | Limits concurrent request bodies (`share ÷ max_request_size` requests). Event queues are bounded by `queue_capacity`.                                                                                                                               |
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
docker run -p 8080:8080 -v ./vyrtel-data:/data vyrtel
```
