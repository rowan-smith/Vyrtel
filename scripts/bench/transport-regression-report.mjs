#!/usr/bin/env node
// Check scripts/bench/transport-regression.sh results against the REL-18
// transport regression budgets (transport-budgets.md, "Regression budgets"),
// which were declared before any candidate was measured:
//
//   t = max(10%, 3 × CV), CV = sample standard deviation ÷ mean of the
//   baseline's runs for that metric
//
//   accepted events/s (mean)  ≥ baseline mean × (1 − t)
//   ingest ACK p95 (mean)     ≤ baseline mean × (1 + t) + 2 ms
//   ingest ACK p99 (mean)     ≤ baseline mean × (1 + t) + 5 ms
//   query p95 (mean)          ≤ baseline mean × (1 + t) + 2 ms
//   peak RSS (mean)           ≤ baseline mean × (1 + t)
//   CPU seconds (mean)        ≤ baseline mean × (1 + t)
//   unexpected failures       0 in every run (non-200 other than 429,
//                             transport errors; ingest and queries)
//
// The evidence must be complete first; anything missing fails the report:
// at least --min-runs (default 3) runs of each build, the same number for
// both, each with summary.json and resources.json, `workloadValid`, steady
// load only (`noWaves`), the same connload arguments (apart from
// `--no-transport-counters`, which only baseline runs may use: the declared
// baseline predates those counters, and no gated metric comes from them),
// exit status 0 after
// `docker stop`, and finite values for every gated metric.
//
//   node scripts/bench/transport-regression-report.mjs OUT [--min-runs N] [--json FILE]
//
// Prints a Markdown table (means, every run's value, t, the limit and the
// verdict) and exits 1 if the evidence is incomplete or a budget is missed.

