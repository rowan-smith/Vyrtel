# Testing

Testing is part of the implementation. Everything below runs in CI
(`.github/workflows/ci.yml`).

```bash
cargo test --workspace --all-features    # all Rust tests (~220, many property-based; includes crash and overload tests)
cd web && npm test                       # frontend unit tests
cd web && npm run e2e                    # Playwright (needs a built server, see below)
cargo bench -p storage -p query          # Criterion benchmarks
node --test scripts/release/*.test.mjs        # release gate (see docs/releasing.md)
```

## Unit tests

Next to the code (`#[cfg(test)]` modules). Highlights:

| Area                                                                 | Where                                |
|----------------------------------------------------------------------|--------------------------------------|
| Query tokenizer, parser (precedence, errors, positions), AST display | `crates/query/src/{lexer,parser}.rs` |
| Comparison evaluation and type coercion rules                        | `crates/query/src/eval.rs`           |
| Field resolution                                                     | `crates/query/src/fields.rs`         |
| Timestamp parsing (RFC 3339, Unix units, precision)                  | `crates/telemetry/src/time.rs`       |
| Varints, zigzag, bounds checks                                       | `crates/storage/src/encoding.rs`     |
| Event codec, versioning, garbage/truncation handling                 | `crates/storage/src/codec.rs`        |
| WAL encoding/decoding, CRC validation, torn records                  | `crates/storage/src/wal.rs`          |
| Segment write/read, metadata, corruption detection                   | `crates/storage/src/segment/mod.rs`  |
| Bloom filters, bitmaps, HyperLogLog, canonical index keys            | `crates/storage/src/index/`          |
| Configuration parsing, env overrides, memory budgets                 | `crates/server/src/config.rs`        |
| OTLP mapping (protobuf + JSON), native/CLEF parsing, templates       | `crates/ingest/src/`                 |
| Metadata store (migrations, CRUD, alert transitions, read-only schema inspection, newer schema refused) | `crates/metadata/tests/store.rs`     |
| Data-directory check: new, pre-manifest and interrupted directories accepted; missing parts, inventory files missing or truncated, an empty metadata database, bad or newer manifests, newer and unreadable metadata refused; `accept_incomplete` returns the problems instead (never format problems); the inventory is dropped on start | `crates/server/src/datadir.rs` |

## Property tests (proptest)

| Property                                                                                                          | Where                             |
|-------------------------------------------------------------------------------------------------------------------|-----------------------------------|
| `encode(event) → decode() == event` for arbitrary logs, spans, metrics                                            | `storage::codec`                  |
| arbitrary bytes never panic the decoder; truncation is always detected                                            | `storage::codec`                  |
| random WAL records → replay → identical records; a cut at any byte replays exactly the complete records before it | `storage::wal`                    |
| segment writing → reading → identical event set (random block sizes)                                              | `storage::segment`                |
| arbitrary valid AST → display → parse → identical AST; parser never panics                                        | `crates/query/tests/roundtrip.rs` |

Event generators live behind the `telemetry/testing` feature so all crates
share them.

## Crash-recovery tests

`crates/storage/tests/recovery.rs` simulates crashes by leaving files in the
states an interrupted process would:

* empty WAL; valid WAL replayed after restart (and ids continue);
* truncated final WAL record → dropped, tail preserved in quarantine,
  subsequent appends clean;
* invalid checksum → replay stops there, nothing after it is interpreted;
* temporary segment file left in `tmp/` → deleted;
* partial/damaged segment in `segments/` → quarantined, rebuilt from its WAL;
* restart after segment commit but before WAL deletion → no duplicates;
* restart during compaction (output committed, inputs still present) → no
  duplicates;
* several WALs at startup → older ones sealed;
* retention across restart, queue-full backpressure, strict durability,
  data-directory locking;
* quarantine evidence: a torn or corrupt final WAL record is preserved byte
  for byte under `quarantine/<signal>/`, the report names signal, kind and
  reason, and a second restart neither re-quarantines nor duplicates; damaged
  segments and WALs with bad or foreign headers are moved whole; reasons
  never contain host paths;
* an unreadable-but-undamaged WAL or segment (mode 000 on Unix, an exclusive
  share lock on Windows) stops startup with an I/O error naming the file,
  leaves it byte-for-byte in place and quarantines nothing; once readable,
  the acknowledged events come back (skipped when running as root, which
  bypasses permissions);
* a file in a newer format is not damage (REL-06): a segment whose header
  and checksummed footer say version 2, a WAL whose header says version 3,
  and a checksummed WAL record with an unknown record version or batch
  codec each stop startup with `UnsupportedFormat` naming the file, with
  every file in the data directory unchanged; `check_formats` finds the
  header cases without opening storage; a segment whose header alone
  disagrees with its footer, and an unknown record version without a valid
  checksum, are still quarantined as damage.

### Process-kill tests

`crates/storage/tests/crash.rs` kills a **real process** mid-write. The test
binary re-invokes itself as a child that ingests with several concurrent
producers and keeps a durable client-side ledger (one fsynced line per
acknowledged batch). The child is stopped either at a named crash point —
`VYRTEL_CRASH_AT=<point>[:<n>]` makes the `crash-points` build `abort()` the
n-th time it reaches `point`, which is as abrupt as `kill -9` — or by
`kill`/`TerminateProcess` after a random delay. The parent then recovers the
directory and checks, in both `strict` and `normal` durability:

