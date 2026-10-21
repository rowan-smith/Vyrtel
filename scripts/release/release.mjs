#!/usr/bin/env node
// Release gate used by .github/workflows/release.yml (see docs/releasing.md).
//
//   node scripts/release/release.mjs plan --tag v0.1.2 [--image ghcr.io/owner/repo]
//   node scripts/release/release.mjs verify-artifacts <dir> [--version 0.1.2]
//   node scripts/release/release.mjs verify-binary <path> [--version 0.1.2]
//   node scripts/release/release.mjs verify-server <url> [--version 0.1.2] [--password <admin password>]
//   node scripts/release/release.mjs verify-image <image> [--version 0.1.2] [--platform linux/arm64]
//                                    [--bind-mount auto|on|off] [--record <dir>]
//   node scripts/release/release.mjs verify-pushed-image <ref> [--version 0.1.2] [--tested <dir>]
//   node scripts/release/release.mjs pretag --tag v0.1.2 [--evidence <dir>] [--create]
//   node scripts/release/release.mjs check-performance --tag v0.1.2 --sha <commit> --evidence <dir>
//                                    [--tag-date <iso>] [--artifacts <dir>]
//   node scripts/release/release.mjs check-compat --tag v0.1.2 --sha <commit> --evidence <dir>
//                                    [--tag-date <iso>] [--artifacts <dir>]
//
// Every command exits non-zero with a "release gate:" message when a check fails, so the
// workflow stops before anything is published. --version defaults to the workspace version.
// No dependencies beyond Node 22.

import { spawn } from 'node:child_process';
import { createHash } from 'node:crypto';
import fs from 'node:fs';
import net from 'node:net';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

export const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../..');

/** Files every release ships, besides SHA256SUMS. Must match the `binaries` matrix in release.yml. */
export const EXPECTED_ARTIFACTS = [
  'vyrtel-linux-x86_64',
  'vyrtel-linux-arm64',
  'vyrtel-macos-arm64',
  'vyrtel-windows-x86_64.exe',
];

/** Text of the page build.rs embeds when the web UI was not built. */
export const UI_PLACEHOLDER_MARKER = 'The web UI was not built into this binary';

export class GateError extends Error {}

const fail = (msg) => {
  throw new GateError(msg);
};

// ---- version and channel ----------------------------------------------------

const NUM = '(0|[1-9]\\d*)';
const TAG = new RegExp(`^v(${NUM}\\.${NUM}\\.${NUM}(?:-([0-9A-Za-z-]+(?:\\.[0-9A-Za-z-]+)*))?(?:\\+[0-9A-Za-z.-]+)?)$`);

/** Parses a release tag (`v` + semver). A tag with a `-suffix` is a prerelease. */
export function parseTag(tag) {
  const m = TAG.exec(tag ?? '');
  if (!m) fail(`tag "${tag}" is not a release tag; expected v<major>.<minor>.<patch>[-<prerelease>], e.g. v0.1.2 or v0.1.2-rc.1`);
  const [, version, major, minor, patch, pre] = m;
  return { tag, version, major: +major, minor: +minor, patch: +patch, prerelease: pre !== undefined };
}

/** The `[workspace.package]` version in Cargo.toml: what `vyrtel --version` and the API report. */
export function workspaceVersion(root = ROOT) {
  const toml = fs.readFileSync(path.join(root, 'Cargo.toml'), 'utf8');
  const section = /^\[workspace\.package\]\s*$([\s\S]*?)(?=^\[|(?![\s\S]))/m.exec(toml);
  const version = section && /^version\s*=\s*"([^"]+)"/m.exec(section[1]);
  if (!version) fail('Cargo.toml has no [workspace.package] version');
  return version[1];
}

/** Every place the release version is recorded, so a tag cannot ship with a stale one. */
export function recordedVersions(root = ROOT) {
  const found = { 'Cargo.toml [workspace.package]': workspaceVersion(root) };
  const lock = fs.readFileSync(path.join(root, 'Cargo.lock'), 'utf8');
  const server = /\[\[package\]\]\r?\nname = "server"\r?\nversion = "([^"]+)"/.exec(lock);
  found['Cargo.lock (server)'] = server ? server[1] : '(missing)';
  found['web/package.json'] = JSON.parse(fs.readFileSync(path.join(root, 'web/package.json'), 'utf8')).version;
  return found;
}

