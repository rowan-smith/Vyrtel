# Architecture

Vyrtel is **one process** with **one data directory**. Crates are code
boundaries, not network boundaries.

```mermaid
flowchart LR
    subgraph clients[Producers & users]
        SDK[OTel SDK / Collector]
        App[App logging JSON / NDJSON]
        UI[Browser UI]
    end

    subgraph vyrtel[vyrtel process]
        HTTP[server: axum HTTP\nauth · routing · SSE]
        ING[ingest: parse & map\nnative JSON · OTLP]
        Q[query: parser · planner · executor]
        ST[storage: WAL · segments · indexes\nper-signal streams]
        MD[metadata: SQLite]
        BG[background jobs\nseal · compact · retention\nalerts · stats flush]
        WEB[embedded web UI]
    end

    subgraph disk[data/]
        WAL[(wal/)]
        SEG[(segments/)]
        DB[(metadata/metadata.db)]
    end

    SDK -- OTLP/HTTP --> HTTP
    App -- POST /api/v1/events --> HTTP
    UI -- REST + SSE --> HTTP
    HTTP --> ING --> ST
    HTTP --> Q --> ST
    HTTP --> MD
    HTTP --> WEB
    BG --> ST
    BG --> MD
    ST --> WAL & SEG
    MD --> DB
```

| Crate           | Responsibility                                                                                         | Depends on         |
|-----------------|--------------------------------------------------------------------------------------------------------|--------------------|
| `telemetry`     | Common event model (`TelemetryEvent`, `Fields`, `Value`), JSON view                                    | –                  |
| `storage`       | Event codec, WAL, segment format, indexes, recovery, sealing, compaction, retention, index cache       | telemetry          |
| `query`         | Lexer, parser, AST, evaluator, index pruning, executor, aggregations, traces, query stats              | telemetry, storage |
| `ingest`        | Native JSON/NDJSON and OTLP (protobuf + JSON) → `TelemetryEvent`                                       | telemetry          |
| `metadata`      | SQLite: migrations, settings, users/sessions, API keys, dashboards, saved queries, alerts, query stats | –                  |
| `server`        | Config, HTTP API, auth, live tail, alert evaluator, embedded UI, binary                                | all                |
| `tools/loadgen` | Load generator                                                                                         | –                  |

## Ingestion path

```mermaid
sequenceDiagram
    participant C as Client
    participant H as HTTP handler
    participant P as Parser (blocking pool)
    participant Q as Bounded queue (per signal)
    participant W as WAL writer thread
    participant A as Active buffer
    participant L as Live subscribers

    C->>H: POST /api/v1/events (or /v1/logs …)
    H->>H: read body ≤ max_request_size (after gzip)
    H->>P: parse + map
    P-->>H: Vec<TelemetryEvent>
    H->>Q: try reserve N events
    alt queue full
        Q-->>H: QueueFull
        H-->>C: 429 + Retry-After
    else accepted
        Q->>W: batch
        W->>W: assign ids, encode, append WAL record
        W->>W: fsync (strict mode, group commit)
        W->>A: publish batch (queryable)
        W->>L: broadcast (bounded ring)
        W-->>H: ack
        H-->>C: 200 {"accepted": N}
    end
```

* Every queue is bounded: request bodies (semaphore), events per signal
  (`queue_capacity`), the writer channel, and the live-tail ring.
* The writer drains whatever is queued into one group commit, so `strict`
  durability costs one fsync per group, not per request.
* A request is acknowledged only after its WAL record is written (and
  fsynced in strict mode) **and** visible to queries.

## Storage path

```mermaid
flowchart TB
    W[WAL writer] -->|append| AW[(active WAL N)]
    W -->|publish| ACT[active buffer]
    ACT -->|size / age / count| ROT{rotate}
    ROT -->|freeze + new WAL N+1| FRZ[frozen buffer]
    FRZ --> SEAL[maintenance thread: sort, write tmp/, fsync]
    SEAL -->|rename = commit| SEG[(segment N)]
    SEAL -->|then delete| AW
    SEG --> CMP[compaction: merge small segments]
    SEG --> RET[retention: delete expired segments]
```

Each signal (logs, traces, metrics) is an independent stream with its own
writer thread, maintenance thread, WAL files, segments and retention. See
[storage-format.md](storage-format.md) for file layouts, the commit
procedure and recovery.

## Query path

```mermaid
flowchart LR
    T[query text] --> LX[lexer] --> PS[parser] --> AST[AST]
    AST --> CMP[compile: resolve fields, pre-process literals]
    CMP --> SNAP[storage snapshot: segments + unsealed batches]
    SNAP --> PLAN[per segment: time range → bitmap / Bloom / zone-map candidate blocks]
    PLAN --> READ[decode only needed columns]
    READ --> EVAL[evaluate predicate]
    EVAL --> TOP[top-K by time, cursor, early stop]
    TOP --> RES[events + diagnostics]
```

* Parsing is independent of execution (`query::parser` knows nothing about
  storage).
* A *snapshot* holds `Arc`s to segments and unsealed batches, so sealing,
  compaction and retention never disturb a running query.
* Segments are visited newest-first; once the page is full, any segment or
  block whose newest event is older than the oldest kept result is skipped
  (`segmentsNotNeeded`).
* Queries run on the blocking thread pool under a concurrency limit, with a
  timeout and a scan-bytes limit.
* Every query records per-field statistics (fields used, index kind, bytes
  read, segments skipped). They are flushed to SQLite every minute — the
  input for adaptive indexing in a later version.

## Metadata

SQLite (bundled, WAL mode, foreign keys on, versioned migrations, prepared
statements) stores product metadata only: settings, API key hashes, users
and sessions, dashboards and panels, saved queries, alerts with state and
history, aggregated query statistics. Telemetry never goes to SQLite.

## Background jobs

| Job                             | Where                         | Interval                                  |
|---------------------------------|-------------------------------|-------------------------------------------|
| WAL fsync (`normal` durability) | writer thread                 | `fsync_interval` (1 s)                    |
| Segment sealing                 | maintenance thread per signal | on rotation                               |
| Retention, compaction           | maintenance thread per signal | 30 s                                      |
| Alert evaluation                | tokio task                    | every 5 s, each alert at its own interval |
| Query-stats flush               | tokio task                    | 60 s and at shutdown                      |
| Session purge                   | tokio task                    | hourly                                    |

## Frontend / backend relationship

The React + TypeScript UI (`web/`) is built by Vite into static files that
are embedded into the binary at compile time (`rust-embed`; see
`crates/server/build.rs`). In production the server serves the UI at `/`
and falls back to `index.html` for client-side routes; the UI talks to the
same origin's REST API and SSE live tail. During development `npm run dev`
serves the UI with hot reload and proxies `/api` and `/v1` to a running
server.

## Process boundaries & shutdown

There is exactly one OS process. On SIGINT/SIGTERM the server stops
accepting connections, ends live-tail streams, drains in-flight requests,
flushes query statistics, then each storage stream fsyncs its WAL and stops
its threads. Unsealed data is not sealed at shutdown — WAL replay restores
it on the next start, which keeps shutdown fast. A data-directory lock
(`data/.lock`) prevents two processes from using the same directory.

Vyrtel's own diagnostics go to stderr via `tracing` (text or JSON). They
are deliberately **not** ingested into Vyrtel.
