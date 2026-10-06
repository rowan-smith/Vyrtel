# Testing

Testing is part of the implementation. Everything below runs in CI
(`.github/workflows/ci.yml`).

```bash
cargo test --workspace --all-features    # all Rust tests (~190, many property-based)
cd web && npm test                       # frontend unit tests
cd web && npm run e2e                    # Playwright (needs a built server, see below)
cargo bench -p storage -p query          # Criterion benchmarks
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
| Metadata store (migrations, CRUD, alert transitions)                 | `crates/metadata/tests/store.rs`     |

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
  data-directory locking.

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
* ingest → rotate segment → query (diagnostics show segment pruning);
* OTLP logs (JSON and gzip protobuf) queried through the native API;
* OTLP spans → trace search → trace by id → correlated logs;
* OTLP metrics → names → sum/count/max/avg with group-by and filters;
* retention → restart → verify retained/deleted data;
* API errors: invalid JSON, invalid events, invalid queries (with position),
  oversized requests, unsupported media types, unknown endpoints;
* missing auth, bad auth, ingest-only keys, sessions, logout, revocation;
* pagination, time ranges, empty results, facets, histograms;
* live stream (SSE) delivery and filtering, and that open streams do not
  block shutdown;
* dashboards, saved queries, alerts firing/resolving with webhook delivery,
  the background evaluator.

Fixtures shared with E2E live in `tests/fixtures/`.

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

```bash
cd web && npm ci && npm run build && cd ..
cargo build -p server
cd web && npx playwright install chromium && npm run e2e
```

Set `OBSERVER_BIN` to test a different binary (e.g. a release build).

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