/** Fails unless the tag's version equals every recorded workspace version. */
export function checkTagMatchesWorkspace(tag, root = ROOT) {
  const parsed = parseTag(tag);
  const recorded = recordedVersions(root);
  const wrong = Object.entries(recorded).filter(([, v]) => v !== parsed.version);
  if (wrong.length) {
    const list = wrong.map(([where, v]) => `${where} = ${v}`).join(', ');
    fail(`tag ${tag} is version ${parsed.version} but ${list}; bump the workspace version (and run cargo update -w) before tagging`);
  }
  return parsed;
}

/**
 * What a tag publishes. Stable releases become the GitHub "latest" release and move the
 * `latest` and `<major>.<minor>` image tags; prereleases only get their exact version tag.
 */
export function releasePlan(tag, { image = 'ghcr.io/rowan-smith/vyrtel', root = ROOT } = {}) {
  const t = checkTagMatchesWorkspace(tag, root);
  // Registry references must be lowercase (github.repository keeps the repo's capital V).
  const repo = image.toLowerCase();
  const tags = t.prerelease ? [t.version] : [t.version, `${t.major}.${t.minor}`, 'latest'];
  return {
    tag: t.tag,
    version: t.version,
    prerelease: t.prerelease,
    makeLatest: !t.prerelease,
    imageTags: tags.map((x) => `${repo}:${x}`),
    // The exact-version tag, checked in the registry after the push.
    imageRef: `${repo}:${t.version}`,
    // Where each platform's tested image is pushed by digest before the tags are assembled.
    imageRepo: repo,
  };
}

// ---- artifacts ----------------------------------------------------------------

const sha256 = (file) => createHash('sha256').update(fs.readFileSync(file)).digest('hex');

/** Parses `sha256sum` output (`<hex>  <name>`, optional `*` binary marker and `./`). */
export function parseChecksums(text) {
  const entries = new Map();
  for (const [i, raw] of text.split(/\r?\n/).entries()) {
    const line = raw.trim();
    if (!line) continue;
    const m = /^([0-9a-fA-F]{64}) [ *](?:\.\/)?(.+)$/.exec(line);
    if (!m) fail(`SHA256SUMS line ${i + 1} is malformed: "${line}"`);
    if (entries.has(m[2])) fail(`SHA256SUMS lists ${m[2]} twice`);
    entries.set(m[2], m[1].toLowerCase());
  }
  return entries;
}

/**
 * Checks a release directory holds exactly the expected binaries plus SHA256SUMS, and that
 * every checksum matches. Returns the verified file names.
 */
export function verifyArtifacts(dir, expected = EXPECTED_ARTIFACTS) {
  const sumsPath = path.join(dir, 'SHA256SUMS');
  if (!fs.existsSync(sumsPath)) fail(`${dir} has no SHA256SUMS`);
  const sums = parseChecksums(fs.readFileSync(sumsPath, 'utf8'));
  const present = fs.readdirSync(dir).filter((f) => f !== 'SHA256SUMS');
  const problems = [];
  for (const name of expected) {
    if (!present.includes(name)) problems.push(`missing artifact ${name}`);
    if (!sums.has(name)) problems.push(`SHA256SUMS has no entry for ${name}`);
  }
  for (const name of present) if (!expected.includes(name)) problems.push(`unexpected file ${name}`);
  for (const name of sums.keys()) if (!expected.includes(name)) problems.push(`SHA256SUMS lists unexpected ${name}`);
  for (const name of expected) {
    const file = path.join(dir, name);
    if (!present.includes(name) || !sums.has(name)) continue;
    if (fs.statSync(file).size === 0) problems.push(`${name} is empty`);
    else if (sha256(file) !== sums.get(name)) problems.push(`checksum mismatch for ${name}`);
  }
  if (problems.length) fail(`release artifacts in ${dir} failed validation:\n  - ${problems.join('\n  - ')}`);
  return expected;
}

