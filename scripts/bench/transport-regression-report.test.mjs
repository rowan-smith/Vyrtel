// Checks of transport-regression-report.mjs against synthetic results.
//
//   node --test scripts/bench/*.test.mjs

import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { afterEach, test } from 'node:test';

import { evaluate, markdown } from './transport-regression-report.mjs';

const dirs = [];
afterEach(() => {
  while (dirs.length) fs.rmSync(dirs.pop(), { recursive: true, force: true });
});

const run = ({ eps = 15000, p95 = 2.3, p99 = 2.8, q95 = 80, rssKb = 110 * 1024, cpu = 40, failures = 0, valid = true, noWaves = true, exit = 0, counters = true } = {}) => ({
  summary: {
    workloadValid: valid,
    failures: valid ? [] : ['something'],
    args: { noWaves, ingestClients: 4, transportCounters: counters },
    ingest: { eventsPerSecond: eps, ackLatency: { p95Ms: p95, p99Ms: p99 }, unavailable503: 0, otherStatus: failures, transportErrors: 0 },
    query: { latency: { p95Ms: q95 }, unavailable503: 0, otherStatus: 0, transportErrors: 0 },
  },
  resources: { rssKb: { peak: rssKb }, cpuSeconds: cpu, serverExit: exit },
});

function results(baseline, candidate) {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'transport-regression-'));
  dirs.push(dir);
  for (const [build, list] of [['baseline', baseline], ['candidate', candidate]]) {
    list.forEach((r, i) => {
      const d = path.join(dir, `${build}-${i + 1}`);
      fs.mkdirSync(d);
      fs.writeFileSync(path.join(d, 'summary.json'), JSON.stringify(r.summary));
      fs.writeFileSync(path.join(d, 'resources.json'), JSON.stringify(r.resources));
    });
  }
  return dir;
}

const three = (o) => [run(o), run(o), run(o)];

test('matching builds pass every budget', () => {
  const r = evaluate(results(three(), three()));
  assert.deepEqual(r.problems, []);
  assert.ok(r.ok, markdown(r));
});

test('the 10% floor applies when the baseline does not vary', () => {
  const ok = evaluate(results(three(), three({ cpu: 43.9 })));
  assert.ok(ok.ok);
  const fail = evaluate(results(three(), three({ cpu: 44.1 })));
  assert.equal(fail.ok, false);
  assert.equal(fail.rows.find((x) => x.metric === 'CPU seconds').pass, false);
});

test('3 × CV widens the tolerance and latency keeps its absolute slack', () => {
  // Baseline p95 2, 3, 4 ms: mean 3, CV 1/3, t = 100%: limit 3 × 2 + 2 = 8 ms.
  const b = [run({ p95: 2 }), run({ p95: 3 }), run({ p95: 4 })];
  const row = (c) => evaluate(results(b, three({ p95: c }))).rows.find((x) => x.metric === 'Ingest ACK latency p95');
  assert.equal(row(8).pass, true);
  assert.equal(row(8.1).pass, false);
  assert.ok(Math.abs(row(8).t - 1) < 1e-9);
});

test('throughput is bounded below', () => {
  const r = evaluate(results(three(), three({ eps: 13400 })));
  assert.equal(r.rows.find((x) => x.metric === 'Acknowledged events/s').pass, false);
  assert.ok(evaluate(results(three(), three({ eps: 13600 }))).ok);
});

test('one unexpected failure in any run fails', () => {
  const c = three();
  c[1] = run({ failures: 1 });
  const r = evaluate(results(three(), c));
  assert.equal(r.ok, false);
  assert.equal(r.rows.at(-1).pass, false);
});

test('incomplete evidence fails before any budget is checked', () => {
  for (const [why, b, c] of [
    ['too few runs', [run(), run()], [run(), run()]],
    ['unequal runs', [...three(), run()], three()],
    ['an invalid workload', three(), [run(), run(), run({ valid: false })]],
    ['waves', three(), [run(), run(), run({ noWaves: false })]],
    ['a failed stop', three(), [run(), run(), run({ exit: 1 })]],
    ['a candidate without transport counters', three(), [run(), run(), run({ counters: false })]],
    ['a missing metric', three(), [run(), run(), run({ q95: null })]],
  ]) {
    const r = evaluate(results(b, c));
    assert.equal(r.ok, false, why);
    assert.ok(r.problems.length > 0, why);
    assert.ok(r.rows.every((x) => x.pass === undefined), why);
    assert.match(markdown(r), /INCOMPLETE/, why);
  }
  assert.equal(evaluate(path.join(os.tmpdir(), 'no-such-transport-results')).ok, false);
});

test('a baseline without transport counters is still comparable', () => {
  const r = evaluate(results(three({ counters: false }), three()));
  assert.deepEqual(r.problems, []);
  assert.ok(r.ok);
});