* every ledgered batch is visible exactly once with the ids it was
  acknowledged with;
* batches are whole; per producer they are contiguous and exceed the ledger
  by at most the one batch in flight;
* sealing/compaction crashes leave visible counts equal to the acknowledged
  dataset, with nothing quarantined;
* a second recovery is identical and new ids do not collide.

| Test                              | Crash points                                                                                       |
|-----------------------------------|----------------------------------------------------------------------------------------------------|
| `kill_before_wal_sync`            | `wal.before_sync` — records written, not synced or acknowledged                                    |
| `kill_after_wal_sync_before_ack`  | `wal.before_ack` — records synced (strict), not acknowledged                                       |
| `kill_after_ack`                  | `wal.after_ack`                                                                                    |
| `kill_during_rotation_and_seal`   | `rotate.after_wal_create`, `segment.before_rename`, `segment.after_rename`, `seal.before_wal_delete` |
| `kill_during_compaction`          | `compact.before_input_delete`, `compact.mid_input_delete`, `segment.before_rename` (compaction output) |
| `kill_during_recovery_seal`       | a crash during rotation, then `recovery.before_wal_delete` while recovering from it                 |
| `random_kills_under_load`         | four kill/restart cycles on one directory at random moments (seed printed; `VYRTEL_CRASH_SEED` replays) |

The points are listed in `crates/storage/src/crash.rs`; without the
`crash-points` feature they compile to nothing. Run just these tests with:

```bash
cargo test -p storage --features crash-points --test crash
```

`tests/integration/crash.rs` does the same end to end: it kills the real
`vyrtel` binary after HTTP acknowledgements (both durability modes),
restarts it and checks every acknowledged event through the query API, plus
the `recovery` section of `/api/v1/system/storage`.

## Storage write-failure tests

`crates/storage/tests/faults.rs` injects storage errors at named points in
the write path. With the test-only `fault-injection` feature,
`storage::fault::inject(dir, point, fault)` makes `point` fail for files
below `dir` until the returned guard is dropped. The points are
`wal.append`, `wal.sync`, `wal.create`, `segment.write`, `segment.sync` and
`probe`. The faults are full disk, permission denied, read-only and short
write (half the bytes, then an error). Without the feature the checks
compile to nothing. The tests check that:

* a failed WAL append (each fault, both durability modes) gets
  `WriteFailed`, is not visible, and leaves no torn record: after a restart
  exactly the acknowledged events are back and nothing is quarantined;
* a failed `strict` fsync rejects the batch, cuts it from the WAL and fsyncs that cut before replying (`notStored`). The
  order is checked with
  `storage::fault::inject::record(dir)`, which lists the fault points reached, because a process restart cannot show
  lost filesystem metadata the way a power loss would;
* if the cut's fsync fails too, the reply is `WriteUncertain` (`unknown`), the idle writer fsyncs the cut on its own
  once storage works (no request needed), and any later acknowledgement waits for that fsync;
* a WAL fault clears without new requests once storage works, but not while writes in the WAL directory still fail;
* after a failed background fsync, `normal` mode waits for an fsync before
  acknowledging, and rejects requests while it keeps failing;
* a failing seal (segment write or fsync) makes new batches fail fast once
  the writer has to wait, and every acknowledged batch is sealed after a
  restart; batches already queued when the writer starts waiting (a burst
  of 200 without pauses) are answered within 2 s, stored or `WriteFailed`
  with the seal's fault, instead of waiting for their acknowledgement
  timeout, and release their queue reservation;
* a failed WAL creation stalls rotation, and ingest resumes by itself once
  storage works;
* faults name the operation, the reason and a data-directory-relative path,
  clear once that part of the write path works again, and never contain the
  host path;
* the write probe reports full, read-only and permission-denied storage,
  and on Unix a real read-only directory (skipped as root).

```bash
cargo test -p storage --features fault-injection --test faults
```

## Storage correctness (indexed vs brute force)

`crates/query/tests/correctness.rs` generates messy datasets (the same field
as int/string/float, nested vs dotted keys, arrays, booleans as strings,
resource fallbacks), stores them across several segments with tiny blocks
and only a few zone-mapped fields, then runs **hundreds of random queries**
and asserts that:

* indexed `count` and `search` return exactly the brute-force result set;
* cursor pagination (both directions, random page sizes) walks exactly that
  set;
* histogram totals equal counts.

It also guards against vacuous passes: many queries must match something and
many must actually have been pruned by indexes. False positives are fine;
false negatives fail the test.

## Integration tests