// ---- running server -------------------------------------------------------------

/**
 * The admin password the checks start servers with (`CHECK_ENV`) and sign in with. Ingestion
 * is left open so the image check can ingest without first creating a key.
 */
export const CHECK_PASSWORD = 'release-check';
export const CHECK_ENV = { VYRTEL_AUTH_ADMIN_PASSWORD: CHECK_PASSWORD, VYRTEL_AUTH_INGEST_API_KEY: 'false' };

async function get(url, headers = {}) {
  const res = await fetch(url, { headers, signal: AbortSignal.timeout(10_000) });
  return { status: res.status, type: res.headers.get('content-type') ?? '', body: await res.text() };
}

/** Signs in as admin; returns the session cookie (`name=value`). */
export async function login(base, password = CHECK_PASSWORD) {
  const res = await fetch(`${base}/api/v1/auth/login`, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ username: 'admin', password }),
    signal: AbortSignal.timeout(10_000),
  });
  if (!res.ok) fail(`admin login answered HTTP ${res.status}: ${await res.text()}`);
  const cookie = res.headers.get('set-cookie')?.split(';')[0];
  if (!cookie) fail('admin login set no session cookie');
  return cookie;
}

/** Checks the HTML served at `/` is the built UI, not the placeholder build.rs embeds. Returns the entry script. */
export function checkIndexHtml(html) {
  if (html.includes(UI_PLACEHOLDER_MARKER)) fail('the web UI is not embedded: / serves the placeholder page');
  if (!html.includes('<div id="root">')) fail('/ does not serve the Vyrtel web UI (no #root element)');
  const script = /<script[^>]*type="module"[^>]*src="(\/assets\/[^"]+\.js)"/.exec(html);
  if (!script) fail('/ does not reference a built /assets/*.js entry script (was the UI built with vite build?)');
  return script[1];
}

async function waitForHealth(base, timeoutMs, exited) {
  const deadline = Date.now() + timeoutMs;
  let last = 'no response';
  while (Date.now() < deadline) {
    if (exited?.()) fail(`server exited before it became healthy: ${exited()}`);
    try {
      const r = await get(`${base}/health`);
      if (r.status === 200) return;
      last = `HTTP ${r.status}`;
    } catch (e) {
      last = e.cause?.code ?? e.message;
    }
    await new Promise((r) => setTimeout(r, 250));
  }
  fail(`${base}/health did not answer 200 within ${timeoutMs / 1000}s (last: ${last})`);
}

/**
 * Checks a running server reports `version` and serves the embedded UI and its assets. Signs
 * in with `password`; returns the session cookie with the results.
 */
export async function verifyServer(base, version, { timeoutMs = 30_000, exited, password = CHECK_PASSWORD } = {}) {
  base = base.replace(/\/+$/, '');
  await waitForHealth(base, timeoutMs, exited);

  const cookie = await login(base, password);
  const info = await get(`${base}/api/v1/system/info`, { cookie });
  if (info.status !== 200) fail(`/api/v1/system/info answered HTTP ${info.status}`);
  const reported = JSON.parse(info.body).version;
  if (reported !== version) fail(`server reports version ${reported}, expected ${version}`);

  const index = await get(`${base}/`);
  if (index.status !== 200 || !index.type.startsWith('text/html')) fail(`/ answered HTTP ${index.status} (${index.type})`);
  const entry = checkIndexHtml(index.body);
  const asset = await get(`${base}${entry}`);
  if (asset.status !== 200 || !/javascript/.test(asset.type) || asset.body.length === 0) {
    fail(`embedded UI asset ${entry} answered HTTP ${asset.status} (${asset.type}, ${asset.body.length} bytes)`);
  }
  return { version: reported, entry, cookie };
}

function freePort() {
  return new Promise((resolve, reject) => {
    const srv = net.createServer();
    srv.once('error', reject);
    srv.listen(0, '127.0.0.1', () => {
      const { port } = srv.address();
      srv.close(() => resolve(port));
    });
  });
}

