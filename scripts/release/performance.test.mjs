// Tests for the pre-tag performance sign-off (performance.mjs) and its release wiring.
//
//   node --test scripts/release/*.test.mjs
//
// Each test builds a throwaway git repository holding a workspace at the release version,
// the release scripts, performance-gates.json and compat-gates.json, plus an evidence
// directory laid out like the release-evidence branch. Tagging also needs the compatibility
// gate (compat.test.mjs covers it); these tests give it a passing record for the same
// candidate.

import assert from 'node:assert/strict';
import { execFileSync, spawnSync } from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { after, before, describe, test } from 'node:test';

import {
    checkArtifactsMatchApproval,
    checkPerformance,
    GATES_FILE,
    METRIC_CATEGORIES,
    pretag,
    recordProblems,
    requiredGates,
} from './performance.mjs';
import { GateError, ROOT } from './release.mjs';

const HERE = import.meta.dirname;
const FIXTURE = JSON.parse(fs.readFileSync(path.join(HERE, 'testdata/performance/0.1.2/PERF-GATE-1.json'), 'utf8'));
const COMPAT = JSON.parse(fs.readFileSync(path.join(HERE, 'testdata/compat/0.1.2/COMPAT-GATE-0.1.2.json'), 'utf8'));
const temps = [];
const tmp = () => {
  const d = fs.mkdtempSync(path.join(os.tmpdir(), 'vyrtel-perf-test-'));
  temps.push(d);
  return d;
};

// Keep the user's git config (signing, hooks, templates) out of the throwaway repositories.
const savedEnv = { ...process.env };
before(() => {
  const empty = path.join(tmp(), 'gitconfig');
  fs.writeFileSync(empty, '');
  Object.assign(process.env, { GIT_CONFIG_GLOBAL: empty, GIT_CONFIG_NOSYSTEM: '1' });
});
after(() => {
  for (const k of ['GIT_CONFIG_GLOBAL', 'GIT_CONFIG_NOSYSTEM']) {
    if (savedEnv[k] === undefined) delete process.env[k];
    else process.env[k] = savedEnv[k];
  }
  temps.forEach((d) => fs.rmSync(d, { recursive: true, force: true }));
});

const git = (cwd, ...args) => execFileSync('git', args, { cwd, encoding: 'utf8', stdio: 'pipe' }).trim();
const gateError = (pattern) => (e) => e instanceof GateError && pattern.test(e.message);

/** A committed repository at `version` with the release scripts and the real gate manifest. */
function repo(version = '0.1.2') {
  const root = tmp();
  fs.writeFileSync(path.join(root, 'Cargo.toml'), `[workspace]\nmembers = []\n\n[workspace.package]\nversion = "${version}"\n`);
  fs.writeFileSync(path.join(root, 'Cargo.lock'), `version = 4\n\n[[package]]\nname = "server"\nversion = "${version}"\n`);
  fs.mkdirSync(path.join(root, 'web'));
  fs.writeFileSync(path.join(root, 'web/package.json'), JSON.stringify({ version }));
  fs.mkdirSync(path.join(root, 'scripts/release'), { recursive: true });
  for (const f of ['release.mjs', 'performance.mjs', 'performance-gates.json', 'compat.mjs', 'compat-gates.json']) {
    fs.copyFileSync(path.join(HERE, f), path.join(root, 'scripts/release', f));
  }
  fs.mkdirSync(path.join(root, 'crates/telemetry/src'), { recursive: true });
  fs.writeFileSync(path.join(root, 'crates/telemetry/src/layout.rs'), 'pub const LAYOUT: &str = "0.1.2";\n');
  git(root, 'init', '--quiet', '--initial-branch=main');
  git(root, 'config', 'user.name', 'Release Test');
  git(root, 'config', 'user.email', 'release-test@example.invalid');
  git(root, 'add', '.');
  git(root, 'commit', '--quiet', '-m', 'candidate');
  return { root, sha: git(root, 'rev-parse', 'HEAD') };
}

/** A passing record (the documented fixture) for `sha`, with `edit` applied. */
function record(sha, edit = (r) => r, base = FIXTURE) {
  const r = structuredClone(base);
  r.candidate.sha = sha;
  return edit(r) ?? r;
}

/**
 * An evidence directory laid out like the release-evidence branch: the performance records,
 * and a passing compatibility record for the candidate of each 0.1.2 record.
 */
