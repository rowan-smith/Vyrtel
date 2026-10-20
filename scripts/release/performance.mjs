// Pre-tag performance sign-off (docs/releasing.md#pre-tag-performance-sign-off).
//
// Each release version declares its performance gates in performance-gates.json. A gate is
// signed off by an approval record on the release-evidence branch at
// performance/<version>/<gate id>.json. The record names the exact candidate commit and the
// artifact hashes that were profiled, and carries the measured metric matrix and a reviewer's
// PASS. A version can only be tagged, and a tag only published, when every gate for it has a
// valid PASS record for the commit being tagged.

import { execFileSync } from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';

import { checkCompat, compatTagLine } from './compat.mjs';
import { checkTagMatchesWorkspace, EXPECTED_ARTIFACTS, GateError, parseChecksums, ROOT } from './release.mjs';

export const GATES_FILE = 'scripts/release/performance-gates.json';
export const EVIDENCE_BRANCH = 'release-evidence';

/** Metric categories every record must cover, with at least one measured row or an N/A reason. */
export const METRIC_CATEGORIES = [
  'footprint', // RSS, heap, growth slope, threads, FDs
  'cpu', // user/system time, utilisation, CPU per accepted event
  'ingestion', // accepted events/s, MiB/s, ACK latency percentiles
  'queries', // cold/warm latency percentiles, throughput, scanned rows/bytes
  'correctness', // acknowledged vs stored, results vs seeded truth, unexpected errors/5xx
  'backpressure', // admission/drain, queue age, permit recovery, time back to idle
  'storage', // disk/WAL/index size, bytes per event, fsync latency, compaction
  'networking', // bytes in/out, connections, transport errors
  'ui', // time-to-usable, interaction latency, browser heap
  'lifecycle', // startup/readiness, recovery, shutdown, binary/image size
];

const SHA1 = /^[0-9a-f]{40}$/;
const SHA256 = /^[0-9a-f]{64}$/;

const fail = (msg) => {
  throw new GateError(msg);
};

const filled = (s) => typeof s === 'string' && s.trim().length > 0;
const isNum = (n) => typeof n === 'number' && Number.isFinite(n);
const time = (s) => (filled(s) && !Number.isNaN(Date.parse(s)) ? Date.parse(s) : NaN);

/** The performance gates a version needs, from the candidate's performance-gates.json. */
export function requiredGates(version, prerelease, root = ROOT) {
  const manifest = JSON.parse(fs.readFileSync(path.join(root, GATES_FILE), 'utf8'));
  if (prerelease && manifest.prereleases === 'not-required') {
    return { gates: [], reason: 'prereleases do not need performance sign-off (performance-gates.json "prereleases")' };
  }
  const base = version.split(/[-+]/)[0];
  const entry = manifest.versions?.[base];
  if (!entry) {
    fail(`${GATES_FILE} declares no performance gates for ${base}; add its gates (or an empty list with a reason) before releasing it`);
  }
  if (!Array.isArray(entry.gates)) fail(`${GATES_FILE}: ${base} has no "gates" list`);
  if (entry.gates.length === 0 && !filled(entry.reason)) fail(`${GATES_FILE}: ${base} has no gates and no "reason"`);
  return { gates: entry.gates, reason: entry.reason };
}

/**
 * Reads <kind>/<version>/<gate>.json (kind `performance` or `compat`) from a directory or a git
 * ref; null when absent.
 */
export function readRecord(evidence, version, gate, kind = 'performance') {
  const rel = `${kind}/${version}/${gate}.json`;
  let text;
  if (evidence.dir) {
    const file = path.join(evidence.dir, rel);
    if (!fs.existsSync(file)) return null;
    text = fs.readFileSync(file, 'utf8');
  } else {
    try {
      text = execFileSync('git', ['show', `${evidence.ref}:${rel}`], { cwd: evidence.cwd, encoding: 'utf8', stdio: 'pipe' });
    } catch {
      return null;
    }
  }
  try {
    return JSON.parse(text);
  } catch (e) {
    fail(`${rel} is not valid JSON: ${e.message}`);
  }
}