export function run(command, args, opts) {
  return new Promise((resolve, reject) => {
    const child = spawn(command, args, { ...opts, stdio: ['ignore', 'pipe', 'pipe'] });
    let stdout = '';
    let stderr = '';
    child.stdout.on('data', (d) => (stdout += d));
    child.stderr.on('data', (d) => (stderr += d));
    child.once('error', reject);
    child.once('close', (code) => resolve({ code, stdout, stderr }));
  });
}

/** The environment without VYRTEL_* settings, so the check sees the binary's defaults. */
function cleanEnv() {
  return Object.fromEntries(Object.entries(process.env).filter(([k]) => !k.toUpperCase().startsWith('VYRTEL_')));
}

/**
 * Checks a built binary: `--version` prints `vyrtel <version>`, and a fresh server on a free
 * loopback port with an empty data directory reports that version and serves the embedded UI.
 * `prefixArgs` lets tests run a script through node.
 */
export async function verifyBinary(binary, version, { prefixArgs = [], timeoutMs = 30_000 } = {}) {
  const work = fs.mkdtempSync(path.join(os.tmpdir(), 'vyrtel-release-check-'));
  const opts = { cwd: work, env: { ...cleanEnv(), ...CHECK_ENV } };
  try {
    const out = await run(binary, [...prefixArgs, '--version'], opts);
    const printed = out.stdout.trim();
    if (out.code !== 0) fail(`${binary} --version exited ${out.code}: ${out.stderr.trim()}`);
    if (printed !== `vyrtel ${version}`) fail(`${binary} --version printed "${printed}", expected "vyrtel ${version}"`);

    const port = await freePort();
    const args = [...prefixArgs, 'serve', '--data-dir', path.join(work, 'data'), '--bind', `127.0.0.1:${port}`];
    const child = spawn(binary, args, { ...opts, stdio: ['ignore', 'ignore', 'pipe'] });
    let stderr = '';
    let exit = null;
    child.stderr.on('data', (d) => (stderr = (stderr + d).slice(-4000)));
    child.once('exit', (code, signal) => (exit = `exit ${code ?? signal}; stderr: ${stderr.trim()}`));
    try {
      return await verifyServer(`http://127.0.0.1:${port}`, version, { timeoutMs, exited: () => exit });
    } finally {
      if (exit === null) {
        const closed = new Promise((r) => child.once('exit', r));
        child.kill();
        await closed;
      }
    }
  } finally {
    fs.rmSync(work, { recursive: true, force: true, maxRetries: 5, retryDelay: 200 });
  }
}

// ---- CLI --------------------------------------------------------------------------

function writeOutputs(outputs) {
  const file = process.env.GITHUB_OUTPUT;
  if (!file) return;
  const lines = Object.entries(outputs).map(([k, v]) =>
    Array.isArray(v) ? `${k}<<__EOF__\n${v.join('\n')}\n__EOF__` : `${k}=${v}`,
  );
  fs.appendFileSync(file, `${lines.join('\n')}\n`);
}

function parseArgs(argv) {
  const positional = [];
  const flags = {};
  for (let i = 0; i < argv.length; i++) {
    if (!argv[i].startsWith('--')) positional.push(argv[i]);
    // A flag with no value (--create) is true.
    else if (i + 1 >= argv.length || argv[i + 1].startsWith('--')) flags[argv[i].slice(2)] = true;
    else flags[argv[i].slice(2)] = argv[++i];
  }
  return { positional, flags };
}

