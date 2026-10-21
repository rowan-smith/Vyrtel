# REL-18 transport evidence: developer host, 2026-10-17

Linux measurements for [REL-18](https://github.com/rowan-smith/Vyrtel/issues/184) (bounded HTTP
connections and transport buffers), against the budgets declared beforehand in
[`../budgets.md`](../budgets.md). Every run directory here holds its results: `summary.json`
(load generator) and `resources.json` (peaks, idle levels, CPU, shutdown). The raw captures
(`resources.csv`, the server process every ~2 s; `samples.csv`, `/api/v1/system/info` every
second; `connload.log`, `server.log`; `matrix.log` and `followup.log` with the order and times of
the runs; `listen-overflows.csv`, the kernel's listen-queue counters sampled during them) were
moved out of the repository and stay available at
[this commit](https://github.com/rowan-smith/Vyrtel/tree/8250c5be7af16dfbeff5ce6b17fb66174ed29a7c/docs/benchmarks/transport/2026-10-17-wsl2). These runs used
`scripts/bench/transport-soak.sh` before it recorded `listenOverflows` itself (added afterwards,
so the columns of `resources.csv` here stop at `socketBuffersKb`); the counters came from a
separate 15 s sampler instead.

**This is not release sign-off evidence.** It ran on a developer laptop's WSL2 VM with other work
on the host, not on the sign-off machine. [#170](https://github.com/rowan-smith/Vyrtel/issues/170)
reruns the procedure ([transport soak on Linux](../../../testing.md#transport-soak-on-linux)) on
the final candidate.

## Environment

| | |
|---|---|
| Host | Windows 11 Pro x86_64 laptop, Docker Desktop 29.5.2, WSL2 kernel 6.18.33.2-microsoft-standard-WSL2, 22 vCPUs, 15.4 GiB |
| Containers | `rust:1.96-bookworm` (Debian 12) as the runtime image; server and load generator in separate containers on one bridge network |
| Server limits | 2 CPUs; 1 GiB and 4096 FDs (normal), 384 MiB and 1024 FDs (constrained) |
| Load generator | 2 CPUs, its own container, 65536 FDs |
| Kernel | `net.core.somaxconn = 4096`, `tcp_max_syn_backlog = 1024`, `tcp_syncookies = 1`, `tcp_abort_on_overflow = 0` |
| Build | `cargo build --release --locked -p server -p loadgen`, Rust 1.96.1, `[profile.release]` (thin LTO, 1 codegen unit) |
| Baseline | `main` at `6c8a4a230eb76a8c6a35c24e992bebeced3a4287`; `vyrtel` sha256 `4e3225c68e2e2af6ea1b7d54422c45b85c9db1d3c16cf939b418b079d83db2ad` |
| Candidate | the REL-18 change on that commit (working tree before commit); `vyrtel` sha256 `4e651698103ba4e5bc7fdfda07390afc172232f389a7262ed6be45d241b847db`, `connload` sha256 `abef186d3a94cc3c9c12a7a4e1c9fb468dd33a694a0ecc4e26e0bc8a277a3a1e`. Review later made `body_timeout` absolute (checked before each body read, not only while waiting), so the merged code is not this binary; #170 measures the final one |
| Configurations | **normal**: defaults (`max_memory = 512MB`, `max_connections = 256`, `connection_buffer = 64KB`). **constrained**: `max_memory = 256MB`, `max_connections = 64`, `connection_buffer = 16KB` |
| Steady load (every run) | 4 ingest senders, 200-event NDJSON batches (~40 KB) every 50 ms on keep-alive connections; 2 count-query senders every second; the sampler (1 connection) |
| Waves (flood, soak) | 1,000 simultaneous attempts per wave, a quarter each idle, head trickled 1 byte / 500 ms, body stalled after 10 bytes, normal `GET /health`; every 30 s (flood, 3 min) or 60 s (soak, 30 min); each wave waits up to 90 s for its connections to end |
| Uninstrumented | No profiler attached in these runs. Heap figures come from the separate counting-allocator test ([transport heap](../../../testing.md#transport-heap)) |

## Results against the budgets

Exact values per run are in the tables below and in each run directory.

| Budget | Limit | Result | Verdict |
|---|---|---|---|
| Peak admitted connections | ≤ `max_connections` | 256 of 256 (normal), 64 of 64 (constrained) in every flood and soak | PASS |
| Peak waiting connections | ≤ 64 | 64 in every flood and soak | PASS |
| Transport heap at the limits | ≤ estimate | 23.99 MB of 40.75 MB estimated (200 slots, Linux release) | PASS |
| Heap retained after close | ≤ 512 KiB | 63.1 KiB; 63.0 KiB after write timeouts | PASS |
| Runtime tasks in a flood | ≤ C + 64 + 16 | 264 for C = 200 | PASS |
| Peak server FDs | ≤ C + 192 | normal: 345–346 (floods), 446 (soak) of 448. Constrained: 159–160 (floods) of 256; **325 (soak)** | **FAIL** (constrained soak) |
| FDs / sockets after load | FDs ≤ idle + 4, sockets ≤ 8 | sockets 4 in every run. FDs: floods +1 to +7; **soaks 141 and 209** (idle ~24) | **FAIL** (FDs, soaks and some floods) |
| OOM kills / exit after `docker stop` | none / 0 | none / 0 in every run | PASS |
| Peak RSS | ≤ container limit | normal ≤ 174 MiB of 1024; constrained ≤ 92 MiB of 384 | PASS |
| Post-load RSS | ≤ `max_memory` | normal ≤ 170 MiB of 512; constrained ≤ 85 MiB of 256 | PASS |
| RSS slope, last 20 min of soak | ≤ 1.0 MiB/min | constrained 0.63; **normal 1.90** | **FAIL** (normal) |
| Steady clients' transport errors, `server_busy` | 0 | 0 in flood-normal-2, -3, -4, every constrained flood and both soaks; **flood-normal-1: 38 errors, 1 503** | **FAIL** (flood-normal-1) |
| Acknowledged = persisted | equal | equal in all 27 runs (up to 12,996,400 events) | PASS |
| Shutdown after load | ≤ 8 s, exit 0 | 0.5–1.6 s, exit 0 | PASS |
| Wave connections end | ≤ 82 s | **server side: ≤ 61 s** (sampled `active` back to its pre-wave level). **Client side: every first wave reached the 90 s patience** | **FAIL** as declared (client side) |
| Steady regression vs baseline | see below | 5 of 6 metrics within tolerance; **peak RSS outside** | **FAIL** (peak RSS) |

### What the failures are

* **Peak FDs (constrained soak) and FDs after load.** Not connections: server sockets were back to 4
  after every run. Storage keeps every sealed segment open for reads
  (`SegmentReader` holds its `File`), so descriptors grow with the data kept: 13.0 M events in the
  normal soak, and more, smaller segments with `max_memory = 256MB`. The budget's 128-descriptor
  allowance for files ignored this, so it was wrong; the same growth happens on 0.1.1. The
  configuration docs now size descriptors as
  `max_connections + 64 + 25 + segments` ([file descriptors](../../../configuration.md#memory-accounting)).
* **RSS slope (normal soak).** 1.90 MiB/min with waves; the [control soak](#control-soak) without
  waves, at twice the ingest rate, stayed at 0.61. Not attributed: see there. Post-load RSS stayed
  within `max_memory` in every run.
* **Steady-client errors (flood-normal-1).** That run overlapped a `cargo test --workspace` on the
  same host (07:22–07:27; see `matrix.log`), which slowed the 2-CPU server (37 s of CPU against
  15–16 s in runs 2 and 3, query p95 197 ms against 39–52 ms). The errors come from ingest
  admission, not the transport: bodies stalled on purpose hold all 3 ingest slots for
  `body_timeout`, so steady senders mostly get `429`, and their rejected bodies wait behind those
  stalled ones for the 32 discard slots (REL-02); past the 10 s discard timeout the server drops
  the body and resets the connection, and the sender's reconnect then meets a full server. A
  clean repeat, flood-normal-4, had no errors or 503s. The run is kept and counted as a FAIL.
* **Wave connections end (client side).** Each wave's 1,000 handshakes overflow the kernel's
  accept queue (tokio's backlog of 1024; up to 1,595 `ListenOverflows` per run in
  `listen-overflows.csv`). With SYN cookies and `tcp_abort_on_overflow = 0`, the client's
  handshake completes while the kernel has dropped the server's side, so an idle client sees no
  close until its own timeout (here the 90 s patience); clients that send data get a reset. The
  server never had those connections: peak established sockets on the port were 320 (normal) and
  128 (constrained), and `active` was back to its pre-wave level within 61 s, the slow-body
  connections holding ingest slots until `body_timeout` (60 s) being last. The budget measured the
  client side, so it fails as declared. Documented in
  [what happens to a connection](../../../configuration.md#what-happens-to-a-connection).
* **Steady peak RSS.** Candidate mean 121.9 MiB against a limit of 119.5 (baseline 108.6 MiB × 1.10).
  The three baseline runs happened to land on the low of two levels. Nine interleaved diagnostic
  runs afterwards (`diag-steady-*`) show both builds bimodal, about 111 or 124–136 MiB:
  baseline 111.6 / 111.6 / 129.6 (mean 117.6), candidate 136.0 / 111.9 / 111.2 (119.7), and the
  candidate with the old 408 KB buffer 109.9 / 123.4 / 117.1 (116.8). No systematic difference,
  so not a regression of this change, but the declared budget is missed and is not re-scored. #170
  should gate peak RSS on more runs, as [reproducible baselines](../../benchmarks.md#regression-budgets)
  does with its variance factor.

## Steady load: baseline vs candidate (normal, no waves)

Three 60 s runs each, back to back (`steady-baseline-*`, `steady-candidate-*`). The load is
paced, so events/s compares the same offered rate; latency, CPU and memory are what can regress.
Tolerance `t = max(10%, 3 × CV of the baseline)`.

| Metric | Baseline (mean of 3) | CV | t | Candidate (mean of 3) | Delta | Limit | Verdict |
|---|---:|---:|---:|---:|---:|---:|---|
| Acknowledged events/s | 14,566 | 0.2% | 10% | 14,534 | −32 (−0.2%) | ≥ 13,109 | PASS |
| Ingest ACK p95 (ms) | 2.158 | 4.7% | 14.0% | 2.138 | −0.020 | ≤ 4.460 | PASS |
| Ingest ACK p99 (ms) | 6.624 | 13.9% | 41.6% | 6.585 | −0.039 | ≤ 14.380 | PASS |
| Query p95 (ms) | 32.13 | 4.1% | 12.3% | 32.13 | 0.00 | ≤ 38.08 | PASS |
| Peak RSS (MiB) | 108.6 | 0.1% | 10% | 121.9 | +13.3 | ≤ 119.5 | **FAIL** (see above) |
| CPU (s) | 8.08 | 1.8% | 10% | 7.99 | −0.09 | ≤ 8.89 | PASS |
| Unexpected failures | 0 | | | 0 | | 0 | PASS |

Per run (`ok / 429 / 503 / transport errors`; 429s are ingest admission with 4 senders and 3 slots):

| Run | Events/s | ACK p50 / p95 / p99 ms | Query p95 ms | Peak RSS MiB | CPU s | Ingest requests | Acked = stored | Shutdown s |
|---|---:|---|---:|---:|---:|---|---|---:|
| steady-baseline-1 | 14,541 | 1.467 / 2.240 / 6.700 | 33.5 | 108.6 | 8.25 | 4519 / 18 / 0 / 0 | 903,800 ✓ | 0.55 |
| steady-baseline-2 | 14,602 | 1.381 / 2.045 / 5.670 | 32.0 | 108.6 | 7.98 | 4535 / 16 / 0 / 0 | 907,000 ✓ | 0.56 |
| steady-baseline-3 | 14,554 | 1.429 / 2.188 / 7.503 | 30.9 | 108.7 | 8.01 | 4520 / 24 / 0 / 0 | 904,000 ✓ | 0.50 |
| steady-candidate-1 | 14,561 | 1.437 / 2.156 / 6.221 | 33.1 | 134.1 | 8.02 | 4524 / 20 / 0 / 0 | 904,800 ✓ | 0.54 |
| steady-candidate-2 | 14,500 | 1.413 / 2.142 / 6.918 | 31.0 | 108.2 | 7.99 | 4506 / 39 / 0 / 0 | 901,200 ✓ | 0.60 |
| steady-candidate-3 | 14,542 | 1.411 / 2.115 / 6.616 | 32.2 | 123.4 | 7.95 | 4517 / 28 / 0 / 0 | 903,400 ✓ | 0.51 |

The diagnostic runs ran later, when the host was slower for all three builds alike (CPU ~15 s,
ACK p95 ~4 ms); compare them only with each other.

## Floods (3 min, a wave of 1,000 attempts every 30 s)

| Run | Peak active / waiting | Admitted | Refused | Header / body timeouts | Peak FDs / sockets | After: FDs / sockets | Peak RSS MiB | Socket buffers MiB | CPU s | Steady ingest ok / 429 / 503 / errors | ACK p95 / p99 ms | Query p95 ms | Acked = stored | Shutdown s |
|---|---|---:|---:|---|---|---|---:|---:|---:|---|---|---:|---|---:|
| flood-baseline-1 (0.1.1) | unbounded | — | — | — | 766 / 748 | 24 / 6 | 147.4 | 0.1 | 10.2 | 2622 / 11201 / 0 / 0 | 2.19 / 11.29 | 21.3 | 524,400 ✓ | 0.59 |
| flood-normal-1 (host busy) | 256 / 64 | 781 | 1,295 | 356 / 6 | 345 / 324 | 25 / 4 | 140.0 | 0.4 | 37.0 | 4615 / 8162 / 1 / 38 | 6.27 / 57.77 | 197.1 | 923,000 ✓ | 0.69 |
| flood-normal-2 | 256 / 64 | 801 | 1,275 | 315 / 6 | 346 / 324 | 26 / 4 | 118.6 | 0.3 | 15.9 | 4857 / 8546 / 0 / 0 | 2.22 / 8.33 | 51.8 | 971,400 ✓ | 0.61 |
| flood-normal-3 | 256 / 64 | 733 | 1,301 | 333 / 6 | 346 / 324 | 26 / 4 | 121.3 | 0.2 | 14.8 | 4870 / 8546 / 0 / 0 | 2.22 / 8.39 | 39.1 | 974,000 ✓ | 0.61 |
| flood-normal-4 | 256 / 64 | 780 | 1,271 | 328 / 6 | 346 / 324 | 26 / 4 | 123.5 | 0.2 | 29.6 | 4725 / 8623 / 0 / 0 | 4.14 / 10.93 | 75.8 | 945,000 ✓ | 0.92 |
| flood-constrained-1 | 64 / 64 | 158 | 1,851 | 76 / 2 | 160 / 132 | 31 / 4 | 64.1 | 1.0 | 12.3 | 3854 / 9959 / 0 / 0 | 2.14 / 4.48 | 31.6 | 770,800 ✓ | 0.58 |
| flood-constrained-2 | 64 / 64 | 215 | 1,795 | 79 / 2 | 159 / 132 | 31 / 4 | 62.9 | 0.2 | 11.8 | 3784 / 10027 / 0 / 0 | 2.09 / 4.55 | 30.1 | 756,800 ✓ | 0.55 |
| flood-constrained-3 | 64 / 64 | 312 | 1,696 | 80 / 2 | 159 / 132 | 31 / 4 | 84.9 | 0.2 | 11.7 | 3794 / 10026 / 0 / 0 | 2.05 / 2.85 | 29.4 | 758,800 ✓ | 0.51 |

What the 0.1.1 baseline shows: with no transport limits, every idle and trickled connection
stayed open for the full 90 s (500 + 500 per run, still open when the generator gave up), the
server held 748 sockets, and stalled bodies kept the ingest slots indefinitely, so steady ingest
got more 429s (11,201) than with the candidate's `body_timeout`.

Wave outcomes, all waves of a run (`kind.outcome`: `closed` = EOF with no bytes, the idle and
trickled connections closed on `header_timeout`; `refused` = the fixed 503; `rejected` = ingest
`429`; `timeout` = `408`; `reset` = a reset, mostly refusals of clients that had already sent data,
and handshakes the kernel dropped; `other` = still open at the 90 s patience):

| Run | idle closed / refused / other | slow-head closed / reset | slow-body closed / rejected / timeout / reset | normal served / closed / reset |
|---|---|---|---|---|
| flood-baseline-1 (0.1.1) | 0 / 0 / 500 | 0 / 0 (500 other) | 0 / 494 / 0 / 0 (6 other) | 500 / 0 / 0 |
| flood-normal-1 | 362 / 55 / 83 | 289 / 211 | 129 / 160 / 6 / 204 | 161 / 131 / 206 |
| flood-normal-2 | 398 / 26 / 76 | 319 / 181 | 146 / 177 / 6 / 171 | 166 / 146 / 188 |
| flood-normal-3 | 409 / 55 / 36 | 324 / 176 | 158 / 159 / 6 / 177 | 176 / 156 / 168 |
| flood-normal-4 | 406 / 30 / 64 | 318 / 181 | 143 / 164 / 6 / 186 | 167 / 148 / 183 |
| flood-constrained-1 | 437 / 40 / 23 | 367 / 133 | 339 / 36 / 2 / 123 | 37 / 349 / 114 |
| flood-constrained-2 | 412 / 32 / 56 | 250 / 250 | 215 / 33 / 2 / 250 | 38 / 214 / 248 |
| flood-constrained-3 | 292 / 54 / 154 | 251 / 249 | 214 / 32 / 2 / 252 | 37 / 213 / 250 |

## Soaks (30 min, a wave every 60 s, mixed load)

The two soaks ran at the same time (07:48–08:20), each with its own containers and CPU limits.

| | soak-normal | soak-constrained |
|---|---:|---:|
| Waves / attempts | 15 / 15,000 | 16 / 16,000 |
| Peak active / waiting | 256 / 64 | 64 / 64 |
| Admitted / queued / refused | 5,184 / 1,651 / 10,020 | 1,282 / 1,335 / 14,725 |
| Header / body / write timeouts | 2,487 / 45 / 0 | 612 / 15 / 0 |
| Peak FDs / sockets | 446 / 324 | 325 / 132 |
| After: FDs / sockets | 141 / 4 | 209 / 4 |
| Peak established sockets on the port | 320 | 128 |
| RSS idle before / peak / after (MiB) | 16.3 / 174.0 / 170.4 | 15.3 / 92.2 / 77.2 |
| RSS slope, last 20 min | 1.90 MiB/min | 0.63 MiB/min |
| Peak cgroup memory / socket buffers (MiB) | 229.1 / 0.2 | 129.8 / 0.2 |
| CPU (s, of 2 CPUs × 30 min) | 1,351.8 | 1,162.9 |
| Steady ingest ok / 429 / 503 / errors | 64,982 / 67,573 / 0 / 0 | 49,883 / 85,254 / 0 / 0 |
| Ingest ACK p50 / p95 / p99 (ms) | 2.87 / 5.69 / 20.81 | 2.82 / 5.19 / 14.58 |
| Count query p95 (ms) | 1,337.5 | 1,046.4 |
| Acknowledged = stored | 12,996,400 ✓ | 9,976,600 ✓ |
| Server-side drain per wave (s) | 61 (every wave) | 61 (one wave 11) |
| Shutdown (s) / exit | 0.92 / 0 | 1.59 / 0 |

Count queries slow down over the soak because each counts every event of the run so far (up to
13 M), on a server busy with waves; they never failed or timed out.

### Control soak

`soak-normal-control`: the normal configuration with the same steady load for 30 minutes and no
waves, run afterwards to attribute the normal soak's RSS slope. The host slept 6 minutes in (the
VM was suspended for 3.8 h; the load generator's clock stopped with it and its 1,801 one-second
samples have no gap, but `resources.csv` shows the host-side jump at 367 s). The slope window is
after the resume.

| | soak-normal (waves) | soak-normal-control (no waves) |
|---|---:|---:|
| Acknowledged events | 12,996,400 (7,177/s) | 26,453,200 (14,652/s) |
| RSS peak / after (MiB) | 174.0 / 170.4 | 163.8 / 152.0 |
| RSS slope, last 20 min | 1.90 MiB/min | 0.61 MiB/min |
| FDs after / sockets after | 141 / 4 | 274 / 4 |
| Steady ingest 429 / transport errors | 67,573 / 0 | 1,180 / 0 |
| Acknowledged = stored | ✓ | ✓ |

Without waves, at twice the ingest rate, the slope is within the budget and descriptors grow
faster (more segments), which confirms that the descriptor growth is storage's. The higher slope
with waves is **not attributed**: the counting-allocator test retains 63 KB after every connection
has closed, which argues against retention in the transport, and the steady runs show RSS moving
between two levels in both builds, which would fit allocator behaviour; but one 30-minute run
cannot separate these. The RSS-slope FAIL stands until #170 repeats the soak with heap profiling
(and, for a control, the 0.1.1 baseline under the same waves).

## Heap (counting allocator, release, Linux)

From [`tests/overload/transport.rs`](../../../../tests/overload/transport.rs), 200 slots, 64 KB
buffer, in a container (`--memory 6g --cpus 4`):

| | |
|---|---:|
| Heap per idle connection | 18.0 KB |
| Heap per full read buffer (head filling it, never finished) | 122.3 KB |
| Heap per stalled reader (pipelining, never reading) | 47.7 KB |
| Flood of 1,000 on a full server: waiting / refused | 64 / 936 |
| New runtime tasks during the flood | 264 |
| Peak heap growth / estimate | 23.99 MB / 40.75 MB |
| Retained after close / after write timeouts | 63.1 KB / 63.0 KB |
| Kernel TCP memory at the stalled-reader peak (both sides) | ~1,015 MiB (`/proc/net/sockstat`) |

With `--memory 1g` the same test was killed by the OOM killer during the stalled-reader phase: the
kernel's socket buffers, not the heap. Hence the separate kernel-memory section in
[memory accounting](../../../configuration.md#memory-accounting).

## Reproducing

```bash
docker volume create rel18-target
docker run --rm -v "$PWD:/src:ro" -v rel18-target:/target -e CARGO_TARGET_DIR=/target -w /src \
  rust:1.96-bookworm cargo build --release --locked -p server -p loadgen
IMAGE=rust:1.96-bookworm scripts/bench/transport-soak.sh out/flood-normal-1 --duration 180s --attempts 1000
```

The full matrix (steady ×3 each for baseline and candidate, floods ×3 for normal and constrained,
one baseline flood, both soaks) is in [testing](../../../testing.md#transport-soak-on-linux). For
the baseline, build the prior commit onto another volume and pass `SERVER_VOLUME`. On Windows
(Git Bash), set `MSYS_NO_PATHCONV=1`.
