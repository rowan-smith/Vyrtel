// Tests for the release gate (release.mjs) and the release workflow's gating contract.
//
//   node --test scripts/release/release.test.mjs
//   VYRTEL_BIN=target/debug/vyrtel node --test scripts/release/release.test.mjs   # + a real binary

import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { after, describe, test } from 'node:test';

import {
  checkIndexHtml,
  checkTagMatchesWorkspace,
  EXPECTED_ARTIFACTS,
  GateError,
  parseChecksums,
  parseTag,
  releasePlan,
  ROOT,
  verifyArtifacts,
  verifyBinary,
  workspaceVersion,
} from './release.mjs';

const FAKE = path.join(import.meta.dirname, 'testdata', 'fake-vyrtel.mjs');
const temps = [];
after(() => temps.forEach((d) => fs.rmSync(d, { recursive: true, force: true })));

function tempDir() {
  const d = fs.mkdtempSync(path.join(os.tmpdir(), 'vyrtel-release-test-'));
  temps.push(d);
  return d;
}

/** A repo root whose Cargo.toml, Cargo.lock and web/package.json record the given versions. */
function workspace({ cargo = '0.1.2', lock = cargo, web = cargo } = {}) {
  const root = tempDir();
  fs.writeFileSync(
    path.join(root, 'Cargo.toml'),
    `[workspace]\nmembers = ["crates/server"]\n\n[workspace.package]\nversion = "${cargo}"\nedition = "2024"\n\n[workspace.dependencies]\nserde = { version = "1" }\n`,
  );
  fs.writeFileSync(
    path.join(root, 'Cargo.lock'),
    `version = 4\n\n[[package]]\nname = "serde"\nversion = "1.0.0"\n\n[[package]]\nname = "server"\nversion = "${lock}"\n`,
  );
  fs.mkdirSync(path.join(root, 'web'));
  fs.writeFileSync(path.join(root, 'web/package.json'), JSON.stringify({ name: 'vyrtel-web', version: web }));
  return root;
}

/** A repo file with LF line endings, whatever core.autocrlf did to it. */
const repoFile = (rel) => fs.readFileSync(path.join(ROOT, rel), 'utf8').replace(/\r\n/g, '\n');

const gateError = (pattern) => (e) => e instanceof GateError && pattern.test(e.message);

describe('AC1: a tag whose version differs from the workspace fails', () => {
  test('matching stable tag passes', () => {
    assert.equal(checkTagMatchesWorkspace('v0.1.2', workspace()).version, '0.1.2');
  });

  test('tag ahead of the workspace fails and names the stale file', () => {
    assert.throws(() => checkTagMatchesWorkspace('v0.1.3', workspace()), gateError(/0\.1\.3 but Cargo\.toml \[workspace\.package\] = 0\.1\.2/));
  });

  test('prerelease tag must carry the same prerelease version', () => {
    assert.throws(() => checkTagMatchesWorkspace('v0.1.2-rc.1', workspace()), gateError(/0\.1\.2-rc\.1 but/));
    assert.equal(checkTagMatchesWorkspace('v0.1.2-rc.1', workspace({ cargo: '0.1.2-rc.1' })).prerelease, true);
  });

  test('stale Cargo.lock or web/package.json fails even when Cargo.toml matches', () => {
    assert.throws(() => checkTagMatchesWorkspace('v0.1.2', workspace({ lock: '0.1.1' })), gateError(/Cargo\.lock \(server\) = 0\.1\.1/));
    assert.throws(() => checkTagMatchesWorkspace('v0.1.2', workspace({ web: '0.1.1' })), gateError(/web\/package\.json = 0\.1\.1/));
  });

  test('malformed tags are rejected', () => {
    for (const tag of ['0.1.2', 'v0.1', 'v01.1.2', 'v0.1.2-', 'v0.1.2-rc..1', 'release-0.1.2', '', undefined]) {
      assert.throws(() => parseTag(tag), gateError(/not a release tag/), String(tag));
    }
  });

  test('the repository itself is consistent', () => {
    const version = workspaceVersion();
    assert.equal(checkTagMatchesWorkspace(`v${version}`, ROOT).version, version);
  });
});

describe('AC4: a prerelease does not replace the stable latest channel', () => {
  test('stable release is latest and moves latest and major.minor image tags', () => {
    const plan = releasePlan('v0.1.2', { image: 'ghcr.io/rowan-smith/Vyrtel', root: workspace() });
    assert.equal(plan.prerelease, false);
    assert.equal(plan.makeLatest, true);
    assert.deepEqual(plan.imageTags, ['ghcr.io/rowan-smith/vyrtel:0.1.2', 'ghcr.io/rowan-smith/vyrtel:0.1', 'ghcr.io/rowan-smith/vyrtel:latest']);
  });

  test('prerelease is marked, not latest, and gets only its exact image tag', () => {
    for (const v of ['0.1.2-rc.1', '0.2.0-beta', '1.0.0-alpha.3']) {
      const plan = releasePlan(`v${v}`, { root: workspace({ cargo: v }) });
      assert.equal(plan.prerelease, true, v);
      assert.equal(plan.makeLatest, false, v);
      assert.deepEqual(plan.imageTags, [`ghcr.io/rowan-smith/vyrtel:${v}`], v);
    }
  });
});

