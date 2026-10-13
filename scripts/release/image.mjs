// Container image gate (REL-09, docs/releasing.md#container-image-checks).
//
//   node scripts/release/release.mjs verify-image <image> [--version 0.1.2] [--platform linux/arm64]
//                                    [--bind-mount auto|on|off]
//
// Runs a built image the way an operator does and checks, on the platform it was built for:
//
//   1. config     it runs as the non-root `nonroot` user (uid 65532), carries the
//                 org.opencontainers.image.version label for `version`, and is for `platform`;
//   2. version    `--version` prints `vyrtel <version>`;
//   3. serve      on a fresh named volume it becomes ready, reports `version`, serves the
//                 embedded UI, passes its own healthcheck, and its process runs as uid 65532;
//   4. persist    events ingested into that volume, and the server's data directory and
//                 durability, are unchanged after the container is stopped, removed and
//                 recreated on the same volume, and the stop was clean (exit 0);
//   5. bind mount (Linux hosts) a bind-mounted directory the image's user cannot write stops
//                 the server with the documented uid hint; the documented fix
//                 (chown 65532:65532) then lets it start, when this host may chown.
//
// Every container and volume it creates is removed afterwards, pass or fail.

import crypto from 'node:crypto';
import { execFileSync } from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';

import { GateError, run, verifyServer } from './release.mjs';

/** uid/gid of the `nonroot` user the image runs as (crates/server/src/access.rs). */
export const IMAGE_UID = 65532;
export const VERSION_LABEL = 'org.opencontainers.image.version';
/** Platforms the release publishes (release.yml `image-smoke` matrix and push). */
export const PLATFORMS = ['linux/amd64', 'linux/arm64'];

const fail = (msg) => {
  throw new GateError(msg);
};

// ---- pure checks (unit-tested without Docker) ------------------------------------

/** The `--platform` value of this machine's architecture. */
export function hostPlatform(arch = os.arch()) {
  return `linux/${{ x64: 'amd64', arm64: 'arm64' }[arch] ?? arch}`;
}

/**
 * Checks one `docker image inspect` entry: user, version label, platform, data volume. A registry
 * inspection (`registry: true`) has no healthcheck to check: imagetools renders the config as the
 * OCI image config, which has no field for Docker's HEALTHCHECK.
 */
export function checkImageConfig(inspect, { version, platform, registry = false }) {
  const problems = [];
  const config = inspect?.Config ?? {};
  const user = config.User ?? '';
  const [name, group = name] = user.split(':');
  const nonRoot = (id) => id === 'nonroot' || id === String(IMAGE_UID);
  if (!nonRoot(name) || !nonRoot(group)) {
    problems.push(`runs as user "${user || 'root (unset)'}", expected nonroot:nonroot (uid/gid ${IMAGE_UID})`);
  }
  const label = config.Labels?.[VERSION_LABEL];
  if (label !== version) problems.push(`label ${VERSION_LABEL} is ${label === undefined ? 'missing' : `"${label}"`}, expected "${version}"`);
  const actual = `${inspect?.Os}/${inspect?.Architecture}`;
  if (platform && actual !== platform) problems.push(`is for ${actual}, expected ${platform}`);
  if (!Object.hasOwn(config.Volumes ?? {}, '/data')) problems.push('does not declare /data as a volume');
  if (!(config.Env ?? []).includes('VYRTEL_STORAGE_PATH=/data')) problems.push('does not set VYRTEL_STORAGE_PATH=/data');
  if (!registry && !config.Healthcheck?.Test?.includes('healthcheck')) problems.push('has no `vyrtel healthcheck` HEALTHCHECK');
  if (problems.length) fail(`image ${problems.join('; ')}`);
  return { user, version: label, platform: actual };
}

