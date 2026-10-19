# Storage format

This document is the reference for every byte Vyrtel writes to disk. If you
change the format, update this file in the same commit and bump the relevant
version number. Code lives in `crates/storage`.

* All fixed-width integers are **little-endian**.
* `varint` is unsigned LEB128 (max 10 bytes; overflow is a decode error).
* `zigzag` maps signed to unsigned (`(v << 1) ^ (v >> 63)`) before `varint`.
* `str` is `varint length` + UTF-8 bytes. `opt_str` is `varint 0` for
  absent, otherwise `varint (length + 1)` + bytes.
* CRCs are CRC-32 (IEEE, as computed by `crc32fast`).
* Decoders never trust lengths: every length is checked against the
  remaining input before allocating, and nesting depth is capped at 64.

## Data directory

```
data/
├── vyrtel.json                   manifest: data-directory format (see below)
├── .lock                         exclusive process lock
├── metadata/metadata.db          SQLite (product metadata only)
├── wal/{logs,traces,metrics}/    NNNNNNNNNNNN.wal   (12-digit id)
├── segments/{logs,traces,metrics}/ NNNNNNNNNNNN.seg
├── quarantine/<signal>/          damaged files moved aside by recovery
├── indexes/                      reserved for future adaptive indexes
└── tmp/                          in-progress files; never committed data
```

Each signal is an independent *stream* with its own WAL, segments and
retention. File ids are allocated from one per-stream counter, so a WAL and
the segment sealed from it share the same id.

### Manifest

```json
{
  "format": 1,
  "writtenBy": "0.1.2",
  "inventory": {
    "files": {
      "segments/logs/000000000002.seg": 81920,
      "wal/logs/000000000003.wal": 4211
    }
  }
}
```

`format` is the data-directory format: it names every format version
below that the directory may contain. Format 1 is WAL file format 1, WAL
record version 1, event codec 1, segment format 1 and metadata schema
version 1. `writtenBy` is the Vyrtel version that last wrote the file.
Unknown keys are ignored.

`inventory` exists only while the directory is stopped after a clean
shutdown. It lists every `wal/<signal>/*.wal` and
`segments/<signal>/*.seg` file, relative to the data directory, with its
size in bytes. It is written after the WAL flush, the background jobs and
any seal or compaction have finished, so nothing changes those files until
the next start; an incomplete shutdown or a crash writes none. Writing it
is the last shutdown step and shares `server.shutdown_timeout` with the
others: if it fails or does not finish in time, the shutdown is reported
as incomplete (exit status 1). The next start checks it (see below), then,
holding the data-directory lock, removes it before anything else in the
directory changes, so a later crash cannot make it describe files that
have since been sealed, compacted or deleted, and a start refused because
another process holds the lock leaves it in place. The metadata database is not
listed: SQLite may still change its files as the connection closes.

