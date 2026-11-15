# Sign-in memory budgets (AUTH-PERF-04)

Declared 2026-11-15T11:25:00Z, before any measurement of a candidate with
the change they follow (Argon2id's work area mapped per check, and at most
`auth::MAX_SIGN_INS_ADMITTED` (16) sign-ins admitted at once). The limits are
not changed after a candidate is measured; a miss is a FAIL and is fixed,
not re-budgeted. They add to the
[PERF-GATE-1](https://github.com/rowan-smith/Vyrtel/issues/170) budgets and
change none of them.

**Why.** 0.1.2-rc.4 checked one password at a time, but each check's 19 MiB
work area came from glibc's malloc, which kept a freed work area resident
per arena: on an empty server, with nothing stored and no other load,
bursts of concurrent sign-ins, right or wrong (so needing no account), left
111 MiB resident, and it stayed after the load ended (exploratory run on
the reference host, 2026-11-15; with glibc's mmap threshold fixed, the same
run peaked at 35 MiB and kept 16 MiB). The semaphore bounded live hashing
memory, not retained allocator memory. The number of arenas is an amplifier,
not a known ceiling, so no allowance on top of the peak-memory budget would
have been a bound.

`scripts/bench/signin-memory.sh BINARY` measures the published binary with
the default allocator and no `MALLOC_*` settings, on a fresh data directory
with the default configuration. Idle is the server's RSS 2 s after it
reports ready (it has verified the configured admin password once by then).

| Metric | Unit | Limit |
|---|---|---|
| Peak RSS during the sign-in phases above idle | MiB | ≤ 30 (one live work area, 19 MiB, plus bounded overhead) |
| RSS 10 s after the last attempt, above idle | MiB | ≤ 8 |
| Sign-in responses other than 200 (right password), 401 (wrong) or 429 (over the admission limit) | count | 0 |
| Over-limit attempts refused before hashing: 429s in the 64- and 128-at-once phases | count | > 0 |
| 429 response time p99 | ms | ≤ 100 (refused before hashing, not after waiting) |
| Steady ingest and query clients during a wrong-password flood: transport errors and 5xx | count | 0 |
| Argon2id parameters and stored-hash compatibility | — | unchanged (`password::tests`) |

**Measurement fix 2026-11-15** (no limit changed), before any measurement
of a published candidate with the change. A run's statuses alone could pass
without the work they stand for: steady queries that all got `401` (a
failed setup sign-in) or ingestion that got only `429` counted as zero
errors. A run is now evidence only when it is **valid**: the setup sign-in,
an authenticated query and an ingest succeed before anything is measured;
every right-password phase got at least one `200` and every wrong-password
phase at least one `401`; during the flood the steady clients had
successful ingest and query requests and no status other than `200` (and
`429` for ingest, counted apart); a genuine sign-in got in within
`GENUINE_DEADLINE_S` (30 s); and the server was still running at the end.
Every request has a 5 s connect and 30 s total timeout, so a candidate that
hangs fails the run instead of hanging it, and results go to a new
directory that is never overwritten. The admission bound now covers each
sign-in request until its response (including the delay after a failure),
not only its lookup and check.

Clarified the same day, before any published candidate was measured (no
limit changed): the genuine sign-in counts only when it succeeds before
its deadline and while the flood is still running. The flood runs for at
least `FLOOD_S` and until the genuine sign-in has finished, and each of
its attempts may take only what is left of the deadline. Readiness
(`READY_S`, 20 s), `--version` (5 s) and shutdown (`SHUTDOWN_S`, 15 s after
SIGTERM, then SIGKILL) are bounded too, and a server that does not stop in
time makes the run invalid. Every invalid run writes `summary.json`.

Recorded, not budgeted: idle RSS, every phase's response counts and times,
the steady clients' latency, and how long a right-password sign-in takes to
succeed while the flood runs. Admission is global, not per client, so a
flood can make a genuine sign-in wait for a free place (it gets 429 and
retries); per-client throttling is not part of this change.