/** The uid of the `/vyrtel` process in `docker top <container> -o pid,uid,args` output. */
export function processUid(top) {
  const [header = '', ...rows] = top.trim().split(/\r?\n/);
  const col = header.trim().split(/\s+/).indexOf('UID');
  const row = rows.find((l) => /\/vyrtel\b/.test(l));
  if (col < 0 || !row) fail(`docker top shows no /vyrtel process with a UID column:\n${top.trim()}`);
  const uid = row.trim().split(/\s+/)[col];
  if (uid !== String(IMAGE_UID)) fail(`the server process runs as uid ${uid}, expected ${IMAGE_UID}`);
  return Number(uid);
}

/** The host port Docker published for the container's 8080, from `docker port`. */
export function publishedPort(output) {
  const m = /^(?:127\.0\.0\.1|0\.0\.0\.0):(\d+)$/m.exec(output.trim());
  if (!m) fail(`no IPv4 port published for 8080/tcp: ${output.trim() || '(none)'}`);
  return Number(m[1]);
}

/** What a recreated container must still answer: the same events and the same storage settings. */
export function checkPersisted(before, after) {
  const problems = [];
  const key = (e) => `${e.id} ${e.timestamp} ${e.message}`;
  const lost = before.events.filter((e) => !after.events.some((a) => key(a) === key(e)));
  if (lost.length) problems.push(`${lost.length} of ${before.events.length} events are gone or changed: ${lost.map(key).join(', ')}`);
  if (after.events.length !== before.events.length) problems.push(`query returned ${after.events.length} events, expected ${before.events.length}`);
  for (const field of ['dataDir', 'durability']) {
    if (after.info[field] !== before.info[field]) problems.push(`${field} changed from ${before.info[field]} to ${after.info[field]}`);
  }
  if (problems.length) fail(`data did not survive recreating the container: ${problems.join('; ')}`);
}

/** Checks a start refused on an unwritable bind mount explained the uid requirement. */
export function checkBindMountRefusal({ code, stderr }) {
  if (code === 0) fail('the server started on a bind mount its user cannot write');
  const missing = [
    'hint: the data directory /data must be writable',
    `The Docker image runs as uid/gid ${IMAGE_UID}`,
    `chown -R ${IMAGE_UID}:${IMAGE_UID}`,
    'docs/configuration.md#docker-and-bind-mounts',
  ].filter((s) => !stderr.includes(s));
  if (missing.length) fail(`an unwritable bind mount exited ${code} without the uid hint (missing: ${missing.join(' | ')}):\n${stderr.trim()}`);
  return stderr.split(/\r?\n/).find((l) => l.startsWith('vyrtel: ')) ?? '';
}

// ---- docker ----------------------------------------------------------------------

/** Ready line the server prints once it listens (crates/server/src/app.rs banner). */
const READY_LINE = /^Ready\.$/m;
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

async function docker(args, { allowFail = false } = {}) {
  const r = await run('docker', args, {});
  if (r.code !== 0 && !allowFail) fail(`docker ${args.join(' ')} exited ${r.code}: ${r.stderr.trim()}`);
  return r;
}

async function postJson(url, body) {
  const res = await fetch(url, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify(body),
    signal: AbortSignal.timeout(10_000),
  });
  const text = await res.text();
  if (!res.ok) fail(`POST ${url} answered HTTP ${res.status}: ${text}`);
  return JSON.parse(text);
}

async function getJson(url) {
  const res = await fetch(url, { signal: AbortSignal.timeout(10_000) });
  if (!res.ok) fail(`GET ${url} answered HTTP ${res.status}`);
  return res.json();
}

/** Starts a detached container on `mount`, returning its name and loopback base URL. */
async function start(image, platform, name, mount) {
  await docker(['run', '-d', '--platform', platform, '--name', name, '-v', mount, '-p', '127.0.0.1::8080', image]);
  const port = publishedPort((await docker(['port', name, '8080/tcp'])).stdout);
  return `http://127.0.0.1:${port}`;
}

/** verifyServer, with the container's logs in the failure. */
async function ready(name, base, version, timeoutMs) {
  try {
    return await verifyServer(base, version, { timeoutMs });
  } catch (e) {
    const logs = await docker(['logs', name], { allowFail: true });
    throw new GateError(`${e.message}\ncontainer logs:\n${(logs.stdout + logs.stderr).trim()}`);
  }
}

