// Tests for the AUTH-PERF-02 benchmark report (sessionauth-report.mjs): incomplete evidence fails
// before any budget is checked, and the budgets themselves.
//
//   node --test scripts/bench/*.test.mjs
//
// Each test writes a results directory laid out like scripts/bench/sessionauth.sh output.

import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { afterEach, test } from 'node:test';

import { CELLS, evaluate } from './sessionauth-report.mjs';

const SCRIPT = path.join(import.meta.dirname, 'sessionauth-report.mjs');
const dirs = [];
afterEach(() => {
    for (const d of dirs.splice(0)) fs.rmSync(d, { recursive: true, force: true });
});

const statuses = (ok) => ({
    ok,
    unauthorized401: 0,
    forbidden403: 0,
    rejected429: 0,
    unavailable503: 0,
    serverError5xx: 0,
    otherStatus: 0,
    timeouts: 0,
    transportErrors: 0,
});

const latency = (p99Ms) => ({ samples: 100, p50Ms: p99Ms / 4, p95Ms: p99Ms / 2, p99Ms, maxMs: p99Ms * 2 });

const op = (ok, p99Ms) => ({ statuses: statuses(ok), cold: latency(p99Ms), warm: latency(p99Ms) });

/** A passing run of `cell` with `credential`; `edit` changes its summary and server record. */
function run(cell, credential, edit = () => {}) {
    const [workload, clients] = cell.split('-');
    const mixed = workload === 'mixed';
    const session = credential === 'session';
    const summary = {
        auth: credential,
        workload,
        args: {
            concurrency: clients === 'high' ? 32 : 4,
            seedEvents: 20000,
            ingestConcurrency: mixed ? 4 : 0,
            batch: 100,
            writeIntervalMs: 50,
            probeIntervalMs: 10,
            coldMs: 3000,
            timeoutMs: 10000,
        },
        load: { requestsPerSecond: 1000, all: op(10000, 5), search: op(3000, 5), facets: op(3000, 5), dashboard: op(3000, 5) },
        probe: op(1000, 2),
        heartbeat: op(1000, 1),
        ingest: {
            requests: op(mixed ? 500 : 0, 3),
            acknowledgedEvents: mixed ? 50000 : 0,
            eventsPerSecond: mixed ? 2500 : 0,
            seededEvents: 20000,
            persistedEvents: 20000 + (mixed ? 50000 : 0),
        },
        writes: { pairs: mixed ? 100 : 0, requests: op(mixed ? 200 : 0, 3) },
        sessionLookups: {
            lookups: session ? 11200 : 0,
            perSecond: session ? 560 : 0,
            found: session ? 11200 : 0,
            failed: 0,
            peakInFlight: session ? 8 : 1,
            meanQueueMicros: session ? 40 : null,
            meanLockMicros: session ? 10 : null,
            meanQueryMicros: session ? 80 : null,
            maxQueueMicros: 0,
            maxLockMicros: 0,
            maxQueryMicros: 0,
        },
        reconciled: true,
    };
    const server = { cpuSeconds: 10, peakMemoryBytes: 100 * 2 ** 20 };
    edit(summary, server);
    return { summary, server };
}

/** Writes `runs` runs of every cell for both credentials; `change(credential, cell, i)` may edit or drop one (return null). */
function results({ cells = CELLS, runs = 3, change = () => undefined } = {}) {
    const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'sessionauth-report-'));
    dirs.push(dir);
    for (const cell of cells) {
        for (const credential of ['key', 'session']) {
            for (let i = 1; i <= runs; i++) {
                if (change(credential, cell, i) === null) continue;
                const r = run(cell, credential, (s, p) => change(credential, cell, i, s, p));
                const d = path.join(dir, cell, credential, `run${i}`);
                fs.mkdirSync(d, { recursive: true });
                fs.writeFileSync(path.join(d, 'summary.json'), JSON.stringify(r.summary));
                fs.writeFileSync(path.join(d, 'server.json'), JSON.stringify(r.server));
            }
        }
    }
    return dir;
}

const failures = (dir, opts) => evaluate(dir, opts).failures;

/** Results where `edit` changes every session run of `cell`. */
const session = (cell, edit) => results({ change: (k, c, i, s, p) => s && k === 'session' && c === cell && edit(s, p) });

test('complete matched evidence within budget passes', () => {
    assert.deepEqual(failures(results()), []);
    const cli = spawnSync(process.execPath, [SCRIPT, results()], { encoding: 'utf8' });
    assert.equal(cli.status, 0, cli.stderr);
    assert.match(cli.stdout, /All budgets met\./);
});