/** Problems with one metric row; the recorded pass/fail must agree with value vs limit. */
function metricProblems(m, i) {
  const at = `metrics[${i}]${filled(m?.name) ? ` (${m.category}: ${m.name})` : ''}`;
  if (!METRIC_CATEGORIES.includes(m?.category)) return [`${at}: unknown category "${m?.category}"`];
  if ('na' in m) return filled(m.na) ? [] : [`${at}: N/A needs a reason`];
  const p = [];
  if (!filled(m.name)) p.push(`${at}: no name`);
  if (!filled(m.unit)) p.push(`${at}: no unit`);
  if (!isNum(m.value)) p.push(`${at}: value is not a number`);
  const lim = m.limit ?? {};
  if (!isNum(lim.max) && !isNum(lim.min)) p.push(`${at}: no numeric limit (limit.max or limit.min)`);
  if (isNum(m.baseline)) {
    if (!isNum(m.delta)) p.push(`${at}: baseline without delta`);
    else if (isNum(m.value) && Math.abs(m.value - m.baseline - m.delta) > 1e-6 * Math.max(1, Math.abs(m.value))) {
      p.push(`${at}: delta ${m.delta} is not value − baseline (${m.value - m.baseline})`);
    }
  } else if (!filled(m.noBaseline)) {
    p.push(`${at}: no baseline; give one or a "noBaseline" reason with an absolute limit`);
  }
  if (m.result !== 'pass' && m.result !== 'fail') p.push(`${at}: result must be "pass" or "fail"`);
  if (!p.length) {
    const within = (!isNum(lim.max) || m.value <= lim.max) && (!isNum(lim.min) || m.value >= lim.min);
    if (!within && m.result === 'pass') p.push(`${at}: marked pass but ${m.value} ${m.unit} is outside its limit`);
    else if (!within || m.result === 'fail') p.push(`${at}: FAILED (${m.value} ${m.unit}, limit ${JSON.stringify(lim)})`);
  }
  return p;
}

/**
 * Problems with an approval record for `gate` of `version`, as a candidate for commit `sha`.
 * An empty list means the record signs off that exact commit.
 */
export function recordProblems(record, { gate, version, sha }) {
  const p = [];
  if (record.schema !== 1) p.push(`schema must be 1, got ${record.schema}`);
  if (record.gate !== gate.id) p.push(`record is for gate ${record.gate}, expected ${gate.id}`);
  if (record.version !== version) p.push(`record is for version ${record.version}, expected ${version}`);

  const cand = record.candidate ?? {};
  if (!SHA1.test(cand.sha ?? '')) p.push('candidate.sha is not a full 40-character commit SHA');
  else if (cand.sha !== sha) {
    p.push(`stale: approved candidate ${cand.sha.slice(0, 12)} but the release commit is ${sha.slice(0, 12)}; the candidate changed after sign-off, so rerun the affected workloads and record a new PASS`);
  }
  const artifacts = Array.isArray(cand.artifacts) ? cand.artifacts : [];
  if (!artifacts.length) p.push('candidate.artifacts lists no profiled artifact hashes');
  for (const a of artifacts) {
    if (!EXPECTED_ARTIFACTS.includes(a?.name)) p.push(`candidate.artifacts: unknown artifact "${a?.name}"`);
    if (!SHA256.test(a?.sha256 ?? '')) p.push(`candidate.artifacts: ${a?.name} has no sha256`);
  }

  const base = record.baseline ?? {};
  if (!filled(base.release) || !SHA1.test(base.sha ?? '')) p.push('baseline needs the prior approved release and its full commit SHA');

  const declared = time(record.budgetsDeclaredAt);
  const measured = time(record.measuredAt);
  const reviewed = time(record.review?.reviewedAt);
  if (Number.isNaN(declared)) p.push('budgetsDeclaredAt is missing or not a date');
  if (Number.isNaN(measured)) p.push('measuredAt is missing or not a date');
  if (!Number.isNaN(declared) && !Number.isNaN(measured) && declared > measured) {
    p.push('budgets were declared after the candidate was measured; they must be fixed first');
  }

  const review = record.review ?? {};
  if (review.verdict !== 'PASS') p.push(`review verdict is ${JSON.stringify(review.verdict ?? null)}, not PASS`);
  if (!filled(review.reviewer)) p.push('review has no reviewer');
  if (Number.isNaN(reviewed)) p.push('review.reviewedAt is missing or not a date');
  else if (!Number.isNaN(measured) && reviewed < measured) p.push('review predates the measurements');
  else if (reviewed > Date.now() + 5 * 60_000) p.push('review.reviewedAt is in the future');

  if (!Array.isArray(record.reports) || !record.reports.some(filled)) p.push('no profiling report links');

  const metrics = Array.isArray(record.metrics) ? record.metrics : [];
  metrics.forEach((m, i) => p.push(...metricProblems(m, i)));
  for (const c of METRIC_CATEGORIES) {
    if (!metrics.some((m) => m?.category === c)) p.push(`metrics: no "${c}" rows (measure it or add an N/A row with a reason)`);
  }
  return p;
}

/**
 * Checks every gate for the tag's version has a valid PASS record for `sha`. With `tagDate`
 * (the annotated tag's creation time), each review must predate the tag: a post-tag sign-off
 * does not count. Returns the gates and records it accepted.
 */