`tests/integration/` (built as the `server` crate's `integration` test)
starts a real server on an ephemeral port with a temporary data directory and
talks HTTP to it:

* start → ingest → query → restart → query the same events;
* `kill -9` of the `vyrtel` binary after acknowledgements → restart → every
  acknowledged event exactly once; quarantined WAL tails reported by the
  storage API;
* ingest → rotate segment → query (diagnostics show segment pruning);
* OTLP logs (JSON and gzip protobuf) queried through the native API;
* OTLP spans → trace search → trace by id → correlated logs;
* OTLP metrics → names → sum/count/max/avg with group-by and filters;
* retention → restart → verify retained/deleted data;
* API errors: invalid JSON, invalid events, invalid queries (with position),
  oversized requests, unsupported media types, unknown endpoints;
* ingest size and encoding (`ingest_limits.rs`): decoded sizes of limit−1,
  limit and limit+1 bytes on native JSON, NDJSON, OTLP JSON and OTLP
  protobuf, plain and gzip (only the first two stored); a gzip bomb gets 413
  while its upload is still in progress; an oversized `Content-Length` gets
  413 before the body is read; clients that keep sending 8 MB against the
  64 KB limit (with `Content-Length`, chunked, and gzip past the wire limit)
  read the 413, not a broken pipe, and their next request succeeds; empty gzip members cannot exceed the limit on
  the wire; truncated, corrupt and trailing-garbage gzip get
  `invalid_encoding`, unsupported encodings a structured 415; concatenated
  members are all stored; rejected native batches (malformed final record,
  truncated gzip, over the limit) store nothing; OTLP `partialSuccess` is the
  documented partial outcome. Unit tests in `routes/ingest.rs` show decoding
  stops after a small fraction of a 256 MB bomb;
* ingest admission (`admission.rs`): a request holds its slot while parsed
  and queued, excess requests (plain and gzip) get 429 + `Retry-After`
  promptly without their bodies being read, every error frees the slot, and
  so does a client disconnecting while the body is read, while it is parsed
  and while the write awaits acknowledgement (the writer thread is parked
  with `stall_writer` to hold a request there); with
  `max_concurrent = 0` the derived limit (3 at the defaults) is what is
  enforced and what `system/info` reports, and an explicit limit over the
  estimated budget is honoured (REL-14);
* storage write failures (`storage_faults.rs`, needs `--features
  fault-injection`): native, NDJSON and OTLP logs, traces and metrics get
  `503 storage_write_failed` with a reason and store nothing while the WAL cannot write; `/ready` is 503, names the
  signals and recovers without a restart, and also with no new traffic at all;
  `outcome` is `notStored`, or `unknown` when the rollback's fsync is denied too; a failing write probe (full or
  read-only) makes `/ready` report
  `storageWritable: false`; `/api/v1/system/storage` shows the fault without
  the host path; after a short write and a denied fsync, a restart brings
  back exactly the pre-fault dataset (`logs.ndjson` plus one event) with
  nothing quarantined;
* missing auth, bad auth, ingest-only keys, sessions, logout, revocation;
* pagination, time ranges, empty results, facets, histograms;
* live stream (SSE) delivery and filtering, and that open streams do not
  block shutdown;
* graceful shutdown (`shutdown.rs`, REL-05): with the queue full and the
  writer parked, a stop request waits for all 4 admitted requests, refuses
  new work (closed listener or `503 shutting_down`) and exits cleanly; all
  43 acknowledged events are back exactly once after a restart; an idle
  stop takes well under 2 s; when `server.shutdown_timeout` (500 ms) passes
  with a request unacknowledged, or a second stop request arrives, shutdown
  gives up on time and `IncompleteShutdown` names the request and its 5
  queued events; the deadline also bounds a stuck final storage flush;
  a seal blocked at the deadline, or ended by a second stop request, is
  reported as the unfinished phase and its events are back after a
  restart; a second stop request or the deadline also ends a blocked
  background job (`app.rs` unit tests); with `--features fault-injection`,
  a failed final WAL fsync in `normal` durability is reported with its
  fault instead of a clean shutdown (`storage_faults.rs`);
  live-tail streams, including one whose client has not read 300 events,
  end with `shutdown` and `retry: 2000` and a clean end of body, and a
  client reconnecting to the same address after a restart receives new
  events. On Unix the `process` tests send real SIGTERMs to the binary: an
  upload in flight is answered `200` before exit status 0; past a 1 s
  deadline it exits with status 1 within about a second and prints the
  diagnostic; a second SIGTERM exits at once. Unit tests in `app.rs` check
  each refused route and the diagnostic text, and `web/tests/unit/live-tail.test.ts`
  the UI's reconnection and backoff;
* dashboards, saved queries, alerts firing/resolving with webhook delivery,
  the background evaluator;
