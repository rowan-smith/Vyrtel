#!/usr/bin/env node
// Summarise scripts/bench/keychurn.sh results and check them against the
// AUTH-PERF-01 budgets, which were fixed before any run (docs/testing.md,
// "API key churn"):
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
//   node scripts/bench/keychurn-report.mjs target/keychurn [--json out.json]
//
// Prints a Markdown table of medians (and every run's raw value) and exits 1
// if any budget is missed.

import { readdirSync, readFileSync, existsSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';

const [dir, ...rest] = process.argv.slice(2);
if (!dir) {
  console.error('usage: keychurn-report.mjs RESULTS_DIR [--json OUT]');
  process.exit(2);
}
const jsonOut = rest[0] === '--json' ? rest[1] : null;

const median = (xs) => {
  const v = xs.filter((x) => x != null).sort((a, b) => a - b);
  if (!v.length) return null;
  const m = Math.floor(v.length / 2);
  return v.length % 2 ? v[m] : (v[m - 1] + v[m]) / 2;
};

function load(binary) {
  const cells = {};
  const root = join(dir, binary);
  if (!existsSync(root)) return cells;
  for (const cell of readdirSync(root).sort()) {
    cells[cell] = readdirSync(join(root, cell))
      .sort()
      .map((run) => {
        const s = JSON.parse(readFileSync(join(root, cell, run, 'summary.json'), 'utf8'));
        const p = JSON.parse(readFileSync(join(root, cell, run, 'server.json'), 'utf8'));
        const acked = s.ingest.acknowledgedEvents;
        return {
          run,
          eventsPerSecond: s.ingest.eventsPerSecond,
          requestsPerSecond: s.ingest.requestsPerSecond,
          ack: s.ingest.ackLatency,
          auth: s.auth.latency,
          heartbeat: s.heartbeat.latency,
          ingest: s.ingest.statuses,
          authStatuses: s.auth.statuses,
          churn: s.churn,
          reconciled: s.reconciled,
          cpuSeconds: p.cpuSeconds,
          cpuMicrosPerEvent: acked ? (p.cpuSeconds * 1e6) / acked : null,
          peakMemoryBytes: p.peakMemoryBytes,
        };
      });
  }
  return cells;
}

const base = load('baseline');
const cand = load('candidate');
const failures = [];

for (const [name, cells] of [
  ['baseline', base],
  ['candidate', cand],
]) {
  for (const [cell, runs] of Object.entries(cells)) {
    for (const r of runs) {
      const where = `${name} ${cell} ${r.run}`;
      if (!r.reconciled) failures.push(`${where}: acknowledged events not all persisted`);
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
  ['create p99 ms', (r) => r.churn.create.latency.p99Ms, 'info'],
  ['revoke p99 ms', (r) => r.churn.revoke.latency.p99Ms, 'info'],
  ['peak MiB', (r) => r.peakMemoryBytes / 2 ** 20, 'memory'],
];

const fmt = (x) => (x == null ? '–' : Math.abs(x) >= 100 ? x.toFixed(0) : x.toFixed(2));
const lines = [];
const report = {};
for (const cell of Object.keys(cand)) {
  const b = base[cell] ?? [];
  const c = cand[cell];
  report[cell] = {};
  lines.push(`\n### ${cell}\n`);
  lines.push('| Metric | Baseline median | Candidate median | Change | Baseline runs | Candidate runs |');
  lines.push('|---|---:|---:|---:|---|---|');
  for (const [label, get, kind] of metrics) {
    const bv = b.map(get);
    const cv = c.map(get);
    const bm = median(bv);
    const cm = median(cv);
    const change = bm ? ((cm - bm) / bm) * 100 : null;
    report[cell][label] = { baseline: bv, candidate: cv, baselineMedian: bm, candidateMedian: cm };
    lines.push(
      `| ${label} | ${fmt(bm)} | ${fmt(cm)} | ${change == null ? '–' : `${change >= 0 ? '+' : ''}${change.toFixed(1)}%`} | ${bv.map(fmt).join(', ')} | ${cv.map(fmt).join(', ')} |`,
    );
    if (bm == null || cm == null) continue;
    if (kind === 'higher' && cm < 0.9 * bm) failures.push(`${cell}: ${label} ${fmt(cm)} < 90% of ${fmt(bm)}`);
    if (kind === 'lower' && cm > 1.1 * bm) failures.push(`${cell}: ${label} ${fmt(cm)} > 110% of ${fmt(bm)}`);
    if (kind === 'memory' && cm > 1.1 * bm) failures.push(`${cell}: ${label} ${fmt(cm)} > 110% of ${fmt(bm)}`);
    if (kind === 'auth' && cm > Math.max(1.1 * bm, bm + 1)) {
      failures.push(`${cell}: ${label} ${fmt(cm)} > max(110%, +1 ms) of ${fmt(bm)}`);
    }
  }
}

console.log(lines.join('\n'));
console.log(failures.length ? `\nBudgets missed:\n- ${failures.join('\n- ')}` : '\nAll budgets met.');
if (jsonOut) writeFileSync(jsonOut, JSON.stringify({ report, failures }, null, 2));
process.exit(failures.length ? 1 : 0);