The manifest is replaced atomically (`vyrtel.json.tmp` written and
fsynced, renamed over it, directory fsynced): on a start once the metadata
database is migrated and every stream directory exists, and before storage
recovery changes any telemetry file (see [Migrations](#migrations)); with
the inventory at a clean shutdown; and without it when a start removes it.
Directories from before 0.1.2 have none and get one on their next start.
A leftover `vyrtel.json.tmp` from an interrupted write is ignored and
replaced.

### Startup check

Before the data directory is created, locked or changed, the server checks
it, reading only (`crates/server/src/datadir.rs`). If that passes, it takes
the data-directory lock (`storage::DataDirLock`, then handed to
`Storage::open_locked`) and checks again under it, since another process
may have changed the directory in between; only then does anything change. A directory with none of
Vyrtel's top-level entries (`vyrtel.json`, `.lock`, `metadata`, `wal`,
`segments`, `quarantine`, `indexes`, `tmp`) is new. Otherwise startup is
refused, with nothing changed, when:

* `vyrtel.json` is unreadable, has `format` 0 or a `format` above the one
  this build writes, or its inventory names anything but WAL and segment
  files;
* *incomplete*: a file in the inventory is missing or has another size;
* *incomplete*, with a manifest: `metadata/metadata.db`, `wal/<signal>/` or
  `segments/<signal>/` is missing for any signal;
* *incomplete*, without one: WAL or segment files exist but
  `metadata/metadata.db` does not, or the database exists but a stream
  directory does not (a first start creates the stream directories, then
  the database, then WAL files, so an interrupted first start is never
  refused);
* *incomplete*: `metadata/metadata.db` has no schema (an empty file) while
  a manifest or WAL or segment files exist. Every start migrates the
  database before it writes either, so such a database was truncated or
  replaced;
* a `*.seg` or `*.wal` file declares a format version this build does not
  read (`storage::check_formats`: a segment's header, footer and
  checksummed summary; a WAL's header, and the record version and batch
  codec version of every checksummed record; see [Versioning](#versioning));
* `metadata/metadata.db` is not an SQLite database, or its
  `schema_migrations` holds a version above the newest migration. It is
  opened read-only and, when there is no `-wal` file, with SQLite's
  `immutable` flag, so no journal or lock file is created.

The *incomplete* cases are accepted when the server is started with
`--accept-incomplete-restore` (`StartOptions::accept_incomplete_restore`);
each is then logged as a warning. The others never are.

Operator-facing messages are in
[configuration → backup and restore](configuration.md#what-startup-checks).

## Event codec (version 1)

Shared by the WAL and segments (`crates/storage/src/codec.rs`).

### Values

| Tag | Type   | Encoding                                |
|----:|--------|-----------------------------------------|
| 0   | null   | –                                       |
| 1   | false  | –                                       |
| 2   | true   | –                                       |
| 3   | int    | zigzag varint (i64)                     |
| 4   | float  | 8-byte IEEE-754                         |
| 5   | string | `str`                                   |
| 6   | array  | varint count, then values               |
| 7   | object | `fields`                                |

`fields` = varint count, then `(str key, value)` pairs in insertion order.

### Event

An event is a **head** followed by a **body**:

```
head := varint id
        i64    timestamp            (ns since Unix epoch)
        u8     kind                 1 log, 2 span, 3 metric
        u8     level                0 none, 1 Trace … 6 Fatal
        opt_str service
        opt_str environment
        opt_str trace_id
        opt_str span_id
        str    message              log message / span name / metric name

body := zigzag (observed_timestamp - timestamp)
        fields resource
        fields attributes
        payload
```

Payload by kind:

* **log**: `opt_str message_template`, `u8 has_exception` (0/1), and if 1:
  `opt_str type`, `opt_str message`, `opt_str stack_trace`.
* **span**: `opt_str parent_span_id`, `u8 span_kind` (0–5), `varint
  duration_ns`, `u8 status_code` (0 unset, 1 ok, 2 error), `opt_str
  status_message`, `varint n_events` × (`zigzag ts-delta`, `str name`,
  `fields attributes`), `varint n_links` × (`str trace_id`, `str span_id`,
  `fields attributes`).
* **metric**: `opt_str description`, `opt_str unit`, `u8 kind` (1 gauge, 2
  sum, 3 histogram), `u8 temporality` (0 unspecified, 1 delta, 2
  cumulative), `u8 monotonic`, `u8 value_tag`; tag 0 → `f64 value`; tag 1 →
  `varint count`, `u8 flags` (bit0 sum, bit1 min, bit2 max present), the
  present `f64`s in that order, `varint n` × `varint bucket_count`, `varint
  n` × `f64 bound`.

### Batch

```
batch := u8 codec_version (= 1) | varint count | event*
```

Trailing bytes after the last event are a decode error.

## Write-ahead log

```
file   := header record*
header := "VYRTWAL1" (8) | u16 format (=1) | u8 signal | 5 × reserved | u64 wal_id   (24 bytes)
record := u8 record_version (=1) | u32 length | payload[length] | u32 crc32
```

* `crc32` covers `record_version`, `length` and `payload`.
* Each record's payload is one encoded **batch**, i.e. one ingest request.
  A batch is therefore recovered entirely or not at all.
* Records larger than 256 MiB are rejected (treated as corruption on read).

**Creating a WAL file**: the header is written to `tmp/`, fsynced, renamed
into `wal/<signal>/` and the directory is fsynced. A WAL never has a torn
header.

**Appending**: the record is written as it is encoded, in 64 KiB pieces,
so neither the batch nor the record is held whole in memory (an encoded
batch can be far larger than its events, whose OTLP resource is shared in
memory but written out with each event). A first encoding pass takes the
length, so a batch over 256 MiB is refused before anything is written. On
failure, part-way or not, the writer truncates back to the last good offset
straight away (and again before the next append if that truncation fails
too), so a short write never leaves a torn record behind. The bytes are
the same as encoding the record whole: this is not a format change.

**Durability**:

* `strict` — after writing all records of a group commit, `fdatasync` the
  WAL, then acknowledge every request in the group.
* `normal` — acknowledge once the bytes are in the OS (survives a process
  crash), fsync every `fsync_interval` (default 1 s) and on rotation and
  shutdown (an OS crash or power loss can lose up to one interval).

### Acknowledgement contract

An *acknowledged* request is one whose ingest call returned success (HTTP
2xx). What survives depends on how the process stopped:

| Failure                                          | `strict`                                   | `normal`                                                       |
|--------------------------------------------------|--------------------------------------------|----------------------------------------------------------------|
| Clean shutdown (SIGTERM, Ctrl-C), drained in time | nothing lost; every accepted request is acknowledged first | nothing lost; every accepted request is acknowledged first |
| Final WAL fsync fails at shutdown (exit status 1) | cannot happen for acknowledged data (fsynced before acknowledgement) | nothing lost on restart; an OS crash or power loss before the OS writes its cache out can lose acknowledged requests since the last fsync |
| Shutdown deadline passed or stop signal repeated | nothing acknowledged is lost; unacknowledged requests get no response ([details](configuration.md#when-the-deadline-passes-the-signal-is-repeated-or-the-flush-fails)) | same as `strict`                                               |
| Process killed or crashed (`kill -9`, OOM, panic) | nothing lost                               | nothing lost — acknowledged bytes are already in the OS        |
| OS crash or power loss                           | nothing lost                               | up to `fsync_interval` of acknowledged requests may be lost    |
| Disk corrupts bytes after they were written      | the damaged record and everything after it in that WAL are quarantined, not replayed (see recovery) | same                                                           |

In every case, after recovery:

* each acknowledged event that survived is visible **exactly once**, with
  the event id it was acknowledged with — recovery never duplicates stored
  records (it does not deduplicate separate requests that a client
  retried);
* a request is visible entirely or not at all; the only unacknowledged
  requests that may appear are ones that reached the WAL but whose success
  response was never delivered (the process died first, or a `strict`
  fsync reported an error) — clients that retry such a request store it
  twice;
* nothing is fabricated from torn or corrupt bytes.

When `normal` mode loses an unsynced tail to power loss, later events may
reuse the ids of the lost ones (ids continue from the largest id that
survived). Use `strict` if a client stores event ids and needs them to stay
unique across power loss.

### Storage write failures

A request is acknowledged only after its batch was written to the WAL (and fsynced, where the contract above requires
it). When storage cannot write, the request gets **HTTP 503
`storage_write_failed`**. Nothing in it is acknowledged or visible to queries, and the response's `outcome` says whether
it can still turn up later:

* `notStored`: the batch never reached the WAL whole, or it did and was cut off again with the new WAL length fsynced.
  It cannot reappear, even after a power loss, so retrying is safe.
* `unknown`: the batch reached the WAL whole, its fsync failed, and the cut could be made but not fsynced. It stays
  unacknowledged, but after a power loss it may come back, and a retry can then store it twice.

| What fails                                                               | Effect on requests                                                                                                                                                                                         | WAL                                                                                                                                                                                                                                  |
|--------------------------------------------------------------------------|------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|--------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| WAL append (full disk, permission denied, read-only volume, short write) | 503; the batch is not stored                                                                                                                                                                               | partial bytes are cut off immediately                                                                                                                                                                                                |
| WAL fsync before acknowledging                                           | 503 for every request in the group commit: `notStored` if the cut below is durable, else `unknown`                                                                                                         | their records are cut from the WAL and the new length is fsynced before the response is sent. If that fsync fails, the cut stays pending: it is retried by the next fsync, which runs before any later acknowledgement and when idle |
| Background fsync (`normal` mode)                                         | earlier acknowledgements stand (they were already in the OS)                                                                                                                                               | until a later write *and* fsync succeed, `normal` mode also fsyncs before every acknowledgement, and a failed fsync means 503                                                                                                        |
| Creating the next WAL during rotation                                    | writing continues in the current WAL until the buffer must be sealed; then new requests get 503                                                                                                            | rotation is retried automatically; nothing is lost                                                                                                                                                                                   |
| Writing or fsyncing a segment (sealing)                                  | acknowledged data stays in its WAL; once the writer has to wait for that seal, new requests get 503 straight away, and requests already queued behind it get 503 too, instead of waiting for `ack_timeout` | the seal is retried every 5 s, and again on restart                                                                                                                                                                                  |

Each failure is recorded as a *fault* per signal, with the operation, a
reason (`noSpace`, `permissionDenied`, `readOnly`, `shortWrite`, `ioError`,
`other`), the file relative to the data directory and the OS error text.
Faults are listed in `GET /api/v1/system/storage` and logged once at ERROR
level (`storage write failed`; repeats are counted and logged at DEBUG). A
fault clears when the same part of the write path works again: a WAL write that is appended and synced, a segment that
is sealed, a maintenance run that completes. A WAL fault also clears without traffic: once a second, an idle writer
fsyncs its WAL (finishing any pending cut) and writes a 4 KiB probe file in its WAL directory, and if both work the
fault is over. Without this, an unready instance that an orchestrator sends no traffic to would never become ready
again. `GET /ready` is 503 while a WAL or segment fault is present, or while a 4 KiB write-and-fsync probe in `tmp/`
fails, which catches a full or read-only volume before ingest runs into it. That probe runs at most once per second
however often `/ready` is polled. Retention and compaction faults are reported but do not affect readiness.

Recovery works the same way as after a crash: restore free space or
permissions, then let Vyrtel retry by itself or restart it. Every
acknowledged event is still in a WAL or segment. A write failure never
causes anything to be quarantined.

Limitations:

* A volume that is already full or read-only when Vyrtel **starts** stops
  startup (taking the data-directory lock or replaying the WAL fails); it
  does not start in a degraded state.
* The probe writes to `tmp/` only. A permission problem limited to one
  signal's `wal/` or `segments/` directory shows up as a fault when a write
  is first attempted there.
* In `normal` mode, after a failed fsync some operating systems drop the
  dirty pages they could not write, and a later fsync can report success
  anyway. Requests acknowledged *before* the failure can then be lost on
  power loss. `strict` mode never acknowledges data whose fsync failed.
* A 503 with `outcome: "unknown"` can reappear after a power loss (not after a clean or crashed process: the cut is
  already in the OS). A client that retried it then has it twice.

## Segment files (format version 1)

```
┌──────────────────────────────┐ 0
│ Header (32 bytes)            │
├──────────────────────────────┤
│ Block 0: 6 column chunks     │ each chunk independently zstd-compressed
│ Block 1: 6 column chunks     │
│ ...                          │
├──────────────────────────────┤
│ Index region (zstd)          │ loaded lazily, cached under a byte budget
├──────────────────────────────┤
│ Summary region (zstd)        │ loaded at open, kept in memory
├──────────────────────────────┤
│ Footer (56 bytes)            │
└──────────────────────────────┘
```

### Header

| Offset | Size | Field                                |
|-------:|-----:|--------------------------------------|
|      0 |    8 | magic `VYRTSEG1`                     |
|      8 |    2 | format version (1)                   |
|     10 |    1 | signal (1 logs, 2 traces, 3 metrics) |
|     11 |    5 | reserved (0)                         |
|     16 |    8 | segment id                           |
|     24 |    8 | created at (ns)                      |

### Footer

| Offset | Size | Field                                                         |
|-------:|-----:|---------------------------------------------------------------|
|      0 |   20 | summary chunk ref (u64 offset, u32 len, u32 raw_len, u32 crc) |
|     20 |   20 | index chunk ref                                               |
|     40 |    2 | format version (1)                                            |
|     42 |    2 | reserved                                                      |
|     44 |    4 | CRC-32 of bytes 0..44                                         |
|     48 |    8 | magic `VYRTSEND`                                              |

### Blocks and columns

Events in a segment are **sorted by `(timestamp, id)`** and split into blocks
of at most `block_events` (default 1024) events or ~1 MiB of uncompressed
column data. Each block stores six column chunks, so a query decompresses
only the columns it needs:

| # | Column  | Content (per event unless noted)                                                                        |
|--:|---------|---------------------------------------------------------------------------------------------------------|
| 0 | `ts`    | first timestamp as `i64`, then zigzag-varint deltas (sorted → small)                                    |
| 1 | `ids`   | first id as varint, then zigzag-varint deltas                                                           |
| 2 | `meta`  | `u8 kind`, `u8 level`, varint service ref, varint environment ref (0 = none, else dictionary index + 1) |
| 3 | `trace` | `opt_str trace_id`, `opt_str span_id`                                                                   |
| 4 | `msg`   | `str` message / span name / metric name                                                                 |
| 5 | `body`  | `varint length` + event **body** (see codec)                                                            |

Each chunk is zstd-compressed (level 3 by default). Its chunk ref (offset,
compressed length, raw length, CRC-32 of the *compressed* bytes) lives in the
summary's block directory. Readers verify the CRC **before** decompressing
and check the decompressed length.

### Summary region (resident)

```
u64 id | u8 signal | u16 format | i64 created_at
i64 min_ts | i64 max_ts | u64 min_event_id | u64 max_event_id | u64 event_count
u64 raw_bytes | u64 data_bytes
varint n × u64              replaces (compaction inputs)
varint n × str              service dictionary
varint n × str              environment dictionary
varint 7 × bitmap           blocks containing each level code 0..6
varint n × bitmap           blocks containing each service (dictionary order)
varint n × bitmap           blocks containing each environment
u8 has_names [varint n × str]   distinct span/metric names (≤ 4096), else absent
varint n_blocks × (varint count, i64 min_ts, i64 max_ts, 6 × chunk_ref)
chunk_ref := varint offset | varint len | varint raw_len | u32 crc
bitmap    := varint n_words | n_words × u64
```

`index_bytes` (file size − column data) is derived at open, not stored.

### Index region (lazy)

```
varint n × str                         zone-map fields
varint n_blocks × block_index
varint n × field_stats

block_index := bloom | varint n × (u16 zone_field, f64 min, f64 max)
bloom       := u8 k | varint n_words | n_words × u64
field_stats := str path | varint present | u8 type_bits | u8 flags | [f64 min] [f64 max] | 64-byte HLL
```

### Indexes

* **Time** — segment and block `min_ts`/`max_ts`; whole segments and blocks
  outside the query range are skipped without reading.
* **Bitmap indexes** — for level, service and environment: one bitmap over
  blocks per value. A comparison is evaluated against each dictionary value
  with the query evaluator itself, and the matching bitmaps are OR-ed.
* **Bloom filters** — one per block, ~10 bits/key (≈1 % false positives),
  `k = round(bits_per_key · ln 2)`, Kirsch–Mitzenmacher double hashing over a
  64-bit **xxh3** hash. Keys are canonical `(path, value)` pairs for every
  scalar leaf of attributes *and* resource attributes (array elements under
  the array's path), plus `traceId`, `spanId`, `parentSpanId`,
  `messageTemplate`, `exception.type`, and span/metric `name`, `status`,
  `spanKind`.
* **Zone maps** — per block min/max for up to 64 numeric fields per segment
  (the most frequent; plus `durationMs` for spans and `value` for metrics).
  A *tracked* field with no zone entry in a block has no numeric values
  there, so numeric comparisons skip the block.
* **Field stats** — per field (top 256 by presence): presence count, observed
  types, numeric min/max, HyperLogLog (p = 6) distinct estimate.

Canonical keys (`crates/storage/src/index/keys.rs`) make Bloom lookups agree
with query semantics: a key is `xxh3(path ‖ 0xFF ‖ tag ‖ bytes)` where tag is
`s` (string), `i`/`f` (canonical number: integral values are always `i`), or
`b` (bool). A stored string that parses as a number also gets a number key,
and `"true"`/`"false"` (any case) also get a bool key, mirroring the
evaluator's coercions. Indexes may have false positives; **they must never
have false negatives**, which `crates/query/tests/correctness.rs` checks by
comparing indexed results with brute-force evaluation.

## Commit procedure

Sealing the active buffer (rotation by size, age or event count):

1. Writer thread: fsync the current WAL `N`, create WAL `N+1` (atomic, see
   above), move the in-memory buffer to *frozen*, continue on `N+1`.
2. Maintenance thread: sort frozen events, write segment `N` to `tmp/`,
   fsync it.
3. **Commit point**: rename into `segments/<signal>/N.seg`, fsync the
   directory.
4. Delete WAL `N`, fsync the WAL directory.
5. Atomically (under the stream lock) add the segment to the catalog and
   drop the frozen buffer, so queries never see both or neither.

At most one frozen buffer exists. If sealing falls behind, the writer
waits, the ingest queue fills and clients receive HTTP 429. If sealing
*fails* (e.g. disk full), new requests get HTTP 503 `storage_write_failed`
as soon as the writer has to wait for it (see
[Storage write failures](#storage-write-failures)). Either way, data
already acknowledged stays safe in the WAL.

Compaction merges small segments (raw size < ¼ of the segment target, at
least 4 of them) into a new segment whose summary lists the inputs in
`replaces`. After the merged segment is committed, the inputs are removed
from the catalog and deleted.

Retention deletes whole segments whose `max_ts` is older than the signal's
retention period: remove from the catalog first, then unlink.

## Recovery procedure

On startup, per stream (`crates/storage/src/recovery.rs`):

1. Delete everything in `tmp/` (never committed).
2. Open every `*.seg`. A segment that fails validation (magic, version,
   footer CRC, summary CRC, bounds, id/signal mismatch) is moved to
   `quarantine/<signal>/` and logged.
3. Segments listed in another segment's `replaces` are deleted (a
   compaction committed but its cleanup was interrupted).
4. For each WAL file in id order:
   * if segment `N` exists (or was replaced), the WAL is stale → delete it;
   * a WAL whose header is unreadable, or names another signal or id, is
     moved to `quarantine/<signal>/` whole;
   * otherwise replay records until the first incomplete or corrupt record.
     The unusable tail is copied to `quarantine/<signal>/` and fsynced
     **before** the WAL is truncated to the last valid record. Nothing after
     a bad record is interpreted.
5. Every replayed WAL except the newest is sealed into a segment
   immediately; the newest becomes the active WAL.
6. The next file id is one past the largest id seen anywhere; the next event
   id is one past the largest event id seen.

Recovery quarantines only on **proof of damage** (bad checksum, bad
framing, truncation, undecodable content). Any other I/O error while reading
a segment or WAL — permission denied, a failing device — stops startup with
the error instead: moving a readable file aside would hide acknowledged
telemetry. Fix the cause and start again. A file in a format version this
build cannot read is not damage either: startup stops with
`StorageError::UnsupportedFormat` naming the file and leaves it as it is
(see [Versioning](#versioning)).

Every recovery is idempotent: crashing during recovery (for example after
sealing an old WAL but before deleting it) and recovering again gives the
same result.

Guarantees, covered by `crates/storage/tests/recovery.rs` (file states a
crash leaves) and `crates/storage/tests/crash.rs` (real process kills at
every step, see [Testing](testing.md#crash-recovery-tests)):

* committed telemetry never disappears because of a restart;
* incomplete or corrupt records never appear as telemetry;
* no event is duplicated by a crash between any two steps above.

### Quarantine

Damaged data is never deleted by recovery. Each item is preserved under
`quarantine/<signal>/` — the directory names the affected signal:

| File                                      | Kind      | Contents                                                       |
|-------------------------------------------|-----------|----------------------------------------------------------------|
| `<id>.seg.<unix-ms>`                      | `segment` | the whole damaged segment                                      |
| `<id>.wal.<unix-ms>`                      | `wal`     | the whole WAL (unreadable header, or header for another stream) |
| `<id>.wal.tail-<offset>.<unix-ms>`        | `walTail` | the exact bytes after the last valid record at `offset`       |

A `-<n>` suffix is added if a name is already taken, so a later crash never
overwrites earlier evidence. Newly created directories (including
`quarantine/<signal>/` itself) are fsynced into their parent, so evidence
written into them survives power loss too. Each item is logged once at startup (`ERROR`
for whole files, `WARN` for tails) with `signal`, `kind`, `file`,
`quarantined_to`, `bytes` and `reason`, and is listed under
`signals.<signal>.recovery.quarantined` in
[`GET /api/v1/system/storage`](api.md#system) until the next restart.
After that, and after a recovery interrupted once it had set a file aside,
the files are still counted in `signals.<signal>.recovery.quarantineDir` and
the signal's status is `quarantineRetained` until they are deleted, or
`unknown` while the directory cannot be read (see
[Recovery health](configuration.md#recovery-health)).

What was lost:

* `walTail` — only the records in the tail; everything before `offset` was
  replayed.
* `wal` — every event in that WAL.
* `segment` — rebuilt from its WAL if the WAL still existed (the event count
  is unchanged); otherwise its events are not visible.

Quarantined files are never read again by Vyrtel. Keep them for diagnosis
(`walTail` files are raw WAL records, see the format above) and delete them
when no longer needed; `quarantineBytes` in the storage statistics shows how
much space they use.

## Versioning

* WAL file format, WAL record version, codec version and segment format
  version are independent. Readers reject unknown versions explicitly.
* A future format change adds a new version number and keeps the old
  decoder, so existing data directories stay readable. Rust struct layout is
  never written to disk.
* Any change to these versions, the metadata schema or the directory layout
  also raises the manifest `format`, so an older build refuses a whole
  directory from a newer one before reading any data file.
* Observer 0.1.0, Vyrtel's first release under its earlier name, wrote
  the same layouts with the magic bytes `OBSVWAL1` (WAL), `OBSVSEG1` and
  `OBSVSEND` (segment). Such a file is not read and not quarantined:
  `check_formats` and recovery stop with `StorageError::ObserverFormat`.
* An unknown version is a reason to stop, never to quarantine. Where a
  version is not covered by a checksum, it is believed only when nothing
  checksummed contradicts it:

  | Where                          | Unknown version means                                                                                         |
  |--------------------------------|----------------------------------------------------------------------------------------------------------------|
  | Segment footer or summary      | newer format (both are checked before their version is read)                                                  |
  | Segment header                 | newer format, unless the footer's checksum holds and says version 1: then the header is damaged (quarantined) |
  | WAL header (has the WAL magic) | newer format                                                                                                    |
  | WAL record version byte        | newer format if the record's CRC holds; otherwise a corrupt tail, as before                                  |
  | Batch codec version            | newer format (the record's CRC held)                                                                           |

  All of these are checked before startup changes anything, so a newer
  file in one signal stops startup before any signal is recovered or the
  metadata is migrated. The check holds at most 64 KiB of any file in
  memory and decodes no event, column or index:

  * segment: header and footer, then the summary's checksum, streamed in
    64 KiB pieces, and only if it holds, the first 11 decompressed bytes
    (segment id, signal, summary version);
  * WAL: the header, then per record its 5-byte prefix and the first
    payload byte (the batch codec version); the rest of the record is
    skipped. Only when one of those versions is unknown is the record's
    checksum computed (streamed), since an unknown version is believed only
    if it holds. A known record is not checksummed here, so the check may
    look past a corrupt record where replay stops; it can only refuse more.

  The check runs twice per start, before and after taking the
  data-directory lock, then recovery reads the WALs in full. Recovery
  checks the versions again while replaying, before a WAL is truncated or
  sealed.

### Migrations

A start migrates a data directory in this order, after the read-only
startup check passed and the lock is held (`App::build`; the steps are
`datadir::plan`, and `vyrtel check-data-dir` prints them without running
them):

1. Metadata schema migrations (`crates/metadata/src/migrations.rs`), each
   in its own SQLite transaction together with its `schema_migrations` row.
   A failure or crash rolls back only the migration in progress; the next
   open continues from the last committed version. A failure is
   `MetadataError::Migration { version, at }`.
2. `vyrtel.json` records the current `format` and `writtenBy`.
3. Storage recovery and normal operation.

Rules for future format changes, so that every step stays safe to
interrupt and an older build never misreads the result:

* Raise the manifest `format` and add a [`datadir::RELEASES`](../crates/server/src/datadir.rs)
  row with the release. The format is recorded (step 2) before any
  telemetry file is written in the new format, so after an interrupted
  upgrade a build that reads only the old format refuses the directory.
* Keep the decoder for every older version, so existing files never have
  to be rewritten. A change that must rewrite files does it after step 2,
  one file at a time through `tmp/` and an atomic rename, so a restart
  finds each file either old or new and continues.
* Append metadata migrations; never edit an applied one. A migration that
  older releases cannot read is a format change for downgrade purposes:
  `check-data-dir` reports which releases can read the result.
* Add a fixture of the release's data directory
  (`tests/fixtures/datadir/`), so every later build is tested against it.
