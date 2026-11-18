# REL-18 transport budgets

Declared 2026-10-17T07:13:00Z, before any candidate measurement of the
transport runs (`scripts/bench/transport-soak.sh`). Baseline:
`main` at `6c8a4a230eb76a8c6a35c24e992bebeced3a4287` (0.1.1 + REL-17). These
limits are not changed after the candidate is measured; a miss is a FAIL and
is fixed, not re-budgeted.

**Amended 2026-11-15**, before any measurement of a candidate with the change
it follows. The 0.1.2-rc.1 and rc.2 runs missed "FDs ≤ idle-before + 4"
because every segment kept its file open, so descriptors grew with stored
data until the open-file limit stopped ingestion: a real defect, fixed rather
than re-budgeted. Segment files are now opened on demand and at most
`storage::segment::MAX_OPEN_SEGMENT_FILES` (64) stay open, by design, so
`transport-soak.sh` counts them separately (`peakSegmentFds`,
`segmentFdsAfter`) and the idle rule applies to the other descriptors
(`otherFdsBefore`, `otherFdsAfter`). The peak-FD limit is unchanged.

**Amended again 2026-11-15**, before any measurement of a candidate with
`server.listen_backlog`. 0.1.2-rc.3 missed "time for a flood wave's
connections to end ≤ 82 s" with 91.1 s: the 60 s body timeout plus 31 s of
Linux handshake retries (1 + 2 + 4 + 8 + 16 s), because the listen queue
overflowed (tokio binds with a backlog of 128). That single figure,
measured from the client's first attempt, mixes the kernel's handshake,
which Vyrtel only sizes, with the server's own deadlines, which start at
accept. It is replaced by the rows marked *(amended)* and *(added)*, all from
`connload`'s per-attempt timing in `summary.json` (`flood`). The 82 s limit
from the client's first attempt is kept, not loosened: establishment delay
is what a client sees. An attempt the server never ended (`abandoned`,
still open when `connload` gives up) has no accept-to-end figure but counts
end to end, so it fails that row. Clarified the same day, after two
exploratory runs that were discarded (`connload` then resolved the target
name per attempt, and glibc's 5 s DNS retry showed up as establishment
time) and before the runs that are reported. Dropped handshakes (`listenOverflows`) and the
container's `net.core.somaxconn` are recorded, not budgeted. The
`listen_backlog` change must not move any other row; in particular peak
FDs, kernel socket memory and the steady clients' rows are compared
against the same run with the old backlog of 128.

**Measurement fix 2026-11-15** (no limit changed). Sign-in is always on
since 0.1.2, and `connload` did not sign in, so its count queries and
`system/info` samples were refused: "Acknowledged vs persisted events" and
"Query latency p95" were INCOMPLETE in the 0.1.2-rc.1 to rc.3 runs.
`transport-soak.sh` now starts the server with an admin password, and
`connload` signs in with it (session cookie) for queries and samples and
sends an ingest-scoped key with every batch. `summary.json` `reconciled` is
true only when the stored count of connload's service equals the
acknowledged events; `auth` records the kind of credential each client
used. Runs without a credential are not evidence for either row. Because
ingestion is then protected, the flood's `slow-body` attempts send the
ingest key as well, so they still reach the body read and its timeout
instead of being refused by authentication; a flood run whose
`flood.bodyTimeouts` is 0 is invalid. A run is evidence for any row only
when `summary.json` `workloadValid` is true (events acknowledged, queries
answered, every sample read with all its counters, no unexpected status or
transport error on the steady clients, reconciled, no abandoned flood
attempt whatever `--wave-patience` was, as the wave rows above require).
`samples.csv` now covers each wave's cleanup, not only the steady load.
Throughput is over `loadWindowS`, the
steady load's own window, not the time spent counting afterwards.

**Measurement fix 2026-11-18** (no limit changed). 0.1.2-rc.5's
constrained flood run recorded a shutdown of -447 ms: `transport-soak.sh`
timed `docker stop` with two wall-clock readings, and the clock moved back
between them. Every elapsed time the script records (`elapsedMs`,
`loadEndedAtMs`, `shutdownMs`) now comes from `CLOCK_MONOTONIC`. The
regression comparison below is run by `transport-regression.sh` and
checked by `transport-regression-report.mjs`, which applies the rules
below as written and reports INCOMPLETE, not a verdict, when a build has
fewer than three usable runs.

