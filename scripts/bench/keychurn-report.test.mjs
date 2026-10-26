// Tests for the AUTH-PERF-01 benchmark report (keychurn-report.mjs): incomplete evidence fails
// before any budget is checked, and the budgets themselves.
//
//   node --test scripts/bench/*.test.mjs
//
// Each test writes a results directory laid out like scripts/bench/keychurn.sh output.

import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { afterEach, test } from 'node:test';

import { CELLS, evaluate } from './keychurn-report.mjs';

const SCRIPT = path.join(import.meta.dirname, 'keychurn-report.mjs');
const dirs = [];
afterEach(() => {
    for (const d of dirs.splice(0)) fs.rmSync(d, { recursive: true, force: true });
});

const statuses = (ok) => ({
    ok,
    unauthorized401: 0,
    rejected429: 0,
    serverError5xx: 0,
    otherStatus: 0,
    timeouts: 0,
    transportErrors: 0,
});

/** A passing run of `cell`; `edit` changes its summary and server record. */
function run(cell, edit = () => {}) {
    const churn = cell.endsWith('-churn');
    const summary = {
        args: { concurrency: cell.startsWith('high') ? 64 : 4, batch: 1, churnIntervalMs: churn ? 20 : 0, timeoutMs: 10000 },
        ingest: {
            statuses: statuses(1000),
            acknowledgedEvents: 100000,
            eventsPerSecond: 5000,
            requestsPerSecond: 5000,
            ackLatency: { samples: 1000, p50Ms: 1, p95Ms: 2, p99Ms: 3, maxMs: 9 },
        },
        auth: { statuses: { ...statuses(0), unauthorized401: 100 }, latency: { samples: 100, p50Ms: 0.5, p99Ms: 2, maxMs: 4 } },
        heartbeat: { statuses: statuses(100), latency: { samples: 100, p99Ms: 1 } },
        churn: {
            pairs: churn ? 50 : 0,
            create: { statuses: statuses(churn ? 50 : 0), latency: { p99Ms: churn ? 2 : null } },
            revoke: { statuses: statuses(churn ? 50 : 0), latency: { p99Ms: churn ? 2 : null } },
        },
        reconciled: true,
    };
    const server = { cpuSeconds: 10, peakMemoryBytes: 100 * 2 ** 20 };
    edit(summary, server);
    return { summary, server };
}

/** Writes `runs` runs of every cell for both binaries; `change(binary, cell, i)` may edit or drop one (return null). */
function results({ cells = CELLS, runs = 3, change = () => undefined } = {}) {
    const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'keychurn-report-'));
    dirs.push(dir);
    for (const binary of ['baseline', 'candidate']) {
        for (const cell of cells) {
            for (let i = 1; i <= runs; i++) {
                const r = change(binary, cell, i) === null ? null : run(cell, (s, p) => change(binary, cell, i, s, p));
                if (!r) continue;
                const d = path.join(dir, binary, cell, `run${i}`);
                fs.mkdirSync(d, { recursive: true });
                fs.writeFileSync(path.join(d, 'summary.json'), JSON.stringify(r.summary));
                fs.writeFileSync(path.join(d, 'server.json'), JSON.stringify(r.server));
            }
        }
    }
    return dir;
}

const failures = (dir, opts) => evaluate(dir, opts).failures;

test('complete matched evidence within budget passes', () => {
    assert.deepEqual(failures(results()), []);
    const cli = spawnSync(process.execPath, [SCRIPT, results()], { encoding: 'utf8' });
    assert.equal(cli.status, 0, cli.stderr);
    assert.match(cli.stdout, /All budgets met\./);
});

test('an empty or missing results directory fails', () => {
    const empty = fs.mkdtempSync(path.join(os.tmpdir(), 'keychurn-report-'));
    dirs.push(empty);
    const f = failures(empty);
    assert.equal(f.filter((x) => x.endsWith(': no results')).length, 16, f.join('\n'));
    assert.ok(f.some((x) => x === 'baseline low-small-steady: 0 usable runs, at least 3 required'));
    const cli = spawnSync(process.execPath, [SCRIPT, path.join(empty, 'nothing-here')], { encoding: 'utf8' });
    assert.equal(cli.status, 1);
    assert.doesNotMatch(cli.stdout, /All budgets met/);
});

test('a missing cell or binary fails', () => {
    const f = failures(results({ change: (b, c) => (b === 'baseline' && c === 'high-batched-churn' ? null : undefined) }));
    assert.deepEqual(f, [
        'baseline high-batched-churn: no results',
        'baseline high-batched-churn: 0 usable runs, at least 3 required',
        'high-batched-churn: 0 baseline runs but 3 candidate runs',
    ]);
    const g = failures(results({ change: (b) => (b === 'candidate' ? null : undefined) }));
    assert.equal(g.filter((x) => x.startsWith('candidate') && x.includes('0 usable runs')).length, 8);
});

