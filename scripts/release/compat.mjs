// Pre-tag compatibility-cleanup sign-off (docs/releasing.md#pre-tag-compatibility-sign-off).
//
// Every release, stable or prerelease, needs a COMPAT-GATE issue declared in compat-gates.json
// (keyed by its base version, so v0.1.2-rc.1 and v0.1.2 share COMPAT-GATE-0.1.2) and a reviewer's
// PASS record on the release-evidence branch at compat/<version>/<gate id>.json. The record names
// the exact candidate commit and artifact hashes, the layout identity the candidate writes, a
// completed checklist (inventory, removals, supported contract, roadmap, tests, format reset,
// docs) and every approved exception. A version can only be tagged, and a tag only published,
// when that record signs off the commit being tagged. The cleanup comes first: on a version with
// performance gates, the compatibility review must predate every performance measurement of the
// same candidate (docs/releasing.md#release-order).

import fs from 'node:fs';
import path from 'node:path';

import { readRecord } from './performance.mjs';
import { checkTagMatchesWorkspace, EXPECTED_ARTIFACTS, GateError, ROOT } from './release.mjs';

export const COMPAT_FILE = 'scripts/release/compat-gates.json';
export const LAYOUT_FILE = 'crates/telemetry/src/layout.rs';

/** Checklist items every record must answer, in the order of the gate issue template. */
export const CHECKLIST = [
  'inventory', // AC1: path/symbol, old contract, callers, removal, impact, PR; N/A areas with evidence
  'removals', // AC2: every obsolete compatibility-only branch and dependency removed, or an approved exception
  'contract', // AC3: supported version/input matrix and breaking-change notes published
  'roadmap', // AC4: upgrade/restore/migration/codec promises reconciled with the policy
  'tests', // AC5, TC1-TC3: supported workflows, removal negatives, refusal before mutation, crash/fault
  'format-reset', // FR-AC1, FR-AC2, FR-AC5: one layout per format, versions reset, old readers gone
  'layout-identity', // FR-AC3, FR-AC4: foreign, missing, truncated and mixed identities refused unchanged
  'docs', // runbook, policy, API/config/CLI examples, supported versions, backup/upgrade notes
];

const SHA1 = /^[0-9a-f]{40}$/;
const SHA256 = /^[0-9a-f]{64}$/;

const fail = (msg) => {
  throw new GateError(msg);
};

const filled = (s) => typeof s === 'string' && s.trim().length > 0;
const time = (s) => (filled(s) && !Number.isNaN(Date.parse(s)) ? Date.parse(s) : NaN);

/** The base version a release belongs to: `0.1.2` for `0.1.2-rc.1`. */
const baseVersion = (version) => version.split(/[-+]/)[0];

/** The compatibility gate a version needs, from the candidate's compat-gates.json. */
export function requiredCompatGate(version, root = ROOT) {
  const manifest = JSON.parse(fs.readFileSync(path.join(root, COMPAT_FILE), 'utf8'));
  const base = baseVersion(version);
  const gate = manifest.versions?.[base]?.gate;
  if (!gate) {
    fail(
      `${COMPAT_FILE} declares no compatibility gate for ${base}; open its COMPAT-GATE-${base} issue from the ` +
        `"Release compatibility gate" template and add it here before releasing any ${base} build`,
    );
  }
  if (gate.id !== `COMPAT-GATE-${base}`) fail(`${COMPAT_FILE}: ${base} gate must be COMPAT-GATE-${base}, not ${gate.id}`);
  if (!Number.isInteger(gate.issue) || gate.issue <= 0) fail(`${COMPAT_FILE}: ${gate.id} links no issue`);
  if (!filled(manifest.exceptionApprover)) fail(`${COMPAT_FILE} names no "exceptionApprover"`);
  return { gate, approver: manifest.exceptionApprover };
}

/** The layout identity the candidate writes (`telemetry::LAYOUT`). */
export function candidateLayout(root = ROOT) {
  const file = path.join(root, LAYOUT_FILE);
  if (!fs.existsSync(file)) fail(`${LAYOUT_FILE} is missing, so the candidate's layout identity is unknown`);
  const m = /pub const LAYOUT: &str = "([^"]+)";/.exec(fs.readFileSync(file, 'utf8'));
  if (!m) fail(`${LAYOUT_FILE} declares no \`pub const LAYOUT: &str = "…";\``);
  return m[1];
}

/**
 * Problems with a compatibility record for `gate` of `version`, as a candidate for commit
 * `sha` writing layout `layout`. An empty list means the record signs off that exact commit.
 */