test('an empty or missing results directory fails', () => {
    const empty = fs.mkdtempSync(path.join(os.tmpdir(), 'sessionauth-report-'));
    dirs.push(empty);
    const f = failures(empty);
    assert.equal(f.filter((x) => x.endsWith(': no results')).length, 12, f.join('\n'));
    assert.ok(f.includes('auth-low session: 0 usable runs, at least 3 required'));
    const cli = spawnSync(process.execPath, [SCRIPT, path.join(empty, 'nothing-here')], { encoding: 'utf8' });
    assert.equal(cli.status, 1);
    assert.doesNotMatch(cli.stdout, /All budgets met/);
});

test('a missing credential, too few runs or unequal counts fail', () => {
    const f = failures(results({ change: (k, c) => (k === 'key' && c === 'mixed-high' ? null : undefined) }));
    assert.deepEqual(f, [
        'mixed-high key: no results',
        'mixed-high key: 0 usable runs, at least 3 required',
        'mixed-high: 0 key runs but 3 session runs',
    ]);
    assert.ok(failures(results({ runs: 2 })).includes('auth-low key: 2 usable runs, at least 3 required'));
    assert.deepEqual(failures(results({ runs: 2 }), { minRuns: 2 }), []);
    const g = failures(results({ runs: 4, change: (k, c, i) => (k === 'session' && i === 4 ? null : undefined) }));
    assert.ok(g.includes('query-low: 4 key runs but 3 session runs'), g.join('\n'));
});

test('unreadable runs, mislabelled runs and missing or non-finite gated values are unusable', () => {
    const cases = [
        [(s) => delete s.probe, 'summary.json is missing sections'],
        [(s) => (s.auth = 'key'), 'credential key in a session directory'],
        [(s) => (s.workload = 'auth'), 'workload auth in a query-low cell'],
        [(s) => (s.probe.warm.p99Ms = null), 'probe p99 is null'],
        [(s) => (s.sessionLookups.meanLockMicros = null), 'mean session lookup is null'],
        [(s, p) => (p.cpuSeconds = 'NaN'), 'CPU seconds is "NaN"'],
        [(s, p) => (p.peakMemoryBytes = 0), 'peak memory not measured'],
        [(s) => (s.load.all.statuses.ok = 0), 'no load request succeeded'],
        [(s) => (s.probe.warm.samples = 0), 'no warm auth probe measured'],
    ];
    for (const [edit, reason] of cases) {
        const f = failures(results({ change: (k, c, i, s, p) => k === 'session' && c === 'query-low' && i === 1 && s && edit(s, p) }));
        assert.ok(f.includes(`query-low session run1: unusable, ${reason}`), `${reason}:\n${f.join('\n')}`);
        assert.ok(f.includes('query-low session: 2 usable runs, at least 3 required'));
    }
    const dir = results();
    fs.writeFileSync(path.join(dir, 'auth-high', 'key', 'run2', 'summary.json'), '{ truncated');
    assert.ok(failures(dir).some((x) => x.startsWith('auth-high key run2: unusable, unreadable')));
    const g = failures(results({ change: (k, c, i, s) => s && k === 'session' && c === 'auth-high' && (s.args.batch = 1) }));
    assert.ok(g.some((x) => x.startsWith('auth-high: runs used different workloads')), g.join('\n'));
});

test('--cells restricts the expected cells and refuses others', () => {
    const subset = CELLS.filter((c) => c.startsWith('mixed'));
    assert.deepEqual(failures(results({ cells: subset, runs: 5 }), { cells: '^mixed' }), []);
    assert.equal(failures(results({ cells: subset })).filter((x) => x.endsWith(': no results')).length, 8);
    assert.equal(failures(results(), { cells: '^mixed' }).filter((x) => x.startsWith('unexpected cell')).length, 4);
    assert.deepEqual(failures(results(), { cells: 'nomatch' }).slice(0, 1), [`--cells nomatch matches none of ${CELLS.join(', ')}`]);
});