/** The events tagged with `service`, oldest first, and the storage settings the server reports. */
async function snapshot(base, service) {
  const q = await postJson(`${base}/api/v1/query/logs`, { query: `service = "${service}"`, from: '-24h', direction: 'forward', limit: 100 });
  const info = await getJson(`${base}/api/v1/system/info`);
  return { events: q.events, info };
}

async function stopCleanly(name) {
  await docker(['stop', '-t', '30', name]);
  const code = (await docker(['inspect', '-f', '{{.State.ExitCode}}', name])).stdout.trim();
  if (code !== '0') {
    const logs = await docker(['logs', name], { allowFail: true });
    fail(`container ${name} exited ${code} on docker stop, expected a clean shutdown (0):\n${(logs.stdout + logs.stderr).trim()}`);
  }
}

/**
 * Starts `image` detached on `mount`, which its user cannot write, and waits up to `timeoutMs`
 * for it to exit. Returns its exit code and output for checkBindMountRefusal. Fails at once if
 * the server reports it is ready, and at the deadline if it is still running, so a start that
 * unexpectedly succeeds can never hang the gate. The container is always removed. `exec` runs a
 * docker command (tests pass a fake).
 */
export async function awaitRefusal({ image, platform, name, mount, timeoutMs = 30_000, pollMs = 250, exec = docker }) {
  await exec(['run', '-d', '--platform', platform, '--name', name, '-v', mount, image]);
  try {
    const deadline = Date.now() + timeoutMs;
    for (;;) {
      const [running, code] = (await exec(['inspect', '-f', '{{.State.Running}} {{.State.ExitCode}}', name])).stdout.trim().split(/\s+/);
      const logs = await exec(['logs', name], { allowFail: true });
      const output = `${logs.stdout}${logs.stderr}`;
      if (running === 'false') return { code: Number(code), stderr: logs.stderr || logs.stdout };
      if (READY_LINE.test(output)) fail(`the server started and became ready on a bind mount its user cannot write:\n${output.trim()}`);
      if (Date.now() >= deadline) {
        fail(`the server was still running ${timeoutMs / 1000}s after starting on a bind mount its user cannot write; expected it to exit with the uid hint:\n${output.trim()}`);
      }
      await sleep(pollMs);
    }
  } finally {
    await exec(['rm', '-f', name], { allowFail: true });
  }
}

/** Whether this process may change a directory's owner, and how. */
function chownCommand() {
  if (process.getuid?.() === 0) return [];
  try {
    execFileSync('sudo', ['-n', 'true'], { stdio: 'ignore' });
    return ['sudo', '-n'];
  } catch {
    return null;
  }
}

function asHost(prefix, command, args) {
  const [cmd, ...rest] = [...prefix, command, ...args];
  execFileSync(cmd, rest, { stdio: 'inherit' });
}

/** Step 5: an unwritable bind mount is refused with the hint; the documented fix works. */
async function checkBindMount(image, platform, version, prefix, timeoutMs, log) {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'vyrtel-bind-'));
  // Owned by this (non-65532) user, and read-only for everyone else: what a directory
  // created by root or by `docker run -v ./dir:/data` itself looks like to the image's user.
  fs.chmodSync(dir, 0o555);
  const sudo = chownCommand();
  const name = `${prefix}-bind`;
  try {
    const refused = await awaitRefusal({ image, platform, name: `${prefix}-refused`, mount: `${dir}:/data`, timeoutMs });
    const reason = checkBindMountRefusal(refused);
    log(`bind mount: unwritable ${dir} refused with the uid hint (${reason})`);

    if (sudo === null) {
      log('bind mount: documented fix not exercised (needs root or passwordless sudo to chown)');
      return;
    }
    asHost(sudo, 'chown', [`${IMAGE_UID}:${IMAGE_UID}`, dir]);
    asHost(sudo, 'chmod', ['755', dir]);
    const base = await start(image, platform, name, `${dir}:/data`);
    await ready(name, base, version, timeoutMs);
    await stopCleanly(name);
    log(`bind mount: after chown ${IMAGE_UID}:${IMAGE_UID} the server starts and stops cleanly`);
  } finally {
    await docker(['rm', '-f', name], { allowFail: true });
    try {
      if (sudo) asHost(sudo, 'rm', ['-rf', dir]);
      else {
        fs.chmodSync(dir, 0o755);
        fs.rmSync(dir, { recursive: true, force: true });
      }
    } catch {
      // A leftover temp directory is not a release failure.
    }
  }
}

