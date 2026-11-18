// Checks of npm-audit.mjs against synthetic `npm audit --json` reports.
//
//   node --test scripts/ci/*.test.mjs

import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import { test } from 'node:test';

import { advisories, evaluate } from './npm-audit.mjs';

const GHSA = 'GHSA-fc5q-39xc-vjpf';
const via = (name, severity, id) => ({
  source: 1,
  name,
  title: `${name} problem`,
  url: `https://github.com/advisories/${id}`,
  severity,
  range: '*',
});
const report = (...vias) => ({
  auditReportVersion: 2,
  vulnerabilities: Object.fromEntries([
    ...vias.map((v) => [v.name, { name: v.name, severity: v.severity, via: [v] }]),
    // A dependent of a vulnerable package: via names it, no advisory of its own.
    ['vitest', { name: 'vitest', severity: 'high', via: ['picomatch'] }],
  ]),
});
const entry = { id: GHSA, package: 'picomatch', reason: 'no fix yet', expires: '2026-12-31' };
const today = '2026-11-18';

test('the allowlisted advisory passes; others still count', () => {
  const r = evaluate(report(via('picomatch', 'high', GHSA), via('minimatch', 'moderate', 'GHSA-g3hv-75wg-m249')), [entry], today);
  assert.deepEqual(r.problems, []);
  assert.deepEqual(r.allowed.map((e) => e.id), [GHSA]);
  assert.equal(r.advisories.length, 2);
});

test('a high or critical advisory not on the allowlist fails', () => {
  for (const severity of ['high', 'critical']) {
    const r = evaluate(report(via('picomatch', 'high', GHSA), via('other', severity, 'GHSA-aaaa-bbbb-cccc')), [entry], today);
    assert.equal(r.problems.length, 1, severity);
    assert.match(r.problems[0], /other/);
  }
  assert.equal(evaluate(report(via('picomatch', 'high', GHSA)), [], today).problems.length, 1);
});

test('an expired entry fails, from the day after its date', () => {
  const r = report(via('picomatch', 'high', GHSA));
  assert.deepEqual(evaluate(r, [entry], '2026-12-31').problems, []);
  const p = evaluate(r, [entry], '2027-01-01').problems;
  assert.ok(p.some((x) => /expired on 2026-12-31/.test(x)));
  assert.ok(p.some((x) => /picomatch problem/.test(x)), 'and the advisory is no longer allowed');
});

test('an entry that matches nothing, or the wrong package, fails', () => {
  assert.match(evaluate(report(), [entry], today).problems[0], /matches no advisory any more/);
  const wrong = evaluate(report(via('picomatch', 'high', GHSA)), [{ ...entry, package: 'fdir' }], today).problems;
  assert.ok(wrong.some((x) => /names fdir, but the advisory is for picomatch/.test(x)));
});

test('an incomplete entry fails', () => {
  for (const bad of [{ ...entry, reason: '' }, { ...entry, expires: 'soon' }, { id: GHSA }]) {
    assert.ok(evaluate(report(via('picomatch', 'high', GHSA)), [bad], today).problems.length > 0);
  }
});

test('no report is a failure, not a pass', () => {
  assert.throws(() => advisories({ error: { code: 'ENOAUDIT', summary: 'network' } }), /network/);
  assert.throws(() => advisories({}), /no vulnerabilities section/);
});

test('every committed allowlist entry is complete and expires within 90 days of being added', () => {
  const list = JSON.parse(fs.readFileSync(path.join(import.meta.dirname, 'npm-audit-allowlist.json'), 'utf8'));
  for (const e of list) {
    assert.ok(e.id && e.package && e.reason && e.added && e.expires, JSON.stringify(e));
    const days = (Date.parse(e.expires) - Date.parse(e.added)) / 86_400_000;
    assert.ok(days > 0 && days <= 90, `${e.id}: ${days} days`);
  }
});