test('relative budgets: throughput, load and probe p99, CPU and memory', () => {
    assert.deepEqual(failures(session('query-high', (s) => (s.load.requestsPerSecond = 900))), []);
    assert.deepEqual(failures(session('query-high', (s) => (s.load.requestsPerSecond = 899))), ['query-high: requests/s 899 < 90% of 1000']);
    assert.deepEqual(failures(session('query-high', (s) => (s.load.all.warm.p99Ms = 6))), []);
    assert.deepEqual(failures(session('query-high', (s) => (s.load.all.warm.p99Ms = 6.01))), ['query-high: load p99 ms 6.01 > max(110%, +1 ms) of 5.00']);
    assert.deepEqual(failures(session('query-high', (s) => (s.probe.warm.p99Ms = 3.01))), ['query-high: probe p99 ms 3.01 > max(110%, +1 ms) of 2.00']);
    assert.deepEqual(failures(session('query-high', (s, p) => (p.cpuSeconds = 11.5))), ['query-high: CPU µs/request 1150 > 110% of 1000']);
    assert.deepEqual(failures(session('query-high', (s, p) => (p.peakMemoryBytes = 120 * 2 ** 20))), ['query-high: peak MiB 120 > 110% of 100']);
    // The key runs are the reference: a slower key run does not fail.
    const k = results({ change: (c, cell, i, s) => s && c === 'key' && cell === 'auth-low' && (s.load.requestsPerSecond = 500) });
    assert.deepEqual(failures(k), []);
});

test('absolute budgets apply to every session run', () => {
    const f = failures(results({ change: (k, c, i, s) => s && k === 'session' && c === 'auth-low' && i === 2 && (s.sessionLookups.meanQueueMicros = 950) }));
    assert.deepEqual(f, ['auth-low session run2: mean session lookup 1040 µs > 1000 µs']);
    const g = failures(results({ change: (k, c, i, s) => s && c === 'mixed-high' && i === 1 && (s.probe.warm.p99Ms = 26) }));
    assert.deepEqual(g, ['mixed-high session run1: probe p99 26 ms > 25 ms'], 'the key run is not held to it');
});

test('credential isolation: key runs use no session, session runs one per request', () => {
    const f = failures(results({ change: (k, c, i, s) => s && k === 'key' && c === 'auth-low' && i === 1 && (s.sessionLookups.lookups = 1) }));
    assert.deepEqual(f, ['auth-low key run1: 1 session checks in a key-only run (a cookie was sent)']);
    const h = failures(results({ change: (k, c, i, s) => s && k === 'key' && c === 'auth-low' && i === 1 && (s.sessionLookups.cacheHits = 3) }));
    assert.deepEqual(h, ['auth-low key run1: 3 session checks in a key-only run (a cookie was sent)']);
    const g = failures(results({ change: (k, c, i, s) => s && k === 'session' && c === 'query-low' && i === 3 && (s.sessionLookups.lookups = 10999) }));
    assert.deepEqual(g, ['query-low session run3: 10999 session checks for 11000 authenticated requests']);
    // Cache hits count: one SQLite lookup and the rest from the cache.
    const cached = (s) => Object.assign(s.sessionLookups, { lookups: 1, cacheHits: 11199 });
    assert.deepEqual(failures(results({ change: (k, c, i, s) => s && k === 'session' && cached(s) })), []);
});

test('correctness checks apply to every run of both credentials', () => {
    const f = failures(results({ change: (k, c, i, s) => s && k === 'key' && c === 'mixed-low' && i === 2 && (s.reconciled = false) }));
    assert.deepEqual(f, ['mixed-low key run2: acknowledged events not all persisted']);
    const g = failures(results({ change: (k, c, i, s) => s && k === 'session' && c === 'auth-low' && i === 1 && (s.load.all.statuses.unauthorized401 = 1) }));
    assert.equal(g.length, 1);
    assert.match(g[0], /^auth-low session run1: load /);
    const h = failures(results({ change: (k, c, i, s) => s && k === 'session' && c === 'mixed-high' && i === 1 && (s.writes.requests.statuses.unavailable503 = 1) }));
    assert.match(h[0], /^mixed-high session run1: metadata writes /);
    assert.deepEqual(failures(results({ change: (k, c, i, s) => s && c === 'query-high' && (s.load.all.statuses.unavailable503 = 5) })), [], 'busy query slots are reported, not failures');
    const j = failures(results({ change: (k, c, i, s) => s && k === 'session' && c === 'query-low' && i === 1 && (s.probe.statuses.unavailable503 = 1) }));
    assert.match(j[0], /^query-low session run1: auth probe /);
    const l = failures(results({ change: (k, c, i, s) => s && k === 'session' && c === 'auth-high' && i === 1 && (s.sessionLookups.failed = 2) }));
    assert.deepEqual(l, ['auth-high session run1: 2 session lookups failed']);
});