* backup and restore (`backup.rs`, REL-06): a stopped instance with all
  three signals (in segments and in the WAL), a dashboard with a panel, an
  alert, a saved query, an active and a revoked API key and an admin
  session is copied, the original deleted, and the copy started elsewhere
  without `auth.admin_password`; every
  [verification query](configuration.md#verification-queries) answers as
  before, the restored key ingests, the revoked key and anonymous requests
  get 401, the old session is valid, and nothing is quarantined. Backups
  without `metadata/metadata.db` (with and without a manifest), with an
  empty one, without a signal's WAL directory, without one populated
  `.wal` and one `.seg` file listed in the clean-shutdown inventory, with a
  truncated `.seg`, with a newer or unreadable manifest, a newer segment or
  WAL format, or a newer metadata schema are refused by `App::build` with
  the documented message, and every file and directory is unchanged (no
  lock file, no `tmp/`). The real binary exits with status 1 and prints
  the reason. `--accept-incomplete-restore` (`StartOptions`) starts a copy
  without a WAL file, with the segment events and the configuration kept,
  and the next start needs no flag; a running directory has no inventory.
  A directory without a manifest (0.1.1) starts and gets one; a new volume
  holding only `lost+found` starts as new. A start refused because another
  process holds the lock changes nothing, so the inventory still refuses a
  later start that is missing a WAL file. With `--features
  fault-injection` (`storage_faults.rs`): a failed manifest write at
  shutdown is `IncompleteShutdown` (phase `Inventory`, the I/O error, no
  host path, no partial inventory) and the next clean shutdown records it;
  a manifest write stalled past a 500 ms `shutdown_timeout` ends the
  shutdown within 3 s, reported as the deadline in phase `Inventory`;
* upgrades (`upgrade.rs`, REL-07), against the data directories the 0.1.0
  (Observer) and 0.1.1 releases wrote (`tests/fixtures/datadir/`, restored
  byte for byte from the sizes and SHA-256 sums in each `fixture.json`).
  For 0.1.1:
  `vyrtel check-data-dir` exits 0, changes nothing and prints the plan
  (record `vyrtel.json`, readable by 0.1.1 and later afterwards); the
  started server answers every verification query exactly as that release
  did before it was stopped, its ingest key works, its revoked key and
  anonymous requests get 401, nothing is quarantined, `vyrtel.json` is
  recorded, and a second start migrates nothing. The Observer 0.1.0
  directory is refused by `check-data-dir` and by `App::build` with what to
  do, and nothing is quarantined (0.1.1 quarantined all 5 of its files;
  storage unit tests `observer_files_stop_startup_and_are_kept`). A newer manifest format,
  a newer metadata schema, and a checksummed WAL record in a newer format
  in `metrics` while the `logs` WAL has a torn tail are each refused by
  `check-data-dir` (exit 1) and by `App::build` with the supported versions
  named and every file unchanged (before REL-07, the `logs` tail was
  truncated first). With `--features fault-injection`, the fault points the
  upgrade start reaches are recorded, then each one in turn fails once:
  a failed `vyrtel.json` write leaves every telemetry file unchanged, and
  in every case the next start completes the upgrade and answers the
  verification queries as the release did. Unit tests: an interrupted
  metadata migration rolls back only itself and the next open resumes
  (`crates/metadata/src/lib.rs`); `datadir::plan`, the `check-data-dir`
  report and the `RELEASES` table (`crates/server/src/datadir.rs`); WAL
  record and codec versions are refused by `storage::check_formats` while a
  bad-checksum unknown record is still a corrupt tail
  (`crates/storage/tests/recovery.rs`). New fixtures are written by the
  ignored `upgrade::make_datadir_fixture` test with a released binary
  ([tests/fixtures/datadir/README.md](../tests/fixtures/datadir/README.md)).

Fixtures shared with E2E live in `tests/fixtures/`.

### Restore drill

`scripts/backup/restore-drill.sh` runs the published
[backup and restore procedure](configuration.md#backup-and-restore) against
a built binary (Linux, macOS): it populates an instance from
`tests/fixtures`, stops it with SIGTERM, takes a `tar` backup, deletes the
original, restores into a new directory, starts without an admin password,
diffs every verification query against its answers before the backup,
checks the restored API key, anonymous refusal and login, and checks that
copies without `metadata/metadata.db`, with an empty one, without one WAL
file and without one segment file each exit with status 1 and are left
unchanged, and that `--accept-incomplete-restore` starts the copy without
the WAL file. CI runs it against the release build in the backend job.

```bash
cargo build --release -p server && scripts/backup/restore-drill.sh target/release/vyrtel
```

### Overload memory test

`tests/overload/main.rs` (the `server` crate's `overload` test, its own
process because it installs a counting global allocator) holds the ingest
path to a memory budget under sustained overload. Fixed limits:
`max_memory = 64MB`, `max_request_size = 2MB`, `ingest.max_concurrent = 2`;
32 clients send 1.79 MB NDJSON batches (half gzip-compressed, 0.05 MB on the
wire) for 4 s, retrying 429s after 5 ms. It asserts that:

* peak heap growth stays below `max_memory`;
* no more than 2 requests are ever in flight and the server keeps accepting;
* every rejection is a 429 within 1 s and no client sees a reset connection.

```bash
cargo test -p server --test overload -- --nocapture
```

One run on the Windows 11 laptop used for the benchmarks below (test
profile), before and after REL-02 moved admission from "while the body is
read" to "until the write is acknowledged":

| Metric                      | Before                | After               |
|-----------------------------|-----------------------|---------------------|
| Peak heap growth            | 298.4 MB (4.7× budget) | 55.3 MB (< 64 MB)  |
| Slowest 429                 | 1.1 s                 | 161 ms              |
| Connection resets           | 223                   | 0                   |
| Batches accepted in 4 s     | 14                    | 40                  |

Five consecutive runs after the change peaked at 54.3–55.8 MB. Parsing one
1.79 MB body alone peaks at +8.3 MB, and one request end to end at +10.6 MB.

`flood_of_rejected_slow_uploads_is_bounded`, in the same binary, holds the
only ingest slot and opens 600 uploads that announce 1 MB, send 64 KB and
stall. All are rejected at admission. It asserts that rejected bodies kept
for discarding stop at the 256 cap (32 being read), that the runtime gains
no more than one task per connection plus that cap, that kept clients read
the 429, that peak heap growth stays below `max_memory`, and that everything
is released after the 10 s discard timeout. One run: 256 discard tasks,
512 new runtime tasks, 18.2 MB peak heap growth; without the cap, 1,200 new
tasks and 38.2 MB.

The two REL-02 tests set `ingest.max_concurrent` explicitly, so they are
unaffected by the derived sizing below (and log its over-budget warning).

### Ingest amplification and derived admission

REL-14 replaced the derived ingest limit with one budgeted from measured
parse amplification ([formula](configuration.md#ingest-memory)). Three
tests in the same `overload` binary back it. Clients run on a separate
runtime whose allocations the counting allocator skips, so only the
server's heap is measured. Run them on a release build; the figures below
are from one Windows 11 x86_64 laptop (`cargo test --release`, Rust 1.96),
so expect other machines to differ somewhat.

```bash
cargo test -p server --release --test overload -- --nocapture --test-threads 1
# the pre-REL-14 comparison (needs ~2 GB of memory):
cargo test -p server --release --test overload pre_rel14 -- --nocapture --ignored
```

**Fixtures** (`tests/overload/fixtures.rs`). Every body is generated from
seed `0x5EED0014` (xorshift), so reruns are byte-identical. Each one is
filled to just under the target decoded size (2 MB by default;
`VYRTEL_AMPLIFICATION_TARGET=<bytes>` changes it) and sent plain and gzip.
OTLP fixtures are built once as protobuf structs and encoded both ways, so
both encodings carry the same records.

| Fixture | Record shape | Events in 2 MB |
|---|---|---:|
| native JSON / NDJSON representative | app event, level, service, trace ids, 5 properties (~330 B) | 6,150 |
| CLEF representative | Serilog request log, template and 7 properties, 10% with `@x` | 5,940 |
| NDJSON string-heavy | 12 KB of strings per event | 173 |
| native JSON / NDJSON nested properties | 6 levels, ~40 small values | ~7,950 |
| native JSON / NDJSON many small events | `{"message":"…"}` (~30 B) | 87,626 |
| NDJSON empty objects | `{}` | 699,050 |
| OTLP logs representative | SDK resource (7 attributes), body, 4 attributes, trace ids | 9,078 protobuf / 4,017 JSON |
| OTLP logs many small records | body only (~20 B) | 131,072 / 13,189 |
| OTLP logs large shared resource | 200-attribute resource, tiny records | 15,887 / 5,745 |
| OTLP traces representative | 5 attributes, 1 in 8 with an event, 1 in 10 failed | 8,594 / 3,432 |
| OTLP metrics representative | gauges and sums (2 points), histograms (11 buckets) | 11,650 / 5,287 data points |

**Per-format peaks** (`amplification_per_format_is_within_the_admission_estimate`).
For every fixture and encoding it measures, as multiples of the decoded
body: *parse*, decoding alone in-process over a body already in memory;
*admission*, from sending the request until its batch is in the storage
queue, with the writer parked (body read and gzip-decoded, parse, parsed
batch: what an admitted request owns); and *end to end*, until the
response, which adds the WAL record and the active segment (storage's
budget). A fresh server per request keeps earlier writes out of the
window. It asserts that every fixture inside the supported envelope stays
at or under the 20× estimate.

| Fixture | Admission ×body, plain / gzip | End to end ×body | In the 20× envelope |
|---|---:|---:|:---:|
| native JSON representative | 9.42 / 9.35 | 9.42 | yes |
| NDJSON representative | 9.42 / 9.35 | 9.42 | yes |
| CLEF representative | 6.96 / 6.82 | 6.96 | yes |
| NDJSON string-heavy | 2.33 / 2.48 | 3.66 | yes |
| OTLP protobuf logs representative | 12.54 / 12.46 | 12.65 | yes |
| OTLP JSON logs representative | 13.34 / 13.20 | 13.34 | yes |
| OTLP JSON logs many small records | 17.00 / 16.83 | 17.00 | yes |
| OTLP protobuf traces representative | 14.60 / 14.52 | 14.60 | yes |
| OTLP JSON traces representative | 14.36 / 14.22 | 14.36 | yes |
| OTLP protobuf metrics representative | 13.67 / 13.59 | 14.22 | yes |
| OTLP JSON metrics representative | 15.95 / 15.78 | 15.95 | yes |
| native JSON / NDJSON nested properties | 30.42 / 30.28 | 30.42 | no |
| native JSON / NDJSON many small events | 32.10 / 32.03 | 34.72 | no |
| OTLP protobuf logs many small records | 92.28 / 92.28 | 130.86 | no |
| OTLP JSON logs large shared resource | 124.76 / 124.29 | 222.75 | no |
| NDJSON empty objects | 213.25 / 213.05 | 269.24 | no |
| OTLP protobuf logs large shared resource | 1696.56 / 1695.99 | 3079.30 | no |

At 1 MB (`VYRTEL_AMPLIFICATION_TARGET=1048576`, the new default request
size) every figure is within 3% of the 2 MB one: the highest supported
fixture is 17.29× (OTLP/JSON small records) and the worst representative
16.20× (OTLP/JSON metrics); empty objects peak at 213.49×, i.e. 213 MB for
one 1 MB request. These shapes are accepted in production; see the
limitations in [configuration](configuration.md#ingest-memory).

Parse alone is consistently 1.25× body below admission (the body itself
plus read buffers). gzip adds nothing measurable at these sizes: decoding
is streamed in 64 KB reads. The highest supported figure is 17.0× (17.29×
at 1 MB); 20× keeps a 16–18% margin over it and 23–25% over the worst
representative batch (OTLP/JSON metrics, 15.95–16.20×). The shapes outside the envelope amplify by event count
(each event costs a few hundred bytes however small its JSON) or by copying
a shared OTLP resource into every event. Count-based admission cannot cover
them at useful request sizes; see the limitations in
[configuration](configuration.md#ingest-memory).

**Sustained overload** (`derived_admission_holds_the_ingest_budget_under_overload`).
Default and constrained derived configurations, three consecutive runs
each. `4 × limit + 16` clients cycle through every supported fixture at the
request-size limit, plain and gzip, retrying 429s after 5 ms. Each run has
three phases: 1 s of warm-up with every slot held by the test, so all
clients are rejected and their connections' read buffers reach working
size (reported, not counted: per-connection HTTP overhead); 2 s with every
writer parked, so admitted requests hold their slots with their batches
queued and unacknowledged while everyone else is rejected and drained; and
2 s with the writers running. In the parked phase storage allocates
nothing, so all heap growth is attributed to ingest; it includes the
server's connection and runtime buffers, which over-counts. It must stay
within the ingest budget. Process heap growth over both phases includes the
WAL and active segments and must stay within `max_memory`.

| Config | Run | Limit | Clients | Ingest budget | Conn. buffers | Ingest peak (parked) | 429s parked | 200s / 429s running | Process heap peak | RSS before / parked / after |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| default (512MB, 1MB) | 1 | 3 | 28 | 76.8 MB | 12.0 MB | 36.2 MB | 4,959 | 22 / 6,673 | 129.7 MB | 67.5 / 97.8 / 200.9 MB |
| default (512MB, 1MB) | 2 | 3 | 28 | 76.8 MB | 12.3 MB | 37.7 MB | 4,590 | 26 / 6,656 | 142.4 MB | 142.8 / 145.9 / 220.5 MB |
| default (512MB, 1MB) | 3 | 3 | 28 | 76.8 MB | 12.3 MB | 43.4 MB | 4,606 | 22 / 6,385 | 102.3 MB | 42.8 / 112.0 / 183.9 MB |
| constrained (128MB, 256KB) | 1 | 1 | 20 | 19.2 MB | 6.2 MB | 3.5 MB | 2,966 | 76 / 5,151 | 45.9 MB | 53.2 / 60.9 / 115.0 MB |
| constrained (128MB, 256KB) | 2 | 1 | 20 | 19.2 MB | 5.9 MB | 4.1 MB | 2,907 | 86 / 5,192 | 43.2 MB | 114.9 / 115.6 / 78.5 MB |
| constrained (128MB, 256KB) | 3 | 1 | 20 | 19.2 MB | 5.9 MB | 3.5 MB | 2,996 | 68 / 5,067 | 49.0 MB | 32.5 / 47.0 / 106.1 MB |

Every run saturated at exactly the derived limit, returned only 200s and
429s (no resets) and accepted a request afterwards. The default runs peak at
36–43 MB of ingest heap, under both the 72.75 MB estimate and the 76.8 MB
budget. RSS is the whole test process (clients, fixtures and earlier runs
included) and is reported for context only.

**Before and after.** `pre_rel14_defaults_under_the_same_overload` runs the
same harness with the old defaults (16MB requests, 4 slots) and
`queue_capacity = 200000`, so that 16MB batches are parsed and queued rather
than refused for exceeding the queue.

| | Before (512MB, 16MB, 4 slots) | After (512MB, 1MB, 3 slots) |
|---|---:|---:|
| Ingest heap peak, writers parked (76.8 MB budget) | 494.7–714.7 MB | 36.2–43.4 MB |
| Process heap peak (512 MB `max_memory`) | 540.0–825.5 MB | 102.3–142.4 MB |
| Requests / MB accepted in 2 s of writing | 4–8 / 64–128 MB | 22–26 / 22–26 MB |
| 429s in 2 s of writing | 692–709 | 6,385–6,673 |

The trade-off: with 1MB requests and 3 slots, the same overload accepts
fewer bytes per second of writing and rejects far more requests, and
clients that sent 16MB batches must send smaller ones. In return ingest
stays inside its budget instead of exceeding `max_memory`.

**Linux release run.** The same suite in a `rust:1-bookworm` container
(Docker Desktop, Linux x86_64, 22 cores, 15 GB, Rust 1.99.0, `--release
--locked`), with the repository mounted read-only:

```bash
docker run --rm -v "$PWD:/src:ro" -v vyrtel-target:/target -e CARGO_TARGET_DIR=/target -w /src \
  rust:1-bookworm cargo test -p server --release --locked --test overload -- --nocapture --test-threads 1
```

| | Linux x86_64 | Windows x86_64 |
|---|---:|---:|
| Admission ×body, every fixture | within 0.15 of Windows | table above |
| Highest supported fixture | 17.00× | 17.00× |
| Default ingest peak, writers parked (3 runs) | 21.2–28.5 MB | 36.2–43.4 MB |
| Constrained ingest peak (3 runs) | 1.6–4.4 MB | 3.5–4.1 MB |
| Drain peak (12.75 MB allowance) | 2.87 MB | 2.9–6.7 MB |
| Pre-REL-14 ingest peak (3 runs) | 458.7–773.8 MB | 494.7–714.7 MB |

Linux RSS climbed to about 507 MB during the run and stayed there: glibc
keeps freed memory, and the amplification test that runs first allocates
over 400 MB for its out-of-envelope fixtures. RSS from this harness is
therefore a process high-water mark, not a per-run figure. Measure RSS for
release sign-off on a dedicated server process, as PERF-GATE-1 (#170)
requires.

**Rejected-body drain** (`rejected_body_drain_overhead_is_within_the_fixed_allowance`).
One slot, held by a stalled upload; 64 clients send 1 MB bodies (half gzip)
as fast as they are rejected, for 3 s. After 1 s of warm-up the
connections' read buffers are at working size (about 470 KB each, about
29 MB in all, not counted). The remaining 2 s measure the drain itself
against the 12.75 MB fixed overhead (32 drains × hyper's 408 KB read
buffer). Measured drain peaks: 2.9–6.7 MB in release runs, 6.8 MB in a debug
run.

## Frontend tests

Vitest + React Testing Library in `web/tests/unit/`:

* live toggle states (paused red / live green) and toggling;
* query form submission and external updates;
* log row expansion and the vertical property tree (nested objects, arrays,
  null, include/exclude actions);
* filter panel selection;
* error rendering with a caret under the query position;
* trace navigation from a log;
* the Logs page with a mocked API and `EventSource`: results, diagnostics,
  filter clicks updating the query, live streaming, and paused mode keeping
  results stationary;
* helpers (query building, formatting, routing, span tree ordering,
  nanosecond math).

## Browser E2E (Playwright)

`web/tests/e2e/logs.spec.ts` runs against the real binary:

1. launch the server with an empty data directory;
2. ingest the NDJSON and OTLP trace fixtures;
3. open Logs, search `level = Error`, verify the results;
4. expand an event and verify structured properties and the stack trace;
5. confirm Live is paused, ingest an event, verify it does **not** appear;
6. enable Live, ingest events, verify the matching one appears (and the
   non-matching one does not);
7. pause again — results stay;
8. open the related trace and verify the span hierarchy and related logs.

`web/tests/e2e/storage.spec.ts` checks the storage-failure contract against
the same binary, through HTTP only:

1. `/ready` is `{"status": "ready", "storageWritable": true}` and every
   signal in `/api/v1/system/storage` is healthy with no `faults`;
2. on Linux and macOS (skipped on Windows and as root), `data/tmp/` is made
   read-only for real: `/ready` turns 503 with `storageWritable: false` and
   all signals listed, the `writeProbe` fault says `permissionDenied` on
   `tmp/.write-probe` and the response never contains the data directory;
   once permissions are restored `/ready` is 200 again and an event ingested
   before the fault is still queryable.

The data directory is exported to the specs as `VYRTEL_E2E_DATA`.

```bash
cd web && npm ci && npm run build && cd ..
cargo build -p server
cd web && npx playwright install chromium && npm run e2e
```

Set `VYRTEL_BIN` to test a different binary (e.g. a release build).

## Release gate tests

`scripts/release/release.test.mjs` and `scripts/release/performance.test.mjs` (`node --test`,
CI job **Release gate tests**) cover the checks in
[`scripts/release/release.mjs`](../scripts/release/release.mjs) and
[`scripts/release/performance.mjs`](../scripts/release/performance.mjs) that
[`release.yml`](../.github/workflows/release.yml) runs before publishing (see
[releasing.md](releasing.md)):

| Acceptance criterion | Tests |
|---|---|
| A tag whose version differs from the workspace fails | stable and prerelease tags against temporary workspaces; stale `Cargo.toml`, `Cargo.lock` or `web/package.json`; malformed tags; the repository's own version is consistent |
| Failed checks publish nothing | the workflow's job graph: `image` and `release` transitively need `preflight`, `performance`, `ci` (the whole callable CI workflow), `binaries` and `verify`; only a tag push publishes (a manual run is a rehearsal); release builds set `VYRTEL_REQUIRE_WEB_UI` |
| Embedded UI, version output and checksums are verified | artifact sets that are complete, missing a binary, tampered, missing `SHA256SUMS` or an entry, holding unexpected or empty files; the artifact list matches the build matrix; a fake binary (`testdata/fake-vyrtel.mjs`) that is correct, prints or serves the wrong version, serves the placeholder UI, misses its UI assets or exits on startup |
| A prerelease does not replace the stable latest channel | stable plans are Latest and tag `X.Y.Z`, `X.Y`, `latest`; prereleases (`-rc.1`, `-beta`, `-alpha.3`) are not Latest and tag only `X.Y.Z-pre`; image names are lowercased |
| TC-PERF-01: missing or failed performance evidence blocks tagging and publication | in throwaway git repositories: no record → `pretag --create` (function and CLI) exits 1 and creates no tag; FAIL verdict, missing reviewer, failing or mis-marked metric rows, missing limit/baseline/category/reports/artifact hashes, N/A without reason, budgets declared after measuring; a version with no declared gates; a review after the tag or a lightweight tag is refused by the workflow check; uncommitted changes and existing tags |
| TC-PERF-02: stale evidence is rejected until revalidated | a commit after PASS makes the record stale (no tag) until a PASS for the new commit; same-version gates (0.2.0) on different commits, or one missing; a record for another gate or version; rebuilt artifacts whose hash differs without documented `equivalence` |
| TC-PERF-03: a complete passing matrix for the exact candidate passes | the documented template covers every metric category and passes; `pretag --create` makes an annotated tag on the candidate with the PASS lines, which `check-performance --tag-date` (the workflow's check) accepts; matching or documented-equivalent artifacts; prereleases need no sign-off; the manifest declares unique gates with issues; the workflow wiring |

```bash
node --test scripts/release/*.test.mjs
VYRTEL_BIN=target/debug/vyrtel node --test scripts/release/*.test.mjs   # also checks a real binary
```

The CI **End-to-end** job also builds the server with `VYRTEL_REQUIRE_WEB_UI=1` and runs
`verify-binary` against it, so the gate is exercised against a real binary on every pull request. The performance tests
run git with an empty global configuration (`GIT_CONFIG_GLOBAL`), so local signing or hook
settings do not affect them.

## Benchmarks

Criterion benchmarks (`cargo bench -p storage -p query`). Results from one
run on a Windows 11 laptop, release profile — treat them as relative, not
absolute:

| Benchmark                                                       | Result                    |
|-----------------------------------------------------------------|---------------------------|
| WAL append, 1-event batch                                       | 5.0 µs (no fsync)         |
| WAL append, 100-event batch                                     | 34 µs (~2.9 M events/s)   |
| Encode 1,000 events                                             | 213 µs                    |
| Decode 1,000 events                                             | 2.8 ms (allocation-bound) |
| Create 20k-event segment (sort, encode, compress, index, fsync) | 183 ms (~110k events/s)   |
| Decode a whole 20k-event segment                                | 71 ms                     |
| Decompress one block, all columns                               | 556 µs (~470 MiB/s)       |
| Decompress one block, meta column only                          | 34 µs                     |
| Parse a 5-clause query                                          | 3.2 µs                    |
| 200k events / 4 segments: `message contains` (scan)             | 39 ms                     |
| … `traceId = …` (Bloom filter)                                  | 1.6 ms                    |
| … `level = Error` (bitmap)                                      | 4.9 ms                    |
| … `durationMs > 1990` (zone map)                                | 14 ms                     |
| … latest 200 events                                             | 0.66 ms                   |
| … count all (block counts, no decode)                           | 2.9 µs                    |

The benchmark dataset is synthetic and very regular (13.6× compression;
its unique-per-event `requestId` makes Bloom filters a large share of a tiny
segment). The load generator's data is more realistic: see below.

### End-to-end benchmarks

The Criterion benchmarks above measure library code in-process. For HTTP-level
numbers (ingest throughput and query latency against a real release server, plus
a mixed-load p50/p90/p95/p99 profile and index hotspots), use the .NET harness
under `dotnet/`. Results are published at [benchmarks](benchmarks.md):

```bash
cargo build --release -p server
dotnet run --project dotnet/Vyrtel.Benchmark -c Release -- --job short
dotnet run --project dotnet/Vyrtel.Profiling -c Release -- --duration 30 --concurrency 4
```

## Load generator

`tools/loadgen` sends realistic structured logs (services, error rate,
high-cardinality customer and request ids, nested HTTP properties,
exceptions with stack traces) and optionally OTLP traces.

```bash
cargo run --release -p loadgen -- \
  --target http://localhost:8080 --rate 10000 --duration 60s \
  --services 8 --error-rate 0.02 --cardinality 10000 --traces-every 200
```

Options: `--rate`, `--duration`, `--services`, `--error-rate`,
`--cardinality`, `--batch`, `--concurrency`, `--traces-every`, `--api-key`,
`--seed`. It reports throughput, latency and backpressure (429/503).

Observed on the same laptop with a release build (`--rate 10000 --duration
30s --traces-every 200`, default config):

| Metric                              | Value                                       |
|-------------------------------------|---------------------------------------------|
| Accepted                            | 302,000 events, 10,063 events/s, 0 rejected |
| Request latency (500-event batches) | 5.3 ms mean, 20.9 ms max                    |
| Process memory during the run       | 112 MB working set (126 MB peak)            |
| Idle memory before the run          | 20 MB working set                           |
| Compression (sealed segments)       | 4.9× raw → stored                           |
| Index overhead                      | 9.2% of compressed data                     |
| Query latency on that data          | 2–105 ms (see diagnostics in the UI)        |
