// Tests for the pre-tag compatibility sign-off (compat.mjs) and its release wiring
// (COMPAT-GATE-0.1.2, AC6, AC7, TC5).
//
//   node --test scripts/release/*.test.mjs
//
// Each test builds a throwaway git repository holding a workspace at the release version, the
// release scripts, both gate manifests and the layout identity the candidate writes, plus an
// evidence directory laid out like the release-evidence branch.

import assert from 'node:assert/strict';
import { execFileSync, spawnSync } from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { after, before, describe, test } from 'node:test';

import {
    candidateLayout,
    checkCompat,
    CHECKLIST,
    COMPAT_FILE,
    compatRecordProblems,
    requiredCompatGate
} from './compat.mjs';
import { pretag } from './performance.mjs';
import { GateError, ROOT } from './release.mjs';

const HERE = import.meta.dirname;
const read = (rel) => JSON.parse(fs.readFileSync(path.join(HERE, rel), 'utf8'));
const COMPAT = read('testdata/compat/0.1.2/COMPAT-GATE-0.1.2.json');
const PERF = read('testdata/performance/0.1.2/PERF-GATE-1.json');
const temps = [];
const tmp = () => {
  const d = fs.mkdtempSync(path.join(os.tmpdir(), 'vyrtel-compat-test-'));
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
const LAYOUT_RS = 'crates/telemetry/src/layout.rs';
const layoutSource = (layout) => `pub const LAYOUT: &str = "${layout}";\n`;

/** A committed repository at `version` with the release scripts and the real manifests. */
function repo(version = '0.1.2', layout = '0.1.2') {
  const root = tmp();
  fs.writeFileSync(path.join(root, 'Cargo.toml'), `[workspace]\nmembers = []\n\n[workspace.package]\nversion = "${version}"\n`);
  fs.writeFileSync(path.join(root, 'Cargo.lock'), `version = 4\n\n[[package]]\nname = "server"\nversion = "${version}"\n`);
  fs.mkdirSync(path.join(root, 'web'));
  fs.writeFileSync(path.join(root, 'web/package.json'), JSON.stringify({ version }));
  fs.mkdirSync(path.join(root, 'scripts/release'), { recursive: true });
  for (const f of ['release.mjs', 'performance.mjs', 'performance-gates.json', 'compat.mjs', 'compat-gates.json']) {
    fs.copyFileSync(path.join(HERE, f), path.join(root, 'scripts/release', f));
  }
  fs.mkdirSync(path.join(root, path.dirname(LAYOUT_RS)), { recursive: true });
  fs.writeFileSync(path.join(root, LAYOUT_RS), layoutSource(layout));
  git(root, 'init', '--quiet', '--initial-branch=main');
  git(root, 'config', 'user.name', 'Release Test');
  git(root, 'config', 'user.email', 'release-test@example.invalid');
  commit(root, 'candidate');
  return { root, sha: git(root, 'rev-parse', 'HEAD') };
}

function commit(root, message) {
  git(root, 'add', '.');
  git(root, 'commit', '--quiet', '-m', message);
  return git(root, 'rev-parse', 'HEAD');
}

/** The documented compatibility record for `sha`, with `edit` applied. */
function compat(sha, edit = (r) => r) {
  const r = structuredClone(COMPAT);
  r.candidate.sha = sha;
  return edit(r) ?? r;
}

/** The documented performance record for `sha`. */
function perf(sha, edit = (r) => r) {
  const r = structuredClone(PERF);
  r.candidate.sha = sha;
  return edit(r) ?? r;
}

/** An evidence directory laid out like the release-evidence branch. */
function evidence({ compat: c = [], performance: p = [] } = {}) {
  const dir = tmp();
  for (const [kind, records] of [['compat', c], ['performance', p]]) {
    for (const r of records) {
      const d = path.join(dir, kind, r.version);
      fs.mkdirSync(d, { recursive: true });
      fs.writeFileSync(path.join(d, `${r.gate}.json`), JSON.stringify(r, null, 2));
    }
  }
  return { dir };
}

const tags = (root) => git(root, 'tag', '--list');

function cli(root, ...args) {
  return spawnSync(process.execPath, ['scripts/release/release.mjs', ...args], { cwd: root, encoding: 'utf8', env: process.env });
}

describe('TC5: missing, failed or stale compatibility evidence blocks tagging and publication', () => {
  test('no compatibility record: pretag refuses and creates no tag, even with performance PASS', () => {
    const { root, sha } = repo();
    const ev = evidence({ performance: [perf(sha)] });
    assert.throws(
      () => pretag({ tag: 'v0.1.2', cwd: root, evidence: ev, create: true }),
      gateError(/COMPAT-GATE-0\.1\.2 \(#229\): no approval record at release-evidence:compat\/0\.1\.2\/COMPAT-GATE-0\.1\.2\.json/),
    );
    assert.equal(tags(root), '');
    const r = cli(root, 'check-compat', '--tag', 'v0.1.2', '--sha', sha, '--evidence', ev.dir);
    assert.equal(r.status, 1, r.stdout + r.stderr);
    assert.match(r.stderr, /release gate: compatibility sign-off for v0\.1\.2 is incomplete/);
  });

  test('failed, incomplete or unapproved records are refused', () => {
    const { root, sha } = repo();
    const item = (name, change) => (r) => void Object.assign(r.checklist.find((c) => c.item === name), change);
    const exception = (e) => (r) => void r.exceptions.push({ what: 'old reader', approvedBy: 'rowan-smith', owner: 'storage', removalRelease: '0.1.3', issue: 300, ...e });
    const cases = [
      [(r) => void (r.review.verdict = 'FAIL'), /review verdict is "FAIL", not PASS/],
      [(r) => void delete r.review.reviewer, /review has no reviewer/],
      [(r) => void (r.review.reviewedAt = 'soon'), /review\.reviewedAt is missing or not a date/],
      [item('removals', { result: 'fail', evidence: 'legacy reader left in' }), /checklist: "removals" FAILED \(legacy reader left in\)/],
      [item('format-reset', { result: 'na', evidence: '' }), /checklist: "format-reset" has no evidence/],
      [item('docs', { result: 'skipped' }), /checklist: "docs" result must be "pass", "na" or "fail"/],
      [(r) => void (r.checklist = r.checklist.filter((c) => c.item !== 'layout-identity')), /checklist: no "layout-identity" item/],
      [(r) => void r.checklist.push({ item: 'vibes', result: 'pass', evidence: 'x' }), /checklist: unknown item "vibes"/],
      [(r) => void delete r.inventory, /no inventory link/],
      [(r) => void (r.candidate.artifacts = []), /lists no reviewed artifact hashes/],
      [(r) => void (r.issue = 1), /record links issue #1, expected #229/],
      [(r) => void delete r.exceptions, /exceptions must be a list/],
      [exception({ approvedBy: 'someone-else' }), /approved by "someone-else", but only rowan-smith may approve an exception/],
      [exception({ owner: '' }), /exceptions\[0\] \(old reader\): no owner/],
      [exception({ removalRelease: undefined }), /no removal release/],
      [exception({ issue: undefined }), /no linked issue/],
    ];
    for (const [edit, expected] of cases) {
      const ev = evidence({ compat: [compat(sha, edit)], performance: [perf(sha)] });
      assert.throws(() => checkCompat({ tag: 'v0.1.2', sha, evidence: ev, root }), gateError(expected), String(expected));
      assert.throws(() => pretag({ tag: 'v0.1.2', cwd: root, evidence: ev, create: true }), gateError(expected));
    }
    assert.equal(tags(root), '');
    // An exception with Rowan's approval, an owner, a removal release and an issue is accepted.
    const ok = evidence({ compat: [compat(sha, exception({}))], performance: [perf(sha)] });
    assert.equal(checkCompat({ tag: 'v0.1.2', sha, evidence: ok, root }).record.exceptions.length, 1);
  });

  test('a changed candidate needs a renewed review', () => {
    const { root, sha } = repo();
    const ev = evidence({ compat: [compat(sha)], performance: [perf(sha)] });
    assert.equal(pretag({ tag: 'v0.1.2', cwd: root, evidence: ev }).sha, sha);

    fs.writeFileSync(path.join(root, 'fix.txt'), 'late change');
    const changed = commit(root, 'late fix');
    // A renewed performance PASS alone is not enough: the cleanup review is stale too.
    const perfOnly = evidence({ compat: [compat(sha)], performance: [perf(changed)] });
    assert.throws(
      () => pretag({ tag: 'v0.1.2', cwd: root, evidence: perfOnly, create: true }),
      gateError(new RegExp(`COMPAT-GATE-0\\.1\\.2 \\(#229\\): stale: approved candidate ${sha.slice(0, 12)} but the release commit is ${changed.slice(0, 12)}`)),
    );
    assert.equal(tags(root), '');
    const renewed = evidence({ compat: [compat(changed)], performance: [perf(changed)] });
    assert.equal(pretag({ tag: 'v0.1.2', cwd: root, evidence: renewed }).sha, changed);
  });

  test('the record must name the layout identity the candidate writes', () => {
    const { root, sha } = repo();
    const ev = (r) => evidence({ compat: [r], performance: [perf(r.candidate.sha)] });
    assert.throws(
      () => checkCompat({ tag: 'v0.1.2', sha, evidence: ev(compat(sha, (r) => void (r.layout.identity = '0.1.1'))), root }),
      gateError(/layout\.identity is "0\.1\.1", but the candidate writes layout 0\.1\.2 \(crates\/telemetry\/src\/layout\.rs\)/),
    );
    // A layout change after the review: stale and a new identity.
    fs.writeFileSync(path.join(root, LAYOUT_RS), layoutSource('0.1.2b'));
    const changed = commit(root, 'change a format');
    assert.equal(candidateLayout(root), '0.1.2b');
    const p = compatRecordProblems(compat(sha), { gate: { id: 'COMPAT-GATE-0.1.2', issue: 229 }, version: '0.1.2', sha: changed, layout: '0.1.2b', approver: 'rowan-smith' }).join('\n');
    assert.match(p, /stale: approved candidate/);
    assert.match(p, /candidate writes layout 0\.1\.2b/);
    fs.writeFileSync(path.join(root, LAYOUT_RS), 'pub const NOT_IT: u8 = 1;\n');
    commit(root, 'no identity');
    assert.throws(() => candidateLayout(root), gateError(/declares no `pub const LAYOUT/));
  });

  test('cleanup comes before the final performance measurements, on the same candidate', () => {
    const { root, sha } = repo();
    // Reviewed after PERF-GATE-1 measured the candidate (measuredAt 2026-10-02T09:00:00Z).
    const late = evidence({ compat: [compat(sha, (r) => void (r.review.reviewedAt = '2026-10-02T10:00:00Z'))], performance: [perf(sha)] });
    assert.throws(() => pretag({ tag: 'v0.1.2', cwd: root, evidence: late, create: true }), gateError(/reviewed after PERF-GATE-1 measured the candidate; the cleanup must be complete and reviewed before the final performance measurements/));
    // Signed off a different candidate than the performance gate.
    const other = 'e'.repeat(40);
    const split = { compat: [compat(other)], performance: [perf(sha)] };
    assert.throws(
      () => checkCompat({ tag: 'v0.1.2', sha, evidence: evidence(split), root, performance: [perf(sha)] }),
      gateError(new RegExp(`PERF-GATE-1 signed off ${sha.slice(0, 12)} but this review is for eeeeeeeeeeee; both must approve the same candidate`)),
    );
    assert.equal(tags(root), '');
  });

  test('a post-tag review or a lightweight tag does not count', () => {
    const { root, sha } = repo();
    const ev = evidence({ compat: [compat(sha)] });
    assert.throws(() => checkCompat({ tag: 'v0.1.2', sha, evidence: ev, root, tagDate: '2026-09-30T00:00:00Z' }), gateError(/reviewed after v0\.1\.2 was created/));
    assert.throws(() => checkCompat({ tag: 'v0.1.2', sha, evidence: ev, root, tagDate: '' }), gateError(/not an annotated tag from the pre-tag procedure/));
  });
});

describe('AC7: every tagged release needs its gate, prereleases included', () => {
  test('a prerelease needs a compatibility PASS of its own, though no performance sign-off', () => {
    const { root, sha } = repo('0.1.2-rc.1');
    assert.throws(() => pretag({ tag: 'v0.1.2-rc.1', cwd: root, evidence: evidence(), create: true }), gateError(/no approval record at release-evidence:compat\/0\.1\.2-rc\.1\/COMPAT-GATE-0\.1\.2\.json/));
    // The final release's record does not cover the prerelease.
    assert.throws(() => checkCompat({ tag: 'v0.1.2-rc.1', sha, evidence: evidence({ compat: [compat(sha)] }), root }), gateError(/no approval record/));
    const rc = evidence({ compat: [compat(sha, (r) => void (r.version = '0.1.2-rc.1'))] });
    const r = pretag({ tag: 'v0.1.2-rc.1', cwd: root, evidence: rc, create: true });
    assert.equal(r.created, true);
    const message = git(root, 'tag', '-l', '--format=%(contents)', 'v0.1.2-rc.1');
    assert.match(message, /Performance-Gate: none \(prereleases do not need performance sign-off/);
    assert.match(message, new RegExp(`Compat-Gate: COMPAT-GATE-0\\.1\\.2 PASS ${sha} layout 0\\.1\\.2 by example-reviewer at 2026-10-01T12:00:00Z`));
  });

  test('a version without a declared gate cannot be tagged until its gate is set up', () => {
    const { root, sha } = repo('0.1.3');
    const record = compat(sha, (r) => Object.assign(r, { gate: 'COMPAT-GATE-0.1.3', issue: 400, version: '0.1.3' }));
    assert.throws(() => checkCompat({ tag: 'v0.1.3', sha, evidence: evidence({ compat: [record] }), root }), gateError(/declares no compatibility gate for 0\.1\.3; open its COMPAT-GATE-0\.1\.3 issue/));

    // The documented setup for a newly assigned release: declare its gate in a reviewed change.
    const manifest = JSON.parse(fs.readFileSync(path.join(root, COMPAT_FILE), 'utf8'));
    manifest.versions['0.1.3'] = { gate: { id: 'COMPAT-GATE-0.1.3', issue: 400, milestone: '0.1.3 — Complete everyday workflows' } };
    fs.writeFileSync(path.join(root, COMPAT_FILE), JSON.stringify(manifest, null, 2));
    const declared = commit(root, 'declare the 0.1.3 compatibility gate');
    assert.throws(() => checkCompat({ tag: 'v0.1.3', sha: declared, evidence: evidence(), root }), gateError(/COMPAT-GATE-0\.1\.3 \(#400\): no approval record at release-evidence:compat\/0\.1\.3\/COMPAT-GATE-0\.1\.3\.json/));
    const signed = compat(declared, (r) => Object.assign(r, { gate: 'COMPAT-GATE-0.1.3', issue: 400, version: '0.1.3' }));
    assert.equal(checkCompat({ tag: 'v0.1.3', sha: declared, evidence: evidence({ compat: [signed] }), root }).gate.id, 'COMPAT-GATE-0.1.3');

    manifest.versions['0.1.3'].gate.id = 'COMPAT-GATE-3';
    fs.writeFileSync(path.join(root, COMPAT_FILE), JSON.stringify(manifest, null, 2));
    commit(root, 'misnamed');
    assert.throws(() => requiredCompatGate('0.1.3', root), gateError(/0\.1\.3 gate must be COMPAT-GATE-0\.1\.3, not COMPAT-GATE-3/));
  });

  test('CLI pretag --create records both gates in the tag, and the workflow check accepts it', () => {
    const { root, sha } = repo();
    const ev = evidence({ compat: [compat(sha)], performance: [perf(sha)] }).dir;
    const r = cli(root, 'pretag', '--tag', 'v0.1.2', '--evidence', ev, '--create');
    assert.equal(r.status, 0, r.stderr);
    assert.match(r.stdout, /Compat-Gate: COMPAT-GATE-0\.1\.2 PASS/);
    const message = git(root, 'tag', '-l', '--format=%(contents)', 'v0.1.2');
    assert.match(message, /Performance-Gate: PERF-GATE-1 PASS/);
    assert.match(message, /Compat-Gate: COMPAT-GATE-0\.1\.2 PASS/);

    const tagDate = git(root, 'for-each-ref', 'refs/tags/v0.1.2', '--format=%(taggerdate:iso-strict)');
    const check = cli(root, 'check-compat', '--tag', 'v0.1.2', '--sha', sha, '--evidence', ev, '--tag-date', tagDate);
    assert.equal(check.status, 0, check.stderr);
    assert.match(check.stdout, /Compat-Gate: COMPAT-GATE-0\.1\.2 PASS/);

    // verify: the built artifacts must be the reviewed ones.
    const artifacts = tmp();
    fs.writeFileSync(path.join(artifacts, 'SHA256SUMS'), `${'b'.repeat(64)}  vyrtel-linux-x86_64\n`);
    const mismatch = cli(root, 'check-compat', '--tag', 'v0.1.2', '--sha', sha, '--evidence', ev, '--artifacts', artifacts);
    assert.equal(mismatch.status, 1);
    assert.match(mismatch.stderr, /COMPAT-GATE-0\.1\.2: vyrtel-linux-x86_64 built as bbbbbbbbbbbb but aaaaaaaaaaaa was approved/);
  });

  test('the repository manifest and template are well formed', () => {
    const manifest = JSON.parse(fs.readFileSync(path.join(ROOT, COMPAT_FILE), 'utf8'));
    assert.equal(manifest.exceptionApprover, 'rowan-smith');
    for (const [version, { gate }] of Object.entries(manifest.versions)) {
      assert.match(version, /^\d+\.\d+\.\d+$/);
      assert.equal(gate.id, `COMPAT-GATE-${version}`);
      assert.ok(Number.isInteger(gate.issue) && gate.issue > 0, `${gate.id} links an issue`);
    }
    assert.deepEqual(requiredCompatGate('0.1.2', ROOT).gate, { id: 'COMPAT-GATE-0.1.2', issue: 229, milestone: '0.1.2 — Reliability and release confidence' });
    assert.deepEqual(requiredCompatGate('0.1.2-rc.1', ROOT).gate.id, 'COMPAT-GATE-0.1.2');
    assert.match(candidateLayout(ROOT), /^[\x21-\x7e]{1,8}$/, 'the real layout identity is found');
    // The template answers every checklist item and passes for its own candidate.
    assert.deepEqual(COMPAT.checklist.map((c) => c.item), CHECKLIST);
    const gate = { id: 'COMPAT-GATE-0.1.2', issue: 229 };
    assert.deepEqual(compatRecordProblems(COMPAT, { gate, version: '0.1.2', sha: COMPAT.candidate.sha, layout: '0.1.2', approver: 'rowan-smith' }), []);
  });

  test('the gate issue template asks for every checklist item', () => {
    const template = fs.readFileSync(path.join(ROOT, '.github/ISSUE_TEMPLATE/compat-gate.yml'), 'utf8');
    for (const item of CHECKLIST) assert.match(template, new RegExp(`\\b${item}\\b`), item);
  });
});

describe('release workflow wiring', () => {
  const workflow = fs.readFileSync(path.join(ROOT, '.github/workflows/release.yml'), 'utf8').replace(/\r\n/g, '\n');

  test('the compat job runs for every tag and blocks the build', () => {
    assert.match(workflow, /\n {2}compat:\n {4}name: Pre-tag compatibility sign-off\n {4}needs: preflight\n/);
    assert.match(workflow, /check-compat --tag "\$TAG" --sha "\$GITHUB_SHA" --evidence evidence "\$\{args\[@\]\}"/);
    assert.match(workflow, /\n {2}binaries:\n(?: {4}.*\n)*? {4}needs: \[preflight, performance, compat, ci\]\n/);
    assert.match(workflow, /\n {2}verify:\n(?: {4}.*\n)*? {4}needs: \[preflight, performance, compat, binaries\]\n/);
  });

  test('verify checks the built artifacts against the reviewed hashes', () => {
    assert.match(workflow, /check-compat --tag "\$TAG" --sha "\$GITHUB_SHA" --evidence evidence --artifacts artifacts/);
  });
});