import { readdirSync, readFileSync, existsSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { pathToFileURL } from 'node:url';

const BUILDS = ['baseline', 'candidate'];

/** name, unit, read(summary, resources), higherIsBetter, absolute slack */
export const METRICS = [
  { key: 'eventsPerSecond', name: 'Acknowledged events/s', unit: 'events/s', higher: true, slack: 0,
    read: (s) => s?.ingest?.eventsPerSecond },
  { key: 'ackP95Ms', name: 'Ingest ACK latency p95', unit: 'ms', slack: 2, read: (s) => s?.ingest?.ackLatency?.p95Ms },
  { key: 'ackP99Ms', name: 'Ingest ACK latency p99', unit: 'ms', slack: 5, read: (s) => s?.ingest?.ackLatency?.p99Ms },
  { key: 'queryP95Ms', name: 'Query latency p95', unit: 'ms', slack: 2, read: (s) => s?.query?.latency?.p95Ms },
  { key: 'peakRssMiB', name: 'Peak RSS', unit: 'MiB', slack: 0, read: (_, r) => (r?.rssKb?.peak ?? NaN) / 1024 },
  { key: 'cpuSeconds', name: 'CPU seconds', unit: 's', slack: 0, read: (_, r) => r?.cpuSeconds },
];

const finite = (x) => typeof x === 'number' && Number.isFinite(x);
const mean = (xs) => xs.reduce((a, b) => a + b, 0) / xs.length;
const cv = (xs) => {
  const m = mean(xs);
  const sd = Math.sqrt(xs.reduce((a, x) => a + (x - m) ** 2, 0) / (xs.length - 1));
  return sd / m;
};
const readJson = (p) => JSON.parse(readFileSync(p, 'utf8'));

/** Unexpected failures of one run: non-200 other than 429, and transport errors. */
export const unexpected = (s) =>
  ['ingest', 'query'].reduce(
    (n, k) => n + (s?.[k]?.unavailable503 ?? 0) + (s?.[k]?.otherStatus ?? 0) + (s?.[k]?.transportErrors ?? 0),
    0,
  );

function loadRun(dir) {
  let s;
  let r;
  try {
    s = readJson(join(dir, 'summary.json'));
    r = readJson(join(dir, 'resources.json'));
  } catch (e) {
    return { error: `unreadable: ${e.message}` };
  }
  const problems = [];
  if (s.workloadValid !== true) problems.push(`not workloadValid (${(s.failures ?? []).join('; ') || 'no reason given'})`);
  if (s?.args?.noWaves !== true) problems.push('not a steady run (waves were opened)');
  if (r.serverExit !== 0) problems.push(`server exited ${r.serverExit} after docker stop`);
  const values = Object.fromEntries(METRICS.map((m) => [m.key, m.read(s, r)]));
  for (const m of METRICS) if (!finite(values[m.key])) problems.push(`${m.key} missing`);
  const failures = unexpected(s);
  if (!finite(failures)) problems.push('failure counts missing');
  const { transportCounters = true, ...args } = s.args ?? {};
  return problems.length ? { error: problems.join('; ') } : { values, failures, transportCounters, args: JSON.stringify(args) };
}

/** Evaluate a results directory; returns { ok, problems, rows, runs }. */
export function evaluate(dir, { minRuns = 3 } = {}) {
  const problems = [];
  const runs = { baseline: [], candidate: [] };
  const names = existsSync(dir) ? readdirSync(dir, { withFileTypes: true }).filter((e) => e.isDirectory()).map((e) => e.name) : [];
  for (const name of names.sort()) {
    const m = /^(baseline|candidate)-(\d+)$/.exec(name);
    if (!m) {
      problems.push(`unexpected directory ${name}`);
      continue;
    }
    const run = loadRun(join(dir, name));
    if (run.error) problems.push(`${name}: ${run.error}`);
    else runs[m[1]].push({ name, ...run });
  }
  for (const r of runs.candidate) {
    if (r.transportCounters === false) problems.push(`${r.name}: candidate run without transport counters`);
  }
  for (const b of BUILDS) {
    if (runs[b].length < minRuns) problems.push(`${b}: ${runs[b].length} usable run(s), need ${minRuns}`);
  }
  if (runs.baseline.length !== runs.candidate.length) {
    problems.push(`unequal runs: baseline ${runs.baseline.length}, candidate ${runs.candidate.length}`);
  }
  if (new Set([...runs.baseline, ...runs.candidate].map((r) => r.args)).size > 1) {
    problems.push('runs used different connload arguments');
  }

  const rows = [];
  const complete = problems.length === 0;
  for (const m of METRICS) {
    const b = runs.baseline.map((r) => r.values[m.key]);
    const c = runs.candidate.map((r) => r.values[m.key]);
    const row = { metric: m.name, unit: m.unit, baseline: b, candidate: c };
    if (complete) {
      const t = Math.max(0.1, 3 * cv(b));
      const limit = m.higher ? mean(b) * (1 - t) : mean(b) * (1 + t) + m.slack;
      const pass = m.higher ? mean(c) >= limit : mean(c) <= limit;
      Object.assign(row, { baselineMean: mean(b), candidateMean: mean(c), t, limit, op: m.higher ? '≥' : '≤', pass });
    }
    rows.push(row);
  }
  const failures = [...runs.baseline, ...runs.candidate].map((r) => [r.name, r.failures]);
  const failuresPass = failures.every(([, n]) => n === 0);
  rows.push({ metric: 'Unexpected failures', unit: 'count', failures, pass: complete ? failuresPass : undefined });
  const ok = complete && rows.every((r) => r.pass);
  return { ok, problems, rows, runs };
}

const fmt = (x) => (Math.abs(x) >= 100 ? x.toFixed(0) : x.toFixed(2));

export function markdown({ ok, problems, rows }) {
  const out = ['| Metric | Baseline runs | Candidate runs | Baseline mean | Candidate mean | t | Limit | Verdict |', '|---|---|---|---:|---:|---:|---|---|'];
  for (const r of rows) {
    if (r.failures) {
      const v = r.pass === undefined ? 'INCOMPLETE' : r.pass ? 'PASS' : '**FAIL**';
      out.push(`| ${r.metric} | ${r.failures.map(([n, f]) => `${n}: ${f}`).join(', ')} | | | | | 0 | ${v} |`);
      continue;
    }
    const runs = (xs) => xs.map(fmt).join(', ');
    if (r.pass === undefined) {
      out.push(`| ${r.metric} (${r.unit}) | ${runs(r.baseline)} | ${runs(r.candidate)} | | | | | INCOMPLETE |`);
    } else {
      out.push(
        `| ${r.metric} (${r.unit}) | ${runs(r.baseline)} | ${runs(r.candidate)} | ${fmt(r.baselineMean)} | ${fmt(r.candidateMean)} | ` +
          `${(r.t * 100).toFixed(1)}% | ${r.op} ${fmt(r.limit)} | ${r.pass ? 'PASS' : '**FAIL**'} |`,
      );
    }
  }
  out.push('', ok ? '**PASS**' : problems.length ? `**INCOMPLETE**: ${problems.join('; ')}` : '**FAIL**');
  return out.join('\n');
}

if (import.meta.url === pathToFileURL(process.argv[1]).href) {
  const args = process.argv.slice(2);
  const dir = args[0];
  if (!dir) {
    console.error('usage: transport-regression-report.mjs OUT [--min-runs N] [--json FILE]');
    process.exit(2);
  }
  const opt = (name) => {
    const i = args.indexOf(name);
    return i > 0 ? args[i + 1] : undefined;
  };
  const result = evaluate(dir, { minRuns: Number(opt('--min-runs') ?? 3) });
  console.log(markdown(result));
  if (opt('--json')) writeFileSync(opt('--json'), JSON.stringify(result, null, 2));
  process.exit(result.ok ? 0 : 1);
}
