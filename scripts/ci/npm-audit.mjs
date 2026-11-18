#!/usr/bin/env node
// `npm audit --audit-level=high` with an allowlist of specific advisories that have no fix yet
// (scripts/ci/npm-audit-allowlist.json). It fails when:
//
// * any advisory of high or critical severity is not on the allowlist;
// * an allowlist entry has expired (each one has an `expires` date, so an exception cannot
//   linger: renew it on purpose, with a reason, or remove it);
// * an allowlist entry matches nothing any more (the fix arrived: remove the entry), or names a
//   different package than the advisory it allows;
// * npm audit did not produce a report (no network, a malformed lockfile).
//
//   node scripts/ci/npm-audit.mjs DIR [--allowlist FILE] [--report FILE] [--today YYYY-MM-DD]
//
// DIR is where package-lock.json is. --report reads `npm audit --json` output from a file instead
// of running npm (for tests); --today fixes the date expiry is checked against.

import { execFileSync } from 'node:child_process';
import { readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

const GATED = new Set(['high', 'critical']);
const DATE = /^\d{4}-\d{2}-\d{2}$/;

/** Every advisory in an `npm audit --json` (v2) report: {id, package, severity, title, url}. */
export function advisories(report) {
  if (!report || typeof report.vulnerabilities !== 'object' || report.error) {
    const why = report?.error?.summary ?? report?.error?.code ?? 'no vulnerabilities section';
    throw new Error(`npm audit did not produce a report: ${why}`);
  }
  const found = new Map();
  for (const v of Object.values(report.vulnerabilities)) {
    for (const via of v.via ?? []) {
      if (typeof via !== 'object') continue; // a package that depends on a vulnerable one
      const id = /GHSA-[a-z0-9]{4}-[a-z0-9]{4}-[a-z0-9]{4}/.exec(via.url ?? '')?.[0] ?? `npm-${via.source}`;
      found.set(id, { id, package: via.name, severity: via.severity, title: via.title, url: via.url });
    }
  }
  return [...found.values()];
}

/** The problems that fail the check (empty when it passes) and the advisories allowed. */
export function evaluate(report, allowlist, today) {
  const problems = [];
  const all = advisories(report);
  const byId = new Map(all.map((a) => [a.id, a]));
  const allowed = new Map();
  for (const e of allowlist) {
    if (!e?.id || !e.package || !e.reason || !DATE.test(e.expires ?? '')) {
      problems.push(`allowlist entry ${JSON.stringify(e)}: needs id, package, reason and expires (YYYY-MM-DD)`);
      continue;
    }
    const a = byId.get(e.id);
    if (e.expires < today) problems.push(`allowlist entry ${e.id} expired on ${e.expires}: renew it with a reason, or remove it`);
    else if (!a) problems.push(`allowlist entry ${e.id} (${e.package}) matches no advisory any more: remove it`);
    else if (a.package !== e.package) problems.push(`allowlist entry ${e.id} names ${e.package}, but the advisory is for ${a.package}`);
    else allowed.set(e.id, e);
  }
  for (const a of all) {
    if (GATED.has(a.severity) && !allowed.has(a.id)) {
      problems.push(`${a.severity}: ${a.package}: ${a.title} (${a.url ?? a.id})`);
    }
  }
  return { problems, allowed: [...allowed.values()], advisories: all };
}

if (import.meta.url === pathToFileURL(process.argv[1]).href) {
  const args = process.argv.slice(2);
  const opt = (name) => {
    const i = args.indexOf(name);
    return i >= 0 ? args[i + 1] : undefined;
  };
  const dir = args[0];
  if (!dir || dir.startsWith('--')) {
    console.error('usage: npm-audit.mjs DIR [--allowlist FILE] [--report FILE] [--today YYYY-MM-DD]');
    process.exit(2);
  }
  const here = dirname(fileURLToPath(import.meta.url));
  const allowlist = JSON.parse(readFileSync(opt('--allowlist') ?? join(here, 'npm-audit-allowlist.json'), 'utf8'));
  let text = opt('--report') && readFileSync(opt('--report'), 'utf8');
  if (!text) {
    try {
      text = execFileSync('npm', ['audit', '--json'], { cwd: dir, encoding: 'utf8', shell: process.platform === 'win32' });
    } catch (e) {
      text = e.stdout; // npm audit exits 1 when it finds anything; the report is still on stdout
    }
  }
  const today = opt('--today') ?? new Date().toISOString().slice(0, 10);
  let result;
  try {
    result = evaluate(JSON.parse(text), allowlist, today);
  } catch (e) {
    console.error(`npm-audit: ${e.message}`);
    process.exit(1);
  }
  for (const a of result.advisories) console.log(`${a.severity.padEnd(8)} ${a.package}: ${a.title} (${a.id})`);
  for (const e of result.allowed) console.log(`allowed  ${e.id} (${e.package}) until ${e.expires}: ${e.reason}`);
  if (result.problems.length) {
    for (const p of result.problems) console.error(`FAIL     ${p}`);
    process.exit(1);
  }
  console.log('npm audit: no high or critical advisories outside the allowlist');
}