/**
 * Runs every check above against `image` on `platform`. Returns the evidence lines it logged.
 * `bindMount`: 'auto' (Linux hosts only, where Docker enforces host ownership), 'on' or 'off'.
 * `record`: a directory to write the tested image's record to (testedRecord), once every check
 * has passed, so the published image can be checked against it.
 */
export async function verifyImage(image, version, { platform = hostPlatform(), bindMount = 'auto', record, timeoutMs = 60_000, log = console.log } = {}) {
  const evidence = [];
  const note = (line) => {
    evidence.push(line);
    log(line);
  };
  const prefix = `vyrtel-smoke-${crypto.randomBytes(4).toString('hex')}`;
  const volume = `${prefix}-data`;
  const [first, second] = [`${prefix}-1`, `${prefix}-2`];
  try {
    const [inspect] = JSON.parse((await docker(['image', 'inspect', image])).stdout);
    const config = checkImageConfig(inspect, { version, platform });
    note(`config: ${image} is ${config.platform}, user ${config.user}, ${VERSION_LABEL}=${config.version}`);

    const printed = (await docker(['run', '--rm', '--platform', platform, image, '--version'])).stdout.trim();
    if (printed !== `vyrtel ${version}`) fail(`${image} --version printed "${printed}", expected "vyrtel ${version}"`);
    note(`version: --version printed "${printed}"`);

    await docker(['volume', 'create', volume]);
    let base = await start(image, platform, first, `${volume}:/data`);
    const served = await ready(first, base, version, timeoutMs);
    await docker(['exec', first, '/vyrtel', 'healthcheck']);
    const uid = processUid((await docker(['top', first, '-o', 'pid,uid,args'])).stdout);
    note(`serve: ready on a new volume, version ${served.version}, UI ${served.entry}, healthcheck ok, process uid ${uid}`);

    const service = `${prefix}-persist`;
    const events = ['first', 'second', 'third'].map((n, i) => ({ level: 'Information', service, message: `${service} ${n}`, properties: { seq: i } }));
    const accepted = await postJson(`${base}/api/v1/events`, events);
    if (accepted.accepted !== events.length) fail(`ingest accepted ${accepted.accepted} of ${events.length} events`);
    const before = await snapshot(base, service);
    if (before.events.length !== events.length) fail(`query found ${before.events.length} of ${events.length} acknowledged events`);
    await stopCleanly(first);
    await docker(['rm', first]);

    base = await start(image, platform, second, `${volume}:/data`);
    await ready(second, base, version, timeoutMs);
    const after = await snapshot(base, service);
    checkPersisted(before, after);
    await stopCleanly(second);
    note(`persist: ${after.events.length} events (ids ${after.events.map((e) => e.id).join(', ')}), dataDir ${after.info.dataDir} and durability ${after.info.durability} unchanged after recreating the container on volume ${volume}`);

    if (bindMount === 'on' || (bindMount === 'auto' && process.platform === 'linux')) {
      await checkBindMount(image, platform, version, prefix, timeoutMs, note);
    } else {
      note(`bind mount: skipped (${bindMount === 'off' ? '--bind-mount off' : `${process.platform} host; Docker enforces bind-mount ownership only on Linux`})`);
    }
    if (record) {
      const tested = testedRecord(inspect, platform);
      fs.mkdirSync(record, { recursive: true });
      fs.writeFileSync(path.join(record, recordName(platform)), `${JSON.stringify(tested, null, 2)}\n`);
      note(`record: ${tested.diffIds.length} layers of the tested ${platform} image written to ${path.join(record, recordName(platform))}`);
    }
    return evidence;
  } finally {
    await docker(['rm', '-f', first, second], { allowFail: true });
    await docker(['volume', 'rm', '-f', volume], { allowFail: true });
  }
}