/** A release directory with the expected binaries and a correct SHA256SUMS. */
function artifactDir() {
  const dir = tempDir();
  const lines = EXPECTED_ARTIFACTS.map((name) => {
    const body = `binary ${name}`;
    fs.writeFileSync(path.join(dir, name), body);
    return `${createHash('sha256').update(body).digest('hex')}  ${name}`;
  });
  fs.writeFileSync(path.join(dir, 'SHA256SUMS'), `${lines.join('\n')}\n`);
  return dir;
}

describe('AC3: published artifacts and checksums are verified', () => {
  test('complete set with matching checksums passes', () => {
    assert.deepEqual(verifyArtifacts(artifactDir()), EXPECTED_ARTIFACTS);
  });

  test('missing binary fails', () => {
    const dir = artifactDir();
    fs.rmSync(path.join(dir, 'vyrtel-macos-arm64'));
    assert.throws(() => verifyArtifacts(dir), gateError(/missing artifact vyrtel-macos-arm64/));
  });

  test('tampered binary fails', () => {
    const dir = artifactDir();
    fs.appendFileSync(path.join(dir, 'vyrtel-linux-arm64'), 'x');
    assert.throws(() => verifyArtifacts(dir), gateError(/checksum mismatch for vyrtel-linux-arm64/));
  });

  test('missing SHA256SUMS or entry fails', () => {
    const dir = artifactDir();
    const sums = fs.readFileSync(path.join(dir, 'SHA256SUMS'), 'utf8');
    fs.writeFileSync(path.join(dir, 'SHA256SUMS'), sums.split('\n').filter((l) => !l.includes('windows')).join('\n'));
    assert.throws(() => verifyArtifacts(dir), gateError(/no entry for vyrtel-windows-x86_64\.exe/));
    fs.rmSync(path.join(dir, 'SHA256SUMS'));
    assert.throws(() => verifyArtifacts(dir), gateError(/has no SHA256SUMS/));
  });

  test('unexpected or empty files fail', () => {
    const dir = artifactDir();
    fs.writeFileSync(path.join(dir, 'vyrtel-linux-x86_64.sha256'), 'leftover');
    assert.throws(() => verifyArtifacts(dir), gateError(/unexpected file vyrtel-linux-x86_64\.sha256/));

    const empty = artifactDir();
    fs.writeFileSync(path.join(empty, 'vyrtel-linux-x86_64'), '');
    const sums = fs.readFileSync(path.join(empty, 'SHA256SUMS'), 'utf8').replace(/^\w{64}(?=  vyrtel-linux-x86_64$)/m, createHash('sha256').digest('hex'));
    fs.writeFileSync(path.join(empty, 'SHA256SUMS'), sums);
    assert.throws(() => verifyArtifacts(empty), gateError(/vyrtel-linux-x86_64 is empty/));
  });

  test('checksum file parsing accepts sha256sum/shasum output and rejects junk', () => {
    const hex = 'a'.repeat(64);
    assert.deepEqual([...parseChecksums(`${hex}  ./a\n${hex} *b\r\n\n`)], [['a', hex], ['b', hex]]);
    assert.throws(() => parseChecksums('not a checksum line'), gateError(/line 1 is malformed/));
    assert.throws(() => parseChecksums(`${hex}  a\n${hex}  a`), gateError(/lists a twice/));
  });

  test('the expected artifact list matches the binaries matrix in release.yml', () => {
    const workflow = repoFile('.github/workflows/release.yml');
    const matrix = [...workflow.matchAll(/target: (\S+), name: (vyrtel-[\w-]+) \}/g)].map(([, target, name]) =>
      target.includes('windows') ? `${name}.exe` : name,
    );
    assert.deepEqual(matrix.sort(), [...EXPECTED_ARTIFACTS].sort());
  });
});

