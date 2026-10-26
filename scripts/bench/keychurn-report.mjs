#!/usr/bin/env node
// Summarise scripts/bench/keychurn.sh results and check them against the
// AUTH-PERF-01 budgets, which were fixed before any run (docs/testing.md,
// "API key churn benchmark").
//
// The evidence must be complete before any budget is checked; anything
// missing fails the report:
//
// * every expected cell (the 8 of the matrix, or those matching --cells)
//   has results for both binaries and no other cell is present;
// * each cell has at least --min-runs (default 3) usable runs per binary,
//   the same number for both, run with the same workload settings;
// * a run is usable when its summary.json and server.json parse, its gated
//   values are finite, it acknowledged events, it measured the auth probe,
//   and it did (churn cells) or did not (steady cells) change keys.
//
// Budgets:
//
// * every run, both binaries: every acknowledged event persisted; no ingest
//   401, 5xx, timeout or transport error; every key create/revoke
//   succeeded; every auth probe answered 401;
// * per cell, candidate median against baseline median:
//   - accepted events/s at least 90%;
//   - server CPU seconds per accepted event at most 110%;
//   - auth probe p99 at most max(110%, +1 ms);
//   - peak memory at most 110%.
//
//   node scripts/bench/keychurn-report.mjs RESULTS_DIR [--cells REGEX] [--min-runs N] [--json OUT]
//
// Prints a Markdown table of medians (and every usable run's raw value) and
// exits 1 if the evidence is incomplete or any budget is missed.