export async function main(argv) {
  const [command, ...rest] = argv;
  const { positional, flags } = parseArgs(rest);
  const version = flags.version ?? workspaceVersion();
  switch (command) {
    case 'plan': {
      const plan = releasePlan(flags.tag, flags.image ? { image: flags.image } : {});
      console.log(JSON.stringify(plan, null, 2));
      writeOutputs({
        version: plan.version,
        prerelease: plan.prerelease,
        make_latest: plan.makeLatest,
        image_tags: plan.imageTags,
        image_ref: plan.imageRef,
        image_repo: plan.imageRepo,
      });
      return;
    }
    case 'verify-artifacts': {
      const names = verifyArtifacts(positional[0] ?? fail('verify-artifacts needs a directory'));
      console.log(`verified ${names.length} artifacts and SHA256SUMS in ${positional[0]}`);
      return;
    }
    case 'verify-binary': {
      const bin = positional[0] ?? fail('verify-binary needs a binary path');
      const r = await verifyBinary(path.resolve(bin), version);
      console.log(`${bin}: version ${r.version}, embedded UI ok (${r.entry})`);
      return;
    }
    case 'verify-server': {
      const url = positional[0] ?? fail('verify-server needs a URL');
      const r = await verifyServer(url, version, flags.password ? { password: flags.password } : {});
      console.log(`${positional[0]}: version ${r.version}, embedded UI ok (${r.entry})`);
      return;
    }
    case 'verify-image': {
      const { verifyImage } = await import('./image.mjs');
      const image = positional[0] ?? fail('verify-image needs an image reference');
      const mode = flags['bind-mount'] ?? 'auto';
      if (!['auto', 'on', 'off'].includes(mode)) fail(`--bind-mount must be auto, on or off, not "${mode}"`);
      await verifyImage(image, version, {
        ...(flags.platform ? { platform: flags.platform } : {}),
        ...(flags.record ? { record: flags.record } : {}),
        bindMount: mode,
      });
      console.log(`${image}: every image check passed for version ${version}`);
      return;
    }
    case 'verify-pushed-image': {
      const { verifyPushedImage } = await import('./image.mjs');
      const ref = positional[0] ?? fail('verify-pushed-image needs an image reference');
      await verifyPushedImage(ref, version, flags.tested ? { testedDir: flags.tested } : {});
      return;
    }
    case 'pretag': {
      const perf = await import('./performance.mjs');
      const r = perf.pretag({ tag: flags.tag, evidence: flags.evidence ? { dir: flags.evidence } : undefined, create: flags.create === true });
      console.log(perf.tagMessage(r.version, r).trimEnd());
      if (r.created) console.log(`
created annotated tag ${flags.tag} on ${r.sha}; publish it with:
  git push origin ${flags.tag}`);
      else console.log(`
${flags.tag} may be tagged on ${r.sha}; rerun with --create to create the tag`);
      return;
    }
    case 'check-performance': {
      const perf = await import('./performance.mjs');
      if (!flags.sha || !flags.evidence) fail('check-performance needs --sha and --evidence');
      const tagDate = 'tag-date' in flags ? (flags['tag-date'] === true ? '' : flags['tag-date']) : undefined;
      const r = perf.checkPerformance({ tag: flags.tag, sha: flags.sha, evidence: { dir: flags.evidence }, tagDate });
      console.log(perf.tagMessage(r.version, r).trimEnd());
      if (flags.artifacts) perf.checkArtifactsMatchApproval(r.records, flags.artifacts).forEach((l) => console.log(l));
      return;
    }
    case 'check-compat': {
      const perf = await import('./performance.mjs');
      const compat = await import('./compat.mjs');
      if (!flags.sha || !flags.evidence) fail('check-compat needs --sha and --evidence');
      const tagDate = 'tag-date' in flags ? (flags['tag-date'] === true ? '' : flags['tag-date']) : undefined;
      const evidence = { dir: flags.evidence };
      // Cleanup before performance: the review must predate every measurement of the candidate.
      const { records } = perf.checkPerformance({ tag: flags.tag, sha: flags.sha, evidence, tagDate });
      const r = compat.checkCompat({ tag: flags.tag, sha: flags.sha, evidence, tagDate, performance: records });
      console.log(compat.compatTagLine(r));
      if (flags.artifacts) perf.checkArtifactsMatchApproval([r.record], flags.artifacts).forEach((l) => console.log(l));
      return;
    }
    default:
      fail(`unknown command "${command}"; use plan, verify-artifacts, verify-binary, verify-server, verify-image, verify-pushed-image, pretag, check-performance or check-compat`);
  }
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  main(process.argv.slice(2)).catch((e) => {
    console.error(`release gate: ${e instanceof GateError ? e.message : (e.stack ?? e)}`);
    // Not process.exit(): exiting while fetch sockets close trips a libuv assertion on Windows.
    process.exitCode = 1;
  });
}
