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

**Appending**: the record is written with a single `write_all`. On failure
the writer truncates back to the last good offset before the next append.

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
| Clean shutdown (SIGTERM, Ctrl-C)                 | nothing lost                               | nothing lost                                                   |
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

At most one frozen buffer exists. If sealing falls behind or fails (e.g.
disk full) the writer waits, the ingest queue fills and clients receive
HTTP 429 — data already acknowledged stays safe in the WAL.

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
telemetry. Fix the cause and start again.

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
overwrites earlier evidence. Each item is logged once at startup (`ERROR`
for whole files, `WARN` for tails) with `signal`, `kind`, `file`,
`quarantined_to`, `bytes` and `reason`, and is listed under
`signals.<signal>.recovery.quarantined` in
[`GET /api/v1/system/storage`](api.md#system) until the next restart.

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