import { readdirSync, readFileSync, existsSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { pathToFileURL } from 'node:url';

export const CELLS = ['low', 'high'].flatMap((p) =>
  ['small', 'batched'].flatMap((s) => ['steady', 'churn'].map((c) => `${p}-${s}-${c}`)),
);
const BINARIES = ['baseline', 'candidate'];

const median = (xs) => {
  const v = [...xs].sort((a, b) => a - b);
  if (!v.length) return null;
  const m = Math.floor(v.length / 2);
  return v.length % 2 ? v[m] : (v[m - 1] + v[m]) / 2;
};

const finite = (x) => typeof x === 'number' && Number.isFinite(x);

const readJson = (path) => JSON.parse(readFileSync(path, 'utf8'));

/** One run's values, or the reason it is unusable. */
function loadRun(path, cell) {
  let s;
  let p;
  try {
    s = readJson(join(path, 'summary.json'));
    p = readJson(join(path, 'server.json'));
  } catch (e) {
    return { error: `unreadable: ${e.message}` };
  }
  const acked = s?.ingest?.acknowledgedEvents;
  const r = {
    eventsPerSecond: s?.ingest?.eventsPerSecond,
    requestsPerSecond: s?.ingest?.requestsPerSecond,
    ack: s?.ingest?.ackLatency ?? {},
    auth: s?.auth?.latency ?? {},
    heartbeat: s?.heartbeat?.latency ?? {},
    ingest: s?.ingest?.statuses,
    authStatuses: s?.auth?.statuses,
    churn: s?.churn,
    args: s?.args,
    reconciled: s?.reconciled,
    cpuSeconds: p?.cpuSeconds,
    cpuMicrosPerEvent: finite(acked) && acked > 0 ? (p?.cpuSeconds * 1e6) / acked : null,
    peakMemoryBytes: p?.peakMemoryBytes,
  };
  if (!r.ingest || !r.authStatuses || !r.churn?.create?.statuses || !r.churn?.revoke?.statuses || !r.args) {
    return { error: 'summary.json is missing sections' };
  }
  if (!finite(acked) || acked <= 0) return { error: 'no events acknowledged' };
  if (!finite(r.auth.samples) || r.auth.samples <= 0) return { error: 'no auth probe measured' };
  for (const [name, v] of [
    ['events/s', r.eventsPerSecond],
    ['auth p99', r.auth.p99Ms],
    ['CPU seconds', r.cpuSeconds],
    ['CPU per event', r.cpuMicrosPerEvent],
    ['peak memory', r.peakMemoryBytes],
  ]) {
    if (!finite(v) || v < 0) return { error: `${name} is ${JSON.stringify(v)}` };
  }
  if (r.peakMemoryBytes <= 0) return { error: 'peak memory not measured' };
  const churned = r.args.churnIntervalMs > 0;
  if (churned !== cell.endsWith('-churn')) return { error: `churn ${churned ? 'on' : 'off'} in a ${cell} cell` };
  if (churned && !(r.churn.pairs > 0)) return { error: 'no key changes in a churn cell' };
  return r;
}

const workload = (r) => JSON.stringify([r.args.concurrency, r.args.batch, r.args.churnIntervalMs, r.args.timeoutMs]);

const metrics = [
  ['events/s', (r) => r.eventsPerSecond, 'higher'],
  ['requests/s', (r) => r.requestsPerSecond, 'info'],
  ['CPU µs/event', (r) => r.cpuMicrosPerEvent, 'lower'],
  ['ACK p50 ms', (r) => r.ack.p50Ms, 'info'],
  ['ACK p95 ms', (r) => r.ack.p95Ms, 'info'],
  ['ACK p99 ms', (r) => r.ack.p99Ms, 'info'],
  ['ACK max ms', (r) => r.ack.maxMs, 'info'],
  ['auth p50 ms', (r) => r.auth.p50Ms, 'info'],
  ['auth p99 ms', (r) => r.auth.p99Ms, 'auth'],
  ['auth max ms', (r) => r.auth.maxMs, 'info'],
  ['heartbeat p99 ms', (r) => r.heartbeat.p99Ms, 'info'],
  ['429s', (r) => r.ingest.rejected429, 'info'],
  ['key pairs', (r) => r.churn.pairs, 'info'],
  ['create p99 ms', (r) => r.churn.create.latency?.p99Ms, 'info'],
  ['revoke p99 ms', (r) => r.churn.revoke.latency?.p99Ms, 'info'],
  ['peak MiB', (r) => r.peakMemoryBytes / 2 ** 20, 'memory'],
];

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

  const runs = {};
  for (const binary of BINARIES) {
    runs[binary] = {};
    const root = join(dir, binary);
    const present = existsSync(root) ? readdirSync(root) : [];
    for (const cell of present.filter((c) => !expected.includes(c))) {
      failures.push(`${binary}: unexpected cell ${cell}`);
    }
    for (const cell of expected) {
      const path = join(root, cell);
      if (!existsSync(path)) {
        failures.push(`${binary} ${cell}: no results`);
        runs[binary][cell] = [];
        continue;
      }
      runs[binary][cell] = readdirSync(path)
        .sort()
        .flatMap((run) => {
          const r = loadRun(join(path, run), cell);
          if (r.error) {
            failures.push(`${binary} ${cell} ${run}: unusable, ${r.error}`);
            return [];
          }
          return [{ run, ...r }];
        });
    }
  }

  for (const binary of BINARIES) {
    for (const [cell, list] of Object.entries(runs[binary])) {
      for (const r of list) {
        const where = `${binary} ${cell} ${r.run}`;
        if (r.reconciled !== true) failures.push(`${where}: acknowledged events not all persisted`);
        const i = r.ingest;
        if (i.unauthorized401 || i.serverError5xx || i.timeouts || i.transportErrors || i.otherStatus) {
          failures.push(`${where}: ingest ${JSON.stringify(i)}`);
        }
        const a = r.authStatuses;
        if (a.serverError5xx || a.timeouts || a.transportErrors || a.otherStatus || a.rejected429) {
          failures.push(`${where}: auth probe ${JSON.stringify(a)}`);
        }
        for (const op of ['create', 'revoke']) {
          const s = r.churn[op].statuses;
          if (s.unauthorized401 || s.rejected429 || s.serverError5xx || s.otherStatus || s.timeouts || s.transportErrors) {
            failures.push(`${where}: key ${op} ${JSON.stringify(s)}`);
          }
        }
      }
    }
  }

  const fmt = (x) => (!finite(x) ? '–' : Math.abs(x) >= 100 ? x.toFixed(0) : x.toFixed(2));
  const lines = [];
  const report = {};
  for (const cell of expected) {
    const b = runs.baseline[cell];
    const c = runs.candidate[cell];
    // Budgets are checked only on complete, matched evidence.
    let comparable = true;
    for (const [binary, list] of [
      ['baseline', b],
      ['candidate', c],
    ]) {
      if (list.length < minRuns) {
        failures.push(`${binary} ${cell}: ${list.length} usable runs, at least ${minRuns} required`);
        comparable = false;
      }
    }
    if (b.length !== c.length) {
      failures.push(`${cell}: ${b.length} baseline runs but ${c.length} candidate runs`);
      comparable = false;
    }
    const workloads = new Set([...b, ...c].map(workload));
    if (workloads.size > 1) {
      failures.push(`${cell}: runs used different workloads ${[...workloads].join(' / ')}`);
      comparable = false;
    }

    report[cell] = {};
    lines.push(`\n### ${cell}\n`);
    lines.push('| Metric | Baseline median | Candidate median | Change | Baseline runs | Candidate runs |');
    lines.push('|---|---:|---:|---:|---|---|');
    for (const [label, get, kind] of metrics) {
      const bv = b.map(get);
      const cv = c.map(get);
      const gated = kind !== 'info';
      const usable = (xs) => xs.length > 0 && xs.every(finite);
      const bm = usable(bv) ? median(bv) : null;
      const cm = usable(cv) ? median(cv) : null;
      const change = finite(bm) && finite(cm) && bm !== 0 ? ((cm - bm) / bm) * 100 : null;
      report[cell][label] = { baseline: bv, candidate: cv, baselineMedian: bm, candidateMedian: cm };
      lines.push(
        `| ${label} | ${fmt(bm)} | ${fmt(cm)} | ${change == null ? '–' : `${change >= 0 ? '+' : ''}${change.toFixed(1)}%`} | ${bv.map(fmt).join(', ')} | ${cv.map(fmt).join(', ')} |`,
      );
      if (!gated || !comparable) continue;
      if (!finite(bm) || !finite(cm)) {
        failures.push(`${cell}: ${label} has missing or non-finite values`);
        continue;
      }
      if (kind === 'higher' && cm < 0.9 * bm) failures.push(`${cell}: ${label} ${fmt(cm)} < 90% of ${fmt(bm)}`);
      if (kind === 'lower' && cm > 1.1 * bm) failures.push(`${cell}: ${label} ${fmt(cm)} > 110% of ${fmt(bm)}`);
      if (kind === 'memory' && cm > 1.1 * bm) failures.push(`${cell}: ${label} ${fmt(cm)} > 110% of ${fmt(bm)}`);
      if (kind === 'auth' && cm > Math.max(1.1 * bm, bm + 1)) {
        failures.push(`${cell}: ${label} ${fmt(cm)} > max(110%, +1 ms) of ${fmt(bm)}`);
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
    console.error('usage: keychurn-report.mjs RESULTS_DIR [--cells REGEX] [--min-runs N] [--json OUT]');
    return 2;
  }
  const { lines, report, failures } = evaluate(dir, opts);
  console.log(lines.join('\n'));
  console.log(failures.length ? `\nNot met:\n- ${failures.join('\n- ')}` : '\nAll budgets met.');
  if (jsonOut) writeFileSync(jsonOut, JSON.stringify({ options: opts, report, failures }, null, 2));
  return failures.length ? 1 : 0;
}

if (import.meta.url === pathToFileURL(process.argv[1]).href) process.exit(main(process.argv.slice(2)));