export function compatRecordProblems(record, { gate, version, sha, layout, approver }) {
  const p = [];
  if (record.schema !== 1) p.push(`schema must be 1, got ${record.schema}`);
  if (record.gate !== gate.id) p.push(`record is for gate ${record.gate}, expected ${gate.id}`);
  if (record.issue !== gate.issue) p.push(`record links issue #${record.issue}, expected #${gate.issue}`);
  if (record.version !== version) p.push(`record is for version ${record.version}, expected ${version}`);

  const cand = record.candidate ?? {};
  if (!SHA1.test(cand.sha ?? '')) p.push('candidate.sha is not a full 40-character commit SHA');
  else if (cand.sha !== sha) {
    p.push(`stale: approved candidate ${cand.sha.slice(0, 12)} but the release commit is ${sha.slice(0, 12)}; the candidate changed after review, so review the change and record a new PASS`);
  }
  const artifacts = Array.isArray(cand.artifacts) ? cand.artifacts : [];
  if (!artifacts.length) p.push('candidate.artifacts lists no reviewed artifact hashes');
  for (const a of artifacts) {
    if (!EXPECTED_ARTIFACTS.includes(a?.name)) p.push(`candidate.artifacts: unknown artifact "${a?.name}"`);
    if (!SHA256.test(a?.sha256 ?? '')) p.push(`candidate.artifacts: ${a?.name} has no sha256`);
  }

  if (record.layout?.identity !== layout) {
    p.push(`layout.identity is ${JSON.stringify(record.layout?.identity ?? null)}, but the candidate writes layout ${layout} (${LAYOUT_FILE})`);
  }
  if (!filled(record.layout?.evidence)) p.push('layout has no evidence link (format versions before/after, identity tests)');
  if (!filled(record.inventory)) p.push('no inventory link');

  const items = new Map((Array.isArray(record.checklist) ? record.checklist : []).map((c) => [c?.item, c]));
  for (const item of CHECKLIST) {
    const c = items.get(item);
    if (!c) p.push(`checklist: no "${item}" item`);
    else if (c.result === 'fail') p.push(`checklist: "${item}" FAILED${filled(c.evidence) ? ` (${c.evidence})` : ''}`);
    else if (c.result !== 'pass' && c.result !== 'na') p.push(`checklist: "${item}" result must be "pass", "na" or "fail"`);
    else if (!filled(c.evidence)) p.push(`checklist: "${item}" has no evidence; "${c.result}" needs a link or a reason`);
  }
  for (const item of items.keys()) if (!CHECKLIST.includes(item)) p.push(`checklist: unknown item "${item}"`);

  if (!Array.isArray(record.exceptions)) p.push('exceptions must be a list (empty when there are none)');
  for (const [i, e] of (Array.isArray(record.exceptions) ? record.exceptions : []).entries()) {
    const at = `exceptions[${i}]${filled(e?.what) ? ` (${e.what})` : ''}`;
    if (!filled(e?.what)) p.push(`${at}: names nothing`);
    if (e?.approvedBy !== approver) p.push(`${at}: approved by ${JSON.stringify(e?.approvedBy ?? null)}, but only ${approver} may approve an exception`);
    if (!filled(e?.owner)) p.push(`${at}: no owner`);
    if (!filled(e?.removalRelease)) p.push(`${at}: no removal release`);
    if (!Number.isInteger(e?.issue) || e.issue <= 0) p.push(`${at}: no linked issue`);
  }

  const review = record.review ?? {};
  const reviewed = time(review.reviewedAt);
  if (review.verdict !== 'PASS') p.push(`review verdict is ${JSON.stringify(review.verdict ?? null)}, not PASS`);
  if (!filled(review.reviewer)) p.push('review has no reviewer');
  if (Number.isNaN(reviewed)) p.push('review.reviewedAt is missing or not a date');
  else if (reviewed > Date.now() + 5 * 60_000) p.push('review.reviewedAt is in the future');
  return p;
}

/**
 * Checks the tag's version has a valid compatibility PASS for `sha`. With `tagDate` (the
 * annotated tag's creation time) the review must predate the tag. With `performance` (the
 * accepted performance records of the same version), it must predate every measurement and
 * name the same candidate: cleanup before the final performance sign-off.
 */
export function checkCompat({ tag, sha, evidence, root = ROOT, tagDate, performance = [] }) {
  const t = checkTagMatchesWorkspace(tag, root);
  const { gate, approver } = requiredCompatGate(t.version, root);
  const layout = candidateLayout(root);
  const record = readRecord(evidence, t.version, gate.id, 'compat');
  const at = `${gate.id} (#${gate.issue})`;
  if (!record) fail(`compatibility sign-off for ${tag} is incomplete:\n  - ${at}: no approval record at release-evidence:compat/${t.version}/${gate.id}.json`);

  const p = compatRecordProblems(record, { gate, version: t.version, sha, layout, approver });
  const reviewed = time(record.review?.reviewedAt);
  if (tagDate !== undefined && !Number.isNaN(reviewed)) {
    if (Number.isNaN(time(tagDate))) p.push(`tag ${tag} is not an annotated tag from the pre-tag procedure, so its sign-off cannot be checked`);
    else if (reviewed > time(tagDate)) p.push(`reviewed after ${tag} was created; sign-off must happen before tagging`);
  }
  for (const r of performance) {
    if (r.candidate?.sha !== record.candidate?.sha) {
      p.push(`${r.gate} signed off ${r.candidate?.sha?.slice(0, 12)} but this review is for ${record.candidate?.sha?.slice(0, 12)}; both must approve the same candidate`);
    } else if (!Number.isNaN(reviewed) && reviewed > time(r.measuredAt)) {
      p.push(`reviewed after ${r.gate} measured the candidate; the cleanup must be complete and reviewed before the final performance measurements, so re-measure after this review`);
    }
  }
  if (p.length) fail(`compatibility sign-off for ${tag} is incomplete:\n  - ${p.map((x) => `${at}: ${x}`).join('\n  - ')}`);
  return { version: t.version, gate, record, layout };
}

/** The tag-message line for an accepted compatibility record. */
export function compatTagLine(result) {
  const r = result.record;
  return `Compat-Gate: ${r.gate} PASS ${r.candidate.sha} layout ${r.layout.identity} by ${r.review.reviewer} at ${r.review.reviewedAt}`;
}