Configurations:

* **normal**: defaults (`max_memory = 512MB`, `max_connections = 256`,
  `connection_buffer = 64KB`), server container 2 CPUs, 1 GiB, 4096 FDs.
* **constrained**: `max_memory = 256MB`, `max_connections = 64`,
  `connection_buffer = 16KB`, server container 2 CPUs, 384 MiB, 1024 FDs.

## Absolute budgets (new behaviour, no baseline)

| Metric                                                | Unit    | Limit                                                                                       |
|-------------------------------------------------------|---------|---------------------------------------------------------------------------------------------|
| Peak admitted connections (sampled)                   | count   | ≤ `max_connections`                                                                         |
| Peak waiting connections (sampled)                    | count   | ≤ 64                                                                                        |
| Transport heap at the limits (`tests/overload`)       | bytes   | ≤ `TransportLimits::estimated_peak`                                                        |
| Heap retained after every connection closed           | KiB     | ≤ 512                                                                                       |
| Runtime tasks during a flood (`tests/overload`)       | count   | ≤ `max_connections` + 64 + 16                                                               |
| Peak server FDs                                       | count   | ≤ `max_connections` + 64 + 128                                                              |
| Server FDs and sockets after load (idle)              | count   | non-segment FDs ≤ idle-before + 4; sockets ≤ 8 (amended, see below)                          |
| Segment-file FDs, at any time                         | count   | ≤ `MAX_OPEN_SEGMENT_FILES` (64) (added, see below)                                          |
| Server OOM kills / exit status after `docker stop`    | —       | none / 0                                                                                    |
| Peak RSS                                              | MiB     | normal ≤ 1024; constrained ≤ 384 (container limits)                                         |
| Post-load RSS                                         | MiB     | ≤ `max_memory`                                                                              |
| RSS growth slope, last 20 min of each 30 min soak     | MiB/min | ≤ 1.0                                                                                       |
| Steady clients' transport errors and `server_busy` during floods | count | 0 (ingest, query and sampler clients hold admitted keep-alive connections)        |
| Acknowledged vs persisted events                      | events  | equal                                                                                       |
| Shutdown after load                                   | s       | ≤ 8 (`shutdown_timeout`)                                                                    |
| Server-side cleanup: a flood-wave connection ends after it is established (`flood.worstWave.acceptToEndMaxMs`) *(amended)* | s | ≤ `header_timeout` + `body_timeout` + `PENDING_TIMEOUT` + 10 (82) |
| End to end: a flood wave's connections end after the client's first attempt (`flood.longestWaveS`) *(amended)* | s | ≤ `header_timeout` + `body_timeout` + `PENDING_TIMEOUT` + 10 (82) |
| Connection establishment p99, worst wave (`flood.worstWave.establishmentP99Ms`) *(added)* | s | ≤ 5 |

## Regression budgets (steady mixed load, no waves, normal configuration)

Three 60 s runs each of baseline and candidate. Tolerance `t = max(10%, 3 ×
CV)`, where CV is the coefficient of variation of the baseline's three runs
for that metric (the 3 × CV rule of [reproducible baselines](../../dotnet/Vyrtel.Baseline/README.md#regression-budgets),
with a 10% floor because three runs on a shared WSL2 VM are noisy).

| Metric                         | Unit     | Limit                                    |
|--------------------------------|----------|------------------------------------------|
| Acknowledged events/s (mean)   | events/s | ≥ baseline mean × (1 − t)                |
| Ingest ACK latency p95 (mean)  | ms       | ≤ baseline mean × (1 + t) + 2 ms         |
| Ingest ACK latency p99 (mean)  | ms       | ≤ baseline mean × (1 + t) + 5 ms         |
| Query latency p95 (mean)       | ms       | ≤ baseline mean × (1 + t) + 2 ms         |
| Peak RSS (mean)                | MiB      | ≤ baseline mean × (1 + t)                |
| CPU seconds (mean)             | s        | ≤ baseline mean × (1 + t)                |
| Unexpected failures            | count    | 0 (non-200 other than 429, transport errors) |
