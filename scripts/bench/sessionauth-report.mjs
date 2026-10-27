#!/usr/bin/env node
// Summarise scripts/bench/sessionauth.sh results and check them against the
// AUTH-PERF-02 budgets, which were fixed before any run (docs/testing.md,
// "Session authentication benchmark"). Session-cookie runs are compared with
// admin-key runs of the same cell on the same binary.
//
// The evidence must be complete before any budget is checked; anything
// missing fails the report:
//
// * every expected cell (the 6 of the matrix, or those matching --cells)
//   has results for both credentials and no other cell is present;
// * each cell has at least --min-runs (default 3) usable runs per
//   credential, the same number for both, run with the same workload;
// * a run is usable when its summary.json and server.json parse, it used
//   the credential and workload of its directory, its gated values are
//   finite, and it completed load requests and auth probes.
//
// Budgets:
//
// * every run, both credentials: no load, probe or metadata-write request
//   answered 401, 403, 5xx other than 503, another error status, a timeout
//   or a transport error; every probe succeeded; no session lookup failed;
//   in mixed cells every acknowledged event persisted and the writer made
//   changes; key runs used no session (no cookie was sent); session runs
//   authenticated every successful load request, probe and write by
//   session (a cache hit or a SQLite lookup);
// * session runs, absolute: mean SQLite session lookup (queue for a turn
//   and a blocking thread + metadata lock + query) at most 1 ms; warm
//   probe p99 at most 25 ms;
// * per cell, session median against key median:
//   - successful load requests/s at least 90%;
//   - warm load p99 and warm probe p99 at most max(110%, +1 ms);
//   - server CPU seconds per successful load request at most 110%;
//   - peak memory at most 110%.
//
//   node scripts/bench/sessionauth-report.mjs RESULTS_DIR [--cells REGEX] [--min-runs N] [--json OUT]
//
// Prints a Markdown table of medians (and every usable run's raw value) and
// exits 1 if the evidence is incomplete or any budget is missed.