// ---- published image -------------------------------------------------------------

/** File name of a tested-image record: `linux-amd64.json`. */
export const recordName = (platform) => `${platform.replace(/\//g, '-')}.json`;

/**
 * What verify-image tested on one platform, for checking the published image against it:
 * the uncompressed layer digests (diff IDs) identify the image's files exactly, however the
 * layers are compressed, tagged or wrapped in a manifest list.
 */
export function testedRecord(inspect, platform) {
  const diffIds = inspect?.RootFS?.Layers ?? [];
  if (!diffIds.length) fail(`docker image inspect reported no layers for the ${platform} image`);
  return { platform, imageId: inspect.Id, diffIds };
}

/** Reads every `*.json` record in `dir`, keyed by platform. */
export function readRecords(dir) {
  const records = {};
  for (const f of fs.readdirSync(dir).filter((n) => n.endsWith('.json'))) {
    const r = JSON.parse(fs.readFileSync(path.join(dir, f), 'utf8'));
    records[r.platform] = r;
  }
  if (!Object.keys(records).length) fail(`no tested-image records in ${dir}`);
  return records;
}

/**
 * `docker buildx imagetools inspect --format '{{json .Image}}'` prints one config for a
 * single-platform manifest and a map keyed by platform for a manifest list: always the map.
 */
export function imagesByPlatform(images) {
  if (images && typeof images.architecture === 'string') return { [`${images.os}/${images.architecture}`]: images };
  return images ?? {};
}

/**
 * Checks a pushed image: every platform in `platforms` is present and passes checkImageConfig
 * and, for each platform in `tested`, has exactly the layers of the image that was smoke-tested.
 * `images` is the output of imagesByPlatform.
 */
export function checkManifest(images, version, platforms = PLATFORMS, tested = {}) {
  const missing = platforms.filter((p) => !images?.[p]);
  if (missing.length) fail(`the pushed image has no ${missing.join(' or ')} variant (has ${Object.keys(images ?? {}).join(', ') || 'none'})`);
  return platforms.map((platform) => {
    const oci = images[platform];
    const checked = checkImageConfig({ Os: oci.os, Architecture: oci.architecture, Config: oci.config }, { version, platform, registry: true });
    const want = tested[platform]?.diffIds;
    if (want) {
      const got = oci.rootfs?.diff_ids ?? [];
      if (got.length !== want.length || got.some((d, i) => d !== want[i])) {
        fail(`the pushed ${platform} image is not the one that was smoke-tested: its layers are ${got.join(', ') || 'none'}, the tested image's are ${want.join(', ')}`);
      }
    }
    return { ...checked, sameAsTested: Boolean(want) };
  });
}

/**
 * Inspects a pushed image in its registry, without pulling it, and runs checkManifest. With
 * `testedDir`, the platforms are those recorded there (verify-image --record) and each must have
 * the tested image's layers; otherwise every published platform is checked for config only.
 */
export async function verifyPushedImage(ref, version, { testedDir, log = console.log } = {}) {
  const tested = testedDir ? readRecords(testedDir) : {};
  const platforms = testedDir ? Object.keys(tested).sort() : PLATFORMS;
  const out = (await docker(['buildx', 'imagetools', 'inspect', ref, '--format', '{{json .Image}}'])).stdout;
  const checked = checkManifest(imagesByPlatform(JSON.parse(out)), version, platforms, tested);
  for (const c of checked) {
    log(`${ref}: ${c.platform}, user ${c.user}, ${VERSION_LABEL}=${c.version}${c.sameAsTested ? ', same layers as the smoke-tested image' : ''}`);
  }
  return checked;
}