test('fewer runs than required, or unequal counts, fail', () => {
    assert.ok(failures(results({ runs: 2 })).includes('baseline low-small-steady: 2 usable runs, at least 3 required'));
    assert.deepEqual(failures(results({ runs: 2 }), { minRuns: 2 }), []);
    const f = failures(results({ runs: 4, change: (b, c, i) => (b === 'candidate' && i === 4 ? null : undefined) }));
    assert.ok(f.includes('low-small-steady: 4 baseline runs but 3 candidate runs'), f.join('\n'));
});

test('unreadable runs and missing or non-finite gated values are unusable', () => {
    const cases = [
        [(s) => delete s.auth, 'summary.json is missing sections'],
        [(s) => (s.auth.latency.p99Ms = null), 'auth p99 is null'],
        [(s, p) => (p.cpuSeconds = 'NaN'), 'CPU seconds is "NaN"'],
        [(s, p) => (p.peakMemoryBytes = 0), 'peak memory not measured'],
        [(s) => (s.ingest.acknowledgedEvents = 0), 'no events acknowledged'],
        [(s) => (s.auth.latency.samples = 0), 'no auth probe measured'],
    ];
    for (const [edit, reason] of cases) {
        const f = failures(results({ change: (b, c, i, s, p) => b === 'candidate' && c === 'low-small-steady' && i === 1 && s && edit(s, p) }));
        assert.ok(f.includes(`candidate low-small-steady run1: unusable, ${reason}`), `${reason}:\n${f.join('\n')}`);
        assert.ok(f.includes('candidate low-small-steady: 2 usable runs, at least 3 required'));
    }
    const dir = results();
    fs.writeFileSync(path.join(dir, 'baseline', 'low-small-churn', 'run2', 'summary.json'), '{ truncated');
    assert.ok(failures(dir).some((x) => x.startsWith('baseline low-small-churn run2: unusable, unreadable')));
});

test('a run whose workload does not match its cell fails', () => {
    const f = failures(results({ change: (b, c, i, s) => s && c === 'low-small-churn' && i === 1 && (s.args.churnIntervalMs = 0) }));
    assert.ok(f.some((x) => x.includes('low-small-churn run1: unusable, churn off in a low-small-churn cell')), f.join('\n'));
    const g = failures(results({ change: (b, c, i, s) => s && b === 'candidate' && c === 'high-small-steady' && (s.args.batch = 100) }));
    assert.ok(g.some((x) => x.startsWith('high-small-steady: runs used different workloads')), g.join('\n'));
    const h = failures(results({ change: (b, c, i, s) => s && c === 'low-small-churn' && (s.churn.pairs = 0) }));
    assert.ok(h.some((x) => x.endsWith('unusable, no key changes in a churn cell')));
});

test('--cells restricts the expected cells and refuses others', () => {
    const subset = CELLS.filter((c) => c.startsWith('high-batched'));
    assert.deepEqual(failures(results({ cells: subset, runs: 5 }), { cells: '^high-batched' }), []);
    assert.equal(failures(results({ cells: subset }), {}).filter((x) => x.endsWith(': no results')).length, 12);
    const f = failures(results(), { cells: '^high-batched' });
    assert.equal(f.filter((x) => x.includes('unexpected cell')).length, 12);
    assert.deepEqual(failures(results(), { cells: 'nomatch' }).slice(0, 1), [`--cells nomatch matches none of ${CELLS.join(', ')}`]);
});

test('budgets: throughput, CPU, auth p99 and memory', () => {
    const cand = (edit) => results({ change: (b, c, i, s, p) => s && b === 'candidate' && c === 'high-batched-churn' && edit(s, p) });
    assert.deepEqual(failures(cand((s) => (s.ingest.eventsPerSecond = 4600))), []);
    assert.deepEqual(failures(cand((s) => (s.ingest.eventsPerSecond = 4400))), ['high-batched-churn: events/s 4400 < 90% of 5000']);
    assert.deepEqual(failures(cand((s, p) => (p.cpuSeconds = 11.5))), ['high-batched-churn: CPU µs/event 115 > 110% of 100']);
    assert.deepEqual(failures(cand((s) => (s.auth.latency.p99Ms = 3))), []);
    assert.deepEqual(failures(cand((s) => (s.auth.latency.p99Ms = 3.01))), ['high-batched-churn: auth p99 ms 3.01 > max(110%, +1 ms) of 2.00']);
    assert.deepEqual(failures(cand((s, p) => (p.peakMemoryBytes = 120 * 2 ** 20))), ['high-batched-churn: peak MiB 120 > 110% of 100']);
});

test('correctness checks apply to every run of both binaries', () => {
    const f = failures(results({ change: (b, c, i, s) => s && b === 'baseline' && c === 'low-small-steady' && i === 2 && (s.reconciled = false) }));
    assert.deepEqual(f, ['baseline low-small-steady run2: acknowledged events not all persisted']);
    const g = failures(results({ change: (b, c, i, s) => s && c === 'low-small-churn' && i === 1 && b === 'candidate' && (s.churn.revoke.statuses.serverError5xx = 1) }));
    assert.equal(g.length, 1);
    assert.match(g[0], /^candidate low-small-churn run1: key revoke /);
});