function evidence(records) {
  const dir = tmp();
  const write = (kind, r) => {
    const d = path.join(dir, kind, r.version);
    fs.mkdirSync(d, { recursive: true });
    fs.writeFileSync(path.join(d, `${r.gate}.json`), JSON.stringify(r, null, 2));
  };
  for (const r of records) {
    write('performance', r);
    if (r.version === COMPAT.version) write('compat', { ...structuredClone(COMPAT), candidate: { ...COMPAT.candidate, sha: r.candidate.sha } });
  }
  return { dir };
}

const tags = (root) => git(root, 'tag', '--list');

/** Runs the CLI inside a throwaway repository, as a maintainer would. */
function cli(root, ...args) {
  return spawnSync(process.execPath, ['scripts/release/release.mjs', ...args], { cwd: root, encoding: 'utf8', env: process.env });
}

describe('TC-PERF-01: missing or failed evidence prevents tagging and publication', () => {
  test('no approval record: pretag refuses and creates no tag', () => {
    const { root, sha } = repo();
    assert.throws(() => pretag({ tag: 'v0.1.2', cwd: root, evidence: evidence([]), create: true }), gateError(/PERF-GATE-1 \(#170\): no approval record at release-evidence:performance\/0\.1\.2\/PERF-GATE-1\.json/));
    assert.throws(() => checkPerformance({ tag: 'v0.1.2', sha, evidence: evidence([]), root }), gateError(/no approval record/));
    assert.equal(tags(root), '');
  });

  test('CLI exits 1 and creates no tag without sign-off', () => {
    const { root } = repo();
    const empty = evidence([]).dir;
    const r = cli(root, 'pretag', '--tag', 'v0.1.2', '--evidence', empty, '--create');
    assert.equal(r.status, 1, r.stdout + r.stderr);
    assert.match(r.stderr, /release gate: performance sign-off for v0\.1\.2 is incomplete/);
    assert.equal(tags(root), '');
  });

  test('failed verdicts and failing metrics are refused', () => {
    const { root, sha } = repo();
    const cases = [
      [(r) => void (r.review.verdict = 'FAIL'), /review verdict is "FAIL", not PASS/],
      [(r) => void delete r.review.reviewer, /review has no reviewer/],
      [(r) => void Object.assign(r.metrics[0], { value: 60, delta: 14, result: 'fail' }), /footprint: steady RSS\): FAILED \(60 MiB/],
      [(r) => void Object.assign(r.metrics[0], { value: 60, delta: 14 }), /marked pass but 60 MiB is outside its limit/],
      [(r) => void (r.metrics[2].limit = {}), /no numeric limit/],
      [(r) => void (r.metrics[3].delta = 9), /delta 9 is not value − baseline/],
      [(r) => void delete r.metrics[4].noBaseline, /no baseline; give one or a "noBaseline" reason/],
      [(r) => void (r.metrics = r.metrics.filter((m) => m.category !== 'storage')), /no "storage" rows/],
      [(r) => void (r.metrics[8].na = ' '), /N\/A needs a reason/],
      [(r) => void (r.reports = []), /no profiling report links/],
      [(r) => void (r.candidate.artifacts = []), /lists no profiled artifact hashes/],
      [(r) => void (r.baseline = {}), /baseline needs the prior approved release/],
      [(r) => void (r.budgetsDeclaredAt = '2026-10-02T10:00:00Z'), /budgets were declared after the candidate was measured/],
    ];
    for (const [edit, expected] of cases) {
      const ev = evidence([record(sha, edit)]);
      assert.throws(() => checkPerformance({ tag: 'v0.1.2', sha, evidence: ev, root }), gateError(expected), String(expected));
      assert.throws(() => pretag({ tag: 'v0.1.2', cwd: root, evidence: ev, create: true }), gateError(expected));
    }
    assert.equal(tags(root), '');
  });

  test('a stable version with no declared gates cannot be released', () => {
    const { root, sha } = repo('0.9.0');
    assert.throws(() => checkPerformance({ tag: 'v0.9.0', sha, evidence: evidence([]), root }), gateError(/declares no performance gates for 0\.9\.0/));
  });

  test('a passing post-tag workflow cannot substitute for pre-tag sign-off', () => {
    const { root, sha } = repo();
    const ev = evidence([record(sha)]);
    // Reviewed after the tag was created.
    assert.throws(() => checkPerformance({ tag: 'v0.1.2', sha, evidence: ev, root, tagDate: '2026-10-02T12:00:00Z' }), gateError(/reviewed after v0\.1\.2 was created; sign-off must happen before tagging/));
    // A lightweight tag (git tag v0.1.2) did not go through the pre-tag procedure.
    assert.throws(() => checkPerformance({ tag: 'v0.1.2', sha, evidence: ev, root, tagDate: '' }), gateError(/not an annotated tag from the pre-tag procedure/));
  });

  test('uncommitted changes and existing tags are refused', () => {
    const { root, sha } = repo();
    const ev = evidence([record(sha)]);
    fs.appendFileSync(path.join(root, 'Cargo.toml'), '# local edit\n');
    assert.throws(() => pretag({ tag: 'v0.1.2', cwd: root, evidence: ev }), gateError(/uncommitted changes/));
    git(root, 'checkout', '--', 'Cargo.toml');
    git(root, 'tag', 'v0.1.2');
    assert.throws(() => pretag({ tag: 'v0.1.2', cwd: root, evidence: ev }), gateError(/tag v0\.1\.2 already exists/));
  });
});

describe('TC-PERF-02: stale or mismatched evidence is rejected until revalidated', () => {
  test('changing the candidate after PASS invalidates the sign-off', () => {
    const { root, sha } = repo();
    const ev = evidence([record(sha)]);
    assert.equal(pretag({ tag: 'v0.1.2', cwd: root, evidence: ev }).sha, sha);

    fs.writeFileSync(path.join(root, 'fix.txt'), 'late change');
    git(root, 'add', '.');
    git(root, 'commit', '--quiet', '-m', 'late fix');
    const changed = git(root, 'rev-parse', 'HEAD');
    assert.throws(() => pretag({ tag: 'v0.1.2', cwd: root, evidence: ev, create: true }), gateError(new RegExp(`stale: approved candidate ${sha.slice(0, 12)} but the release commit is ${changed.slice(0, 12)}`)));
    assert.equal(tags(root), '');

    // Revalidated: a new PASS for the changed commit is accepted.
    assert.equal(pretag({ tag: 'v0.1.2', cwd: root, evidence: evidence([record(changed)]) }).sha, changed);
  });

  test('gates sharing a version must sign off the same candidate', () => {
    const { root, sha } = repo('0.2.0');
    const other = 'f'.repeat(40);
    const gate = (id, issue, candidate) => record(candidate, (r) => Object.assign(r, { gate: id, issue, version: '0.2.0' }));
    const split = evidence([gate('PERF-GATE-3', 172, sha), gate('PERF-GATE-4', 173, other)]);
    assert.throws(() => checkPerformance({ tag: 'v0.2.0', sha, evidence: split, root }), gateError(/signed off different candidates/));
    assert.throws(() => checkPerformance({ tag: 'v0.2.0', sha, evidence: evidence([gate('PERF-GATE-3', 172, sha)]), root }), gateError(/PERF-GATE-4 \(#173\): no approval record/));
    const both = evidence([gate('PERF-GATE-3', 172, sha), gate('PERF-GATE-4', 173, sha)]);
    assert.deepEqual(checkPerformance({ tag: 'v0.2.0', sha, evidence: both, root }).gates.map((g) => g.id), ['PERF-GATE-3', 'PERF-GATE-4']);
  });

  test('a record for another gate or version is not accepted', () => {
    const { sha } = repo();
    const gate = { id: 'PERF-GATE-1', issue: 170 };
    assert.match(recordProblems(record(sha, (r) => void (r.gate = 'PERF-GATE-2')), { gate, version: '0.1.2', sha }).join('\n'), /record is for gate PERF-GATE-2/);
    assert.match(recordProblems(record(sha, (r) => void (r.version = '0.1.1')), { gate, version: '0.1.2', sha }).join('\n'), /record is for version 0\.1\.1/);
  });

  test('released artifacts must match the approved hashes or document equivalence', () => {
    const dir = tmp();
    const approved = FIXTURE.candidate.artifacts[0].sha256;
    const sums = (hex) => fs.writeFileSync(path.join(dir, 'SHA256SUMS'), `${hex}  vyrtel-linux-x86_64\n`);

    sums(approved);
    assert.deepEqual(checkArtifactsMatchApproval([FIXTURE], dir), ['PERF-GATE-1: vyrtel-linux-x86_64 matches the approved hash']);

    sums('b'.repeat(64));
    assert.throws(() => checkArtifactsMatchApproval([FIXTURE], dir), gateError(/vyrtel-linux-x86_64 built as bbbbbbbbbbbb but aaaaaaaaaaaa was approved, and the record documents no reproducible equivalence/));

    const equivalent = { ...FIXTURE, equivalence: { method: 'same commit, toolchain and lock file; rebuilt on the release runner', evidence: 'https://example.invalid/run/1' } };
    assert.match(checkArtifactsMatchApproval([equivalent], dir)[0], /differs from the approved hash; equivalent by same commit/);
  });
});

describe('TC-PERF-03: a complete passing matrix for the exact candidate satisfies the gate', () => {
  test('the documented fixture covers every metric category and passes for its candidate', () => {
    assert.deepEqual([...new Set(FIXTURE.metrics.map((m) => m.category))].sort(), [...METRIC_CATEGORIES].sort());
    const sha = FIXTURE.candidate.sha;
    assert.deepEqual(recordProblems(FIXTURE, { gate: { id: 'PERF-GATE-1' }, version: '0.1.2', sha }), []);
  });

  test('CLI pretag --create makes an annotated tag the release workflow then accepts', () => {
    const { root, sha } = repo();
    const ev = evidence([record(sha)]).dir;

    const dry = cli(root, 'pretag', '--tag', 'v0.1.2', '--evidence', ev);
    assert.equal(dry.status, 0, dry.stderr);
    assert.match(dry.stdout, /v0\.1\.2 may be tagged on [0-9a-f]{40}; rerun with --create/);
    assert.equal(tags(root), '');

    const r = cli(root, 'pretag', '--tag', 'v0.1.2', '--evidence', ev, '--create');
    assert.equal(r.status, 0, r.stderr);
    assert.match(r.stdout, /git push origin v0\.1\.2/);
    assert.equal(git(root, 'rev-parse', 'v0.1.2^{commit}'), sha);
    assert.equal(git(root, 'cat-file', '-t', 'v0.1.2'), 'tag', 'annotated');
    assert.match(git(root, 'tag', '-l', '--format=%(contents)', 'v0.1.2'), new RegExp(`Performance-Gate: PERF-GATE-1 PASS ${sha} by example-reviewer at 2026-10-03T09:00:00Z`));

    // What the release workflow's performance job runs for the pushed tag.
    const tagDate = git(root, 'for-each-ref', 'refs/tags/v0.1.2', '--format=%(taggerdate:iso-strict)');
    const check = cli(root, 'check-performance', '--tag', 'v0.1.2', '--sha', sha, '--evidence', ev, '--tag-date', tagDate);
    assert.equal(check.status, 0, check.stderr);
    assert.match(check.stdout, /Performance-Gate: PERF-GATE-1 PASS/);
  });

  test('prereleases need no sign-off; the policy comes from the manifest', () => {
    const { root, sha } = repo('0.1.2-rc.1');
    const r = checkPerformance({ tag: 'v0.1.2-rc.1', sha, evidence: evidence([]), root, tagDate: '' });
    assert.deepEqual(r.gates, []);
    assert.match(r.reason, /prereleases do not need performance sign-off/);
  });

  test('the repository manifest declares a gate with an issue for every delivery version', () => {
    const manifest = JSON.parse(fs.readFileSync(path.join(ROOT, GATES_FILE), 'utf8'));
    const ids = Object.values(manifest.versions).flatMap((v) => v.gates.map((g) => g.id));
    assert.equal(new Set(ids).size, ids.length, 'gate ids are unique');
    for (const [version, entry] of Object.entries(manifest.versions)) {
      assert.match(version, /^\d+\.\d+\.\d+$/);
      for (const g of entry.gates) {
        assert.match(g.id, /^PERF-GATE-\d+$/);
        assert.ok(Number.isInteger(g.issue) && g.issue > 0, `${g.id} links an issue`);
      }
    }
    assert.deepEqual(requiredGates('0.1.2', false, ROOT).gates.map((g) => g.id), ['PERF-GATE-1']);
    assert.deepEqual(requiredGates('0.2.1', false, ROOT).gates.map((g) => g.id), ['PERF-GATE-5', 'PERF-GATE-10']);
  });
});

describe('release workflow wiring', () => {
  const workflow = fs.readFileSync(path.join(ROOT, '.github/workflows/release.yml'), 'utf8').replace(/\r\n/g, '\n');

  test('the performance job blocks the build and checks the tag was created after sign-off', () => {
    assert.match(workflow, /\n {2}binaries:\n(?: {4}.*\n)*? {4}needs: \[preflight, performance, compat, ci\]\n/);
    assert.match(workflow, /check-performance --tag "\$TAG" --sha "\$GITHUB_SHA" --evidence evidence "\$\{args\[@\]\}"/);
    assert.match(workflow, /args=\(--tag-date "\$\(git for-each-ref "refs\/tags\/\$TAG" --format='%\(taggerdate:iso-strict\)'\)"\)/);
  });

  test('verify checks the built artifacts against the approved hashes', () => {
    assert.match(workflow, /check-performance --tag "\$TAG" --sha "\$GITHUB_SHA" --evidence evidence --artifacts artifacts/);
  });
});