export function checkPerformance({ tag, sha, evidence, root = ROOT, tagDate }) {
  const t = checkTagMatchesWorkspace(tag, root);
  const { gates, reason } = requiredGates(t.version, t.prerelease, root);
  if (!gates.length) return { version: t.version, gates: [], reason, records: [] };

  const problems = [];
  const records = [];
  for (const gate of gates) {
    const record = readRecord(evidence, t.version, gate.id);
    if (!record) {
      problems.push(`${gate.id} (#${gate.issue}): no approval record at ${EVIDENCE_BRANCH}:performance/${t.version}/${gate.id}.json`);
      continue;
    }
    const p = recordProblems(record, { gate, version: t.version, sha });
    if (tagDate !== undefined && !Number.isNaN(time(record.review?.reviewedAt))) {
      if (Number.isNaN(time(tagDate))) p.push(`tag ${tag} is not an annotated tag from the pre-tag procedure, so its sign-off cannot be checked`);
      else if (time(record.review.reviewedAt) > time(tagDate)) p.push(`reviewed after ${tag} was created; sign-off must happen before tagging`);
    }
    problems.push(...p.map((x) => `${gate.id} (#${gate.issue}): ${x}`));
    records.push(record);
  }
  const shas = new Set(records.map((r) => r.candidate?.sha).filter(Boolean));
  if (shas.size > 1) {
    problems.push(`gates for ${t.version} signed off different candidates (${[...shas].map((s) => s.slice(0, 12)).join(', ')}); all must approve the same commit`);
  }
  if (problems.length) fail(`performance sign-off for ${tag} is incomplete:\n  - ${problems.join('\n  - ')}`);
  return { version: t.version, gates, records };
}

/**
 * Checks the artifacts the release built match the hashes each gate profiled, or that the
 * record documents how they are reproducibly equivalent (the binaries also passed the
 * release smoke checks by this point). Returns one line per artifact.
 */
export function checkArtifactsMatchApproval(records, artifactsDir) {
  const built = parseChecksums(fs.readFileSync(path.join(artifactsDir, 'SHA256SUMS'), 'utf8'));
  const problems = [];
  const lines = [];
  for (const r of records) {
    const eq = r.equivalence;
    const hasEq = filled(eq?.method) && filled(eq?.evidence);
    for (const a of r.candidate.artifacts) {
      const got = built.get(a.name);
      if (got === a.sha256) lines.push(`${r.gate}: ${a.name} matches the approved hash`);
      else if (hasEq) lines.push(`${r.gate}: ${a.name} differs from the approved hash; equivalent by ${eq.method} (${eq.evidence})`);
      else {
        problems.push(
          `${r.gate}: ${a.name} built as ${got?.slice(0, 12) ?? '(missing)'} but ${a.sha256.slice(0, 12)} was approved, and the record documents no reproducible equivalence`,
        );
      }
    }
  }
  if (problems.length) fail(`released artifacts do not match the approved candidate:\n  - ${problems.join('\n  - ')}`);
  return lines;
}

const git = (args, cwd) => execFileSync('git', args, { cwd, encoding: 'utf8', stdio: 'pipe' }).trim();

/** The tag message the pre-tag procedure writes; the release workflow looks for these lines. */
export function tagMessage(version, result) {
  const lines = result.gates.length
    ? result.records.map((r) => `Performance-Gate: ${r.gate} PASS ${r.candidate.sha} by ${r.review.reviewer} at ${r.review.reviewedAt}`)
    : [`Performance-Gate: none (${result.reason})`];
  if (result.compat) lines.push(compatTagLine(result.compat));
  return `Vyrtel ${version}\n\n${lines.join('\n')}\n`;
}

/**
 * The supported way to create a release tag: checks the working tree is clean, the version
 * matches, every performance gate has signed off HEAD and the compatibility gate has signed off
 * HEAD before those measurements (compat.mjs), then creates an annotated tag. Nothing is tagged
 * if any check fails. Pushing the tag is left to the maintainer.
 */
export function pretag({ tag, cwd = ROOT, evidence, create = false }) {
  const dirty = git(['status', '--porcelain', '--untracked-files=no'], cwd);
  if (dirty) fail(`the working tree has uncommitted changes; commit or stash them so the tag is the commit that was profiled:\n${dirty}`);
  const sha = git(['rev-parse', 'HEAD'], cwd);
  if (!evidence) {
    try {
      git(['fetch', '--quiet', 'origin', `+refs/heads/${EVIDENCE_BRANCH}:refs/remotes/origin/${EVIDENCE_BRANCH}`], cwd);
    } catch (e) {
      fail(`could not fetch the ${EVIDENCE_BRANCH} branch, so no sign-off can be checked (has one been recorded yet?): ${e.stderr?.toString().trim() || e.message}`);
    }
    evidence = { ref: `origin/${EVIDENCE_BRANCH}`, cwd };
  }
  const performance = checkPerformance({ tag, sha, evidence, root: cwd });
  const result = { ...performance, compat: checkCompat({ tag, sha, evidence, root: cwd, performance: performance.records }) };
  let exists = true;
  try {
    git(['rev-parse', '--verify', '--quiet', `refs/tags/${tag}`], cwd);
  } catch {
    exists = false;
  }
  if (exists) fail(`tag ${tag} already exists; a released version is never re-tagged, publish a new version instead`);
  if (create) {
    try {
      git(['tag', '-a', tag, '-m', tagMessage(result.version, result)], cwd);
    } catch (e) {
      fail(`git tag failed: ${e.stderr?.toString().trim() || e.message}`);
    }
  }
  return { ...result, sha, created: create };
}