import { readdirSync, readFileSync, existsSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { pathToFileURL } from 'node:url';

export const CELLS = ['auth', 'query', 'mixed'].flatMap((w) => ['low', 'high'].map((c) => `${w}-${c}`));
const CREDENTIALS = ['key', 'session'];
const MAX_MEAN_LOOKUP_MICROS = 1000;
const MAX_PROBE_P99_MS = 25;

const median = (xs) => {
  const v = [...xs].sort((a, b) => a - b);
  if (!v.length) return null;
  const m = Math.floor(v.length / 2);
  return v.length % 2 ? v[m] : (v[m - 1] + v[m]) / 2;
};

const finite = (x) => typeof x === 'number' && Number.isFinite(x);

const readJson = (path) => JSON.parse(readFileSync(path, 'utf8'));

/** One run's values, or the reason it is unusable. */
function loadRun(path, cell, credential) {
  let s;
  let p;
  try {
    s = readJson(join(path, 'summary.json'));
    p = readJson(join(path, 'server.json'));
  } catch (e) {
    return { error: `unreadable: ${e.message}` };
  }
  if (!s?.load?.all?.statuses || !s?.probe?.statuses || !s?.sessionLookups || !s?.args || !s?.writes || !s?.ingest) {
    return { error: 'summary.json is missing sections' };
  }
  if (s.auth !== credential) return { error: `credential ${s.auth} in a ${credential} directory` };
  if (s.workload !== cell.split('-')[0]) return { error: `workload ${s.workload} in a ${cell} cell` };
  const ok = s.load.all.statuses.ok;
  const lookups = s.sessionLookups;
  // Every part must be present when SQLite was read at all.
  const parts = [lookups.meanQueueMicros, lookups.meanLockMicros, lookups.meanQueryMicros];
  const meanLookup =
    credential === 'session' && lookups.lookups > 0 ? (parts.every(finite) ? parts.reduce((a, b) => a + b) : null) : 0;
  const r = {
    workload: s.workload,
    args: s.args,
    requestsPerSecond: s.load.requestsPerSecond,
    load: s.load,
    loadWarm: s.load.all.warm ?? {},
    loadCold: s.load.all.cold ?? {},
    probe: s.probe,
    probeWarm: s.probe.warm ?? {},
    heartbeatWarm: s.heartbeat?.warm ?? {},
    ingest: s.ingest,
    writes: s.writes,
    lookups,
    meanLookupMicros: meanLookup,
    reconciled: s.reconciled,
    cpuSeconds: p?.cpuSeconds,
    cpuMicrosPerRequest: finite(ok) && ok > 0 ? (p?.cpuSeconds * 1e6) / ok : null,
    peakMemoryBytes: p?.peakMemoryBytes,
  };
  if (!finite(ok) || ok <= 0) return { error: 'no load request succeeded' };
  if (!finite(r.probeWarm.samples) || r.probeWarm.samples <= 0) return { error: 'no warm auth probe measured' };
  for (const [name, v] of [
    ['requests/s', r.requestsPerSecond],
    ['load p99', r.loadWarm.p99Ms],
    ['probe p99', r.probeWarm.p99Ms],
    ['CPU seconds', r.cpuSeconds],
    ['CPU per request', r.cpuMicrosPerRequest],
    ['peak memory', r.peakMemoryBytes],
    ['session lookups', lookups.lookups],
    ['mean session lookup', r.meanLookupMicros],
  ]) {
    if (!finite(v) || v < 0) return { error: `${name} is ${JSON.stringify(v)}` };
  }
  if (r.peakMemoryBytes <= 0) return { error: 'peak memory not measured' };
  return r;
}

const workload = (r) =>
  JSON.stringify([
    r.workload,
    r.args.concurrency,
    r.args.seedEvents,
    r.args.ingestConcurrency,
    r.args.batch,
    r.args.writeIntervalMs,
    r.args.probeIntervalMs,
    r.args.coldMs,
    r.args.timeoutMs,
  ]);

const metrics = [
  ['requests/s', (r) => r.requestsPerSecond, 'higher'],
  ['load p50 ms', (r) => r.loadWarm.p50Ms, 'info'],
  ['load p95 ms', (r) => r.loadWarm.p95Ms, 'info'],
  ['load p99 ms', (r) => r.loadWarm.p99Ms, 'latency'],
  ['load max ms', (r) => r.loadWarm.maxMs, 'info'],
  ['cold load p99 ms', (r) => r.loadCold.p99Ms, 'info'],
  ['search p99 ms', (r) => r.load.search?.warm?.p99Ms, 'info'],
  ['facets p99 ms', (r) => r.load.facets?.warm?.p99Ms, 'info'],
  ['dashboard p99 ms', (r) => r.load.dashboard?.warm?.p99Ms, 'info'],
  ['probe p50 ms', (r) => r.probeWarm.p50Ms, 'info'],
  ['probe p99 ms', (r) => r.probeWarm.p99Ms, 'latency'],
  ['probe max ms', (r) => r.probeWarm.maxMs, 'info'],
  ['heartbeat p99 ms', (r) => r.heartbeatWarm.p99Ms, 'info'],
  ['503s', (r) => r.load.all.statuses.unavailable503, 'info'],
  ['ingest events/s', (r) => r.ingest.eventsPerSecond, 'info'],
  ['ingest 429s', (r) => r.ingest.requests?.statuses?.rejected429, 'info'],
  ['write pairs', (r) => r.writes.pairs, 'info'],
  ['cache hits', (r) => r.lookups.cacheHits ?? 0, 'info'],
  ['lookups/s', (r) => r.lookups.perSecond, 'info'],
  ['lookup queue µs', (r) => r.lookups.meanQueueMicros, 'info'],
  ['lookup lock µs', (r) => r.lookups.meanLockMicros, 'info'],
  ['lookup query µs', (r) => r.lookups.meanQueryMicros, 'info'],
  ['lookup max lock µs', (r) => r.lookups.maxLockMicros, 'info'],
  ['lookups in flight (peak)', (r) => r.lookups.peakInFlight, 'info'],
  ['CPU µs/request', (r) => r.cpuMicrosPerRequest, 'lower'],
  ['peak MiB', (r) => r.peakMemoryBytes / 2 ** 20, 'lower'],
];

/** Correctness of one run; pushes every failure. */
function checkRun(r, where, credential, cell, failures) {
  const bad = (s, allow503) =>
    s.unauthorized401 ||
    s.forbidden403 ||
    s.serverError5xx ||
    s.otherStatus ||
    s.timeouts ||
    s.transportErrors ||
    (!allow503 && s.unavailable503);
  const load = r.load.all.statuses;
  if (bad(load, true)) failures.push(`${where}: load ${JSON.stringify(load)}`);
  const probe = r.probe.statuses;
  if (bad(probe, false) || probe.rejected429) failures.push(`${where}: auth probe ${JSON.stringify(probe)}`);
  if (r.lookups.failed) failures.push(`${where}: ${r.lookups.failed} session lookups failed`);
  if (cell.startsWith('mixed-')) {
    if (r.reconciled !== true) failures.push(`${where}: acknowledged events not all persisted`);
    const w = r.writes.requests?.statuses ?? {};
    if (bad(w, false) || w.rejected429) failures.push(`${where}: metadata writes ${JSON.stringify(w)}`);
    if (!(r.writes.pairs > 0)) failures.push(`${where}: no metadata writes`);
    const i = r.ingest.requests?.statuses ?? {};
    if (bad(i, false)) failures.push(`${where}: ingest ${JSON.stringify(i)}`);
    if (!(r.ingest.acknowledgedEvents > 0)) failures.push(`${where}: nothing ingested`);
  }
  const authenticated = load.ok + probe.ok + (r.writes.requests?.statuses?.ok ?? 0);
  const used = r.lookups.lookups + (r.lookups.cacheHits ?? 0);
  if (credential === 'key' && used !== 0) {
    failures.push(`${where}: ${used} session checks in a key-only run (a cookie was sent)`);
  }
  if (credential === 'session' && used < authenticated) {
    failures.push(`${where}: ${used} session checks for ${authenticated} authenticated requests`);
  }
  if (credential === 'session') {
    if (r.meanLookupMicros > MAX_MEAN_LOOKUP_MICROS) {
      failures.push(`${where}: mean session lookup ${r.meanLookupMicros.toFixed(0)} µs > ${MAX_MEAN_LOOKUP_MICROS} µs`);
    }
    if (r.probeWarm.p99Ms > MAX_PROBE_P99_MS) {
      failures.push(`${where}: probe p99 ${r.probeWarm.p99Ms} ms > ${MAX_PROBE_P99_MS} ms`);
    }
  }
}

/** Checks the results in `dir`; returns the Markdown lines, per-cell values and every failure. */
export function evaluate(dir, { cells = '.', minRuns = 3 } = {}) {
  const failures = [];
  let pattern;
  try {
    pattern = new RegExp(cells);
  } catch (e) {
    return { lines: [], report: {}, failures: [`--cells: ${e.message}`] };
  }
  const expected = CELLS.filter((c) => pattern.test(c));
  if (!expected.length) failures.push(`--cells ${cells} matches none of ${CELLS.join(', ')}`);
  const present = existsSync(dir) ? readdirSync(dir).filter((c) => !c.endsWith('.json') && !c.endsWith('.md')) : [];
  for (const cell of present.filter((c) => !expected.includes(c))) failures.push(`unexpected cell ${cell}`);

  const runs = {};
  for (const cell of expected) {
    runs[cell] = {};
    for (const credential of CREDENTIALS) {
      const path = join(dir, cell, credential);
      if (!existsSync(path)) {
        failures.push(`${cell} ${credential}: no results`);
        runs[cell][credential] = [];
        continue;
      }
      runs[cell][credential] = readdirSync(path)
        .sort()
        .flatMap((run) => {
          const r = loadRun(join(path, run), cell, credential);
          if (r.error) {
            failures.push(`${cell} ${credential} ${run}: unusable, ${r.error}`);
            return [];
          }
          checkRun(r, `${cell} ${credential} ${run}`, credential, cell, failures);
          return [{ run, ...r }];
        });
    }
  }

  const fmt = (x) => (!finite(x) ? '–' : Math.abs(x) >= 100 ? x.toFixed(0) : x.toFixed(2));
  const lines = [];
  const report = {};
  for (const cell of expected) {
    const k = runs[cell].key;
    const s = runs[cell].session;
    let comparable = true;
    for (const [credential, list] of [
      ['key', k],
      ['session', s],
    ]) {
      if (list.length < minRuns) {
        failures.push(`${cell} ${credential}: ${list.length} usable runs, at least ${minRuns} required`);
        comparable = false;
      }
    }
    if (k.length !== s.length) {
      failures.push(`${cell}: ${k.length} key runs but ${s.length} session runs`);
      comparable = false;
    }
    const workloads = new Set([...k, ...s].map(workload));
    if (workloads.size > 1) {
      failures.push(`${cell}: runs used different workloads ${[...workloads].join(' / ')}`);
      comparable = false;
    }

    report[cell] = {};
    lines.push(`\n### ${cell}\n`);
    lines.push('| Metric | Key median | Session median | Change | Key runs | Session runs |');
    lines.push('|---|---:|---:|---:|---|---|');
    for (const [label, get, kind] of metrics) {
      const kv = k.map(get);
      const sv = s.map(get);
      const usable = (xs) => xs.length > 0 && xs.every(finite);
      const km = usable(kv) ? median(kv) : null;
      const sm = usable(sv) ? median(sv) : null;
      const change = finite(km) && finite(sm) && km !== 0 ? ((sm - km) / km) * 100 : null;
      report[cell][label] = { key: kv, session: sv, keyMedian: km, sessionMedian: sm };
      lines.push(
        `| ${label} | ${fmt(km)} | ${fmt(sm)} | ${change == null ? '–' : `${change >= 0 ? '+' : ''}${change.toFixed(1)}%`} | ${kv.map(fmt).join(', ')} | ${sv.map(fmt).join(', ')} |`,
      );
      if (kind === 'info' || !comparable) continue;
      if (!finite(km) || !finite(sm)) {
        failures.push(`${cell}: ${label} has missing or non-finite values`);
        continue;
      }
      if (kind === 'higher' && sm < 0.9 * km) failures.push(`${cell}: ${label} ${fmt(sm)} < 90% of ${fmt(km)}`);
      if (kind === 'lower' && sm > 1.1 * km) failures.push(`${cell}: ${label} ${fmt(sm)} > 110% of ${fmt(km)}`);
      if (kind === 'latency' && sm > Math.max(1.1 * km, km + 1)) {
        failures.push(`${cell}: ${label} ${fmt(sm)} > max(110%, +1 ms) of ${fmt(km)}`);
      }
    }
  }
  return { lines, report, failures };
}

function main(argv) {
  const opts = { cells: '.', minRuns: 3 };
  let dir;
  let jsonOut = null;
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i];
    if (a === '--json') jsonOut = argv[++i];
    else if (a === '--cells') opts.cells = argv[++i];
    else if (a === '--min-runs') opts.minRuns = Number(argv[++i]);
    else if (!dir && !a.startsWith('--')) dir = a;
    else {
      console.error(`unknown argument ${a}`);
      return 2;
    }
  }
  if (!dir || !Number.isInteger(opts.minRuns) || opts.minRuns < 1 || jsonOut === undefined || opts.cells === undefined) {
    console.error('usage: sessionauth-report.mjs RESULTS_DIR [--cells REGEX] [--min-runs N] [--json OUT]');
    return 2;
  }
  const { lines, report, failures } = evaluate(dir, opts);
  console.log(lines.join('\n'));
  console.log(failures.length ? `\nNot met:\n- ${failures.join('\n- ')}` : '\nAll budgets met.');
  if (jsonOut) writeFileSync(jsonOut, JSON.stringify({ options: opts, report, failures }, null, 2));
  return failures.length ? 1 : 0;
}

if (import.meta.url === pathToFileURL(process.argv[1]).href) process.exit(main(process.argv.slice(2)));