describe('AC3: embedded UI and version output are verified', () => {
  test('built index.html passes and yields its entry script', () => {
    // The shape `vite build` writes to web/dist/index.html.
    const html = '<head><script type="module" crossorigin src="/assets/index-tLgrRCUF.js"></script></head><body><div id="root"></div></body>';
    assert.equal(checkIndexHtml(html), '/assets/index-tLgrRCUF.js');
  });

  test('placeholder page, unbuilt dev index.html and other pages fail', () => {
    const buildRs = repoFile('crates/server/build.rs');
    assert.ok(buildRs.includes('The web UI was not built into this binary'), 'placeholder marker matches build.rs');
    assert.throws(() => checkIndexHtml('<p>The web UI was not built into this binary.</p><div id="root"></div>'), gateError(/placeholder/));
    const devIndex = repoFile('web/index.html');
    assert.throws(() => checkIndexHtml(devIndex), gateError(/\/assets\/\*\.js/));
    assert.throws(() => checkIndexHtml('<html>hello</html>'), gateError(/no #root/));
  });

  const fake = (mode, extra = {}) => {
    const env = { FAKE_VYRTEL_MODE: mode, FAKE_VYRTEL_VERSION: '0.1.2', ...extra };
    const saved = { ...process.env };
    return {
      run: async (version = '0.1.2') => {
        Object.assign(process.env, env);
        try {
          return await verifyBinary(process.execPath, version, { prefixArgs: [FAKE], timeoutMs: 10_000 });
        } finally {
          for (const k of Object.keys(env)) if (saved[k] === undefined) delete process.env[k];
        }
      },
    };
  };

  test('binary with matching version and embedded UI passes', async () => {
    assert.deepEqual(await fake('ok').run(), { version: '0.1.2', entry: '/assets/index-abc123.js' });
  });

  test('wrong --version output fails', async () => {
    await assert.rejects(fake('ok', { FAKE_VYRTEL_PRINTED_VERSION: '0.1.1' }).run(), gateError(/printed "vyrtel 0\.1\.1", expected "vyrtel 0\.1\.2"/));
  });

  test('server reporting another version fails', async () => {
    await assert.rejects(fake('wrong-version', { FAKE_VYRTEL_SERVED_VERSION: '0.1.1' }).run(), gateError(/reports version 0\.1\.1, expected 0\.1\.2/));
  });

  test('binary with the placeholder UI fails', async () => {
    await assert.rejects(fake('placeholder').run(), gateError(/web UI is not embedded/));
  });

  test('binary whose UI assets are missing fails', async () => {
    await assert.rejects(fake('no-asset').run(), gateError(/asset \/assets\/index-abc123\.js answered HTTP 404/));
  });

  test('binary that exits on startup fails fast', async () => {
    await assert.rejects(fake('crash').run(), gateError(/exited before it became healthy: exit 2; stderr: vyrtel: configuration error: simulated/));
  });

  test('real binary (set VYRTEL_BIN)', { skip: !process.env.VYRTEL_BIN && 'VYRTEL_BIN not set' }, async () => {
    const r = await verifyBinary(path.resolve(process.env.VYRTEL_BIN), workspaceVersion());
    assert.equal(r.version, workspaceVersion());
  });
});

/** `needs:` of each job in a workflow, read from the single-line form this repo uses. */
function jobNeeds(workflow) {
  const jobs = workflow.slice(workflow.indexOf('\njobs:'));
  const needs = {};
  let job;
  for (const line of jobs.split(/\r?\n/)) {
    const name = /^ {2}([\w-]+):\s*$/.exec(line);
    if (name) needs[(job = name[1])] = [];
    const n = /^ {4}needs: \[?([^\]]+)\]?\s*$/.exec(line);
    if (n && job) needs[job] = n[1].split(',').map((s) => s.trim());
  }
  return needs;
}

function upstream(needs, job, seen = new Set()) {
  for (const n of needs[job] ?? []) if (!seen.has(n)) upstream(needs, n, seen.add(n));
  return seen;
}

describe('AC2: nothing is published unless every gate passed', () => {
  const workflow = repoFile('.github/workflows/release.yml');
  const needs = jobNeeds(workflow);

  test('the release and image jobs depend on preflight, CI, binaries and verify', () => {
    for (const job of ['image', 'release']) {
      for (const gate of ['preflight', 'ci', 'binaries', 'verify']) {
        assert.ok(upstream(needs, job).has(gate), `${job} must (transitively) need ${gate}`);
      }
    }
    assert.ok(upstream(needs, 'release').has('image'), 'the GitHub release is created after the image is pushed');
  });

  test('ci runs the whole CI workflow, which is callable', () => {
    assert.match(workflow, /\n {2}ci:\n(?: {4}.*\n)*? {4}uses: \.\/\.github\/workflows\/ci\.yml\n/);
    const ci = repoFile('.github/workflows/ci.yml');
    assert.match(ci, /\n {2}workflow_call:/);
  });

  test('only a tag push publishes; a manual run is a rehearsal', () => {
    assert.match(workflow, /PUBLISH: \$\{\{ github\.event_name == 'push' \}\}/);
    assert.match(workflow, /\n {2}release:\n(?: {4}.*\n)*? {4}if: github\.event_name == 'push'\n/);
    assert.equal((workflow.match(/push: true/g) ?? []).length, 1, 'one image push');
    assert.match(workflow, /if: env\.PUBLISH == 'true'\n {8}uses: docker\/build-push-action@v6\n(?: {8}.*\n)*? {10}push: true/);
  });

  test('channels come from the preflight plan, never hard-coded', () => {
    assert.doesNotMatch(workflow, /latest=true/);
    assert.match(workflow, /prerelease: \$\{\{ needs\.preflight\.outputs\.prerelease \}\}/);
    assert.match(workflow, /make_latest: \$\{\{ needs\.preflight\.outputs\.make_latest \}\}/);
    assert.match(workflow, /tags: \$\{\{ needs\.preflight\.outputs\.image_tags \}\}/);
  });

  test('release builds refuse to embed the placeholder UI', () => {
    assert.match(workflow, /VYRTEL_REQUIRE_WEB_UI: '1'/);
    assert.match(repoFile('Dockerfile'), /VYRTEL_REQUIRE_WEB_UI=1 cargo build --release/);
  });
});
