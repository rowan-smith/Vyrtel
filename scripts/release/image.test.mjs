// Tests for the container image gate (image.mjs) and the release workflow's image contract (REL-09).
//
//   node --test scripts/release/image.test.mjs
//   VYRTEL_IMAGE=vyrtel:rc node --test scripts/release/image.test.mjs   # + a real image (needs Docker)

import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import { describe, test } from 'node:test';

import {
  awaitRefusal,
  checkBindMountRefusal,
  checkImageConfig,
  checkManifest,
  checkPersisted,
  hostPlatform,
  IMAGE_UID,
  imagesByPlatform,
  PLATFORMS,
  processUid,
  publishedPort,
  recordName,
  testedRecord,
  verifyImage,
  VERSION_LABEL,
} from './image.mjs';
import { GateError, ROOT, workspaceVersion } from './release.mjs';

const repoFile = (rel) => fs.readFileSync(path.join(ROOT, rel), 'utf8').replace(/\r\n/g, '\n');
const gateError = (pattern) => (e) => e instanceof GateError && pattern.test(e.message);

/**
 * The image config as `docker buildx imagetools inspect` shows it: the OCI config, which has no
 * field for Docker's HEALTHCHECK.
 */
const ociConfig = (opts) => {
    const { Healthcheck, ...config } = inspect(opts).Config;
    return config;
};

/** A `docker image inspect` entry for the image the Dockerfile builds, with `overrides` applied. */
function inspect({ user = 'nonroot:nonroot', label = '0.1.2', arch = 'amd64', config = {} } = {}) {
  return {
    Os: 'linux',
    Architecture: arch,
    Config: {
      User: user,
      Env: ['PATH=/usr/local/bin:/usr/bin', 'VYRTEL_STORAGE_PATH=/data', 'VYRTEL_SERVER_BIND=0.0.0.0:8080'],
      Volumes: { '/data': {} },
      Healthcheck: { Test: ['CMD', '/vyrtel', 'healthcheck'] },
      Labels: label === null ? {} : { [VERSION_LABEL]: label },
      ...config,
    },
  };
}

describe('AC4: the image runs as the intended non-root user and advertises its version', () => {
  test('the Dockerfile image passes', () => {
    assert.deepEqual(checkImageConfig(inspect(), { version: '0.1.2', platform: 'linux/amd64' }), {
      user: 'nonroot:nonroot',
      version: '0.1.2',
      platform: 'linux/amd64',
    });
    assert.equal(checkImageConfig(inspect({ user: '65532:65532', arch: 'arm64' }), { version: '0.1.2', platform: 'linux/arm64' }).platform, 'linux/arm64');
  });

  test('root, an unset user and a root group fail', () => {
    for (const user of ['', 'root', '0:0', 'nonroot:root']) {
      assert.throws(() => checkImageConfig(inspect({ user }), { version: '0.1.2' }), gateError(/runs as user .*expected nonroot:nonroot \(uid\/gid 65532\)/));
    }
  });

  test('a missing or different version label fails', () => {
    assert.throws(() => checkImageConfig(inspect({ label: null }), { version: '0.1.2' }), gateError(/org\.opencontainers\.image\.version is missing, expected "0\.1\.2"/));
    assert.throws(() => checkImageConfig(inspect({ label: 'dev' }), { version: '0.1.2' }), gateError(/is "dev", expected "0\.1\.2"/));
  });

  test('an image for another platform fails', () => {
    assert.throws(() => checkImageConfig(inspect({ arch: 'amd64' }), { version: '0.1.2', platform: 'linux/arm64' }), gateError(/is for linux\/amd64, expected linux\/arm64/));
  });

  test('an image without the /data volume, storage path or healthcheck fails, naming each', () => {
    const bare = inspect({ config: { Volumes: null, Env: [], Healthcheck: null } });
    assert.throws(() => checkImageConfig(bare, { version: '0.1.2' }), gateError(/\/data as a volume; .*VYRTEL_STORAGE_PATH=\/data; .*HEALTHCHECK/));
  });

  test('the running process must be uid 65532', () => {
    const top = 'PID                 UID                 COMMAND\n4242                65532               /vyrtel\n';
    assert.equal(processUid(top), IMAGE_UID);
    assert.throws(() => processUid(top.replace('65532', '0')), gateError(/runs as uid 0, expected 65532/));
    assert.throws(() => processUid('PID UID COMMAND\n'), gateError(/no \/vyrtel process/));
    assert.throws(() => processUid('PID COMMAND\n1 /vyrtel\n'), gateError(/UID column/));
  });

  test('the Dockerfile sets the user, the version label and the /data owner', () => {
    const dockerfile = repoFile('Dockerfile');
    assert.match(dockerfile, /\nUSER nonroot:nonroot\n/);
    assert.match(dockerfile, /\nARG VYRTEL_VERSION=dev\n/);
    assert.match(dockerfile, /org\.opencontainers\.image\.version="\$\{VYRTEL_VERSION\}"/);
    assert.match(dockerfile, /COPY --from=build --chown=nonroot:nonroot \/data \/data/);
    assert.match(dockerfile, /VOLUME \["\/data"\]/);
  });
});

describe('AC2: events in a named volume survive recreating the container', () => {
  const events = [
    { id: '000000000000002a', timestamp: '2026-10-13T00:00:00.000Z', message: 'a' },
    { id: '000000000000002b', timestamp: '2026-10-13T00:00:00.001Z', message: 'b' },
  ];
  const info = { dataDir: '/data', durability: 'batch' };

  test('the same events and settings pass', () => {
    checkPersisted({ events, info }, { events: [...events], info: { ...info } });
  });

  test('a lost or changed event fails', () => {
    assert.throws(() => checkPersisted({ events, info }, { events: [events[0]], info }), gateError(/1 of 2 events are gone or changed: 000000000000002b/));
    const changed = [events[0], { ...events[1], id: '0000000000000099' }];
    assert.throws(() => checkPersisted({ events, info }, { events: changed, info }), gateError(/gone or changed/));
  });

  test('extra events or a changed data directory or durability fail', () => {
    assert.throws(() => checkPersisted({ events, info }, { events: [...events, events[0]], info }), gateError(/returned 3 events, expected 2/));
    assert.throws(() => checkPersisted({ events, info }, { events, info: { ...info, dataDir: '/tmp' } }), gateError(/dataDir changed from \/data to \/tmp/));
    assert.throws(() => checkPersisted({ events, info }, { events, info: { ...info, durability: 'none' } }), gateError(/durability changed/));
  });

  test('the published port is read from docker port output', () => {
    assert.equal(publishedPort('127.0.0.1:49153\n'), 49153);
    assert.equal(publishedPort('0.0.0.0:32768\n[::]:32768\n'), 32768);
    assert.throws(() => publishedPort(''), gateError(/no IPv4 port published/));
  });
});

describe('AC3: an unwritable bind mount explains the documented uid requirement', () => {
  const stderr = [
    'vyrtel: creating /data/wal: Permission denied (os error 13)',
    'hint: the data directory /data must be writable by the user running Vyrtel.',
    '      Vyrtel runs as uid 65532 (gid 65532); /data is owned by uid 1001 (gid 118) with mode 555.',
    '      The Docker image runs as uid/gid 65532. Named volumes get that owner automatically;',
    '      for a bind mount, give the host directory to that user first, e.g.',
    '        sudo chown -R 65532:65532 ./vyrtel-data',
    '      See docs/configuration.md#docker-and-bind-mounts.',
  ].join('\n');

  test('a refusal with the hint passes and reports the reason', () => {
    assert.equal(checkBindMountRefusal({ code: 1, stderr }), 'vyrtel: creating /data/wal: Permission denied (os error 13)');
  });

  test('a start that succeeds, or a refusal without the hint, fails', () => {
    assert.throws(() => checkBindMountRefusal({ code: 0, stderr: '' }), gateError(/started on a bind mount/));
    assert.throws(() => checkBindMountRefusal({ code: 1, stderr: 'vyrtel: Permission denied (os error 13)' }), gateError(/without the uid hint/));
  });

  /**
   * A fake `docker` for awaitRefusal: the container exits after `exitAfter` inspections (never
   * when null), printing `stdout`/`stderr`. Records every command it is given.
   */
  function fakeDocker({ exitAfter = null, code = 1, stdout = '', stderr: err = '' }) {
    const calls = [];
    let inspections = 0;
    const exec = async (args) => {
      calls.push(args.join(' '));
      if (args[0] === 'inspect') {
        inspections += 1;
        const exited = exitAfter !== null && inspections > exitAfter;
        return { code: 0, stdout: exited ? `false ${code}\n` : 'true 0\n', stderr: '' };
      }
      if (args[0] === 'logs') return { code: 0, stdout, stderr: err };
      return { code: 0, stdout: '', stderr: '' };
    };
    return { exec, calls };
  }
  const refusal = (exec, timeoutMs = 400) =>
    awaitRefusal({ image: 'vyrtel:rc', platform: 'linux/amd64', name: 'refused', mount: '/tmp/x:/data', timeoutMs, pollMs: 20, exec });

  test('the refusal is awaited detached and the container removed', async () => {
    const docker = fakeDocker({ exitAfter: 2, code: 1, stderr });
    const r = await refusal(docker.exec);
    assert.deepEqual(r, { code: 1, stderr });
    assert.equal(checkBindMountRefusal(r).startsWith('vyrtel: creating /data/wal'), true);
    assert.equal(docker.calls[0], 'run -d --platform linux/amd64 --name refused -v /tmp/x:/data vyrtel:rc');
    assert.equal(docker.calls.at(-1), 'rm -f refused');
  });

  test('a server that keeps running fails at the deadline instead of hanging, and is removed', async () => {
    const docker = fakeDocker({ exitAfter: null, stdout: 'Vyrtel 0.1.2\n' });
    const started = Date.now();
    await assert.rejects(refusal(docker.exec, 300), gateError(/still running 0\.3s after starting on a bind mount its user cannot write/));
    assert.ok(Date.now() - started < 5_000, 'fails within its deadline');
    assert.equal(docker.calls.at(-1), 'rm -f refused');
  });

  test('a server that becomes ready fails at once, before the deadline, and is removed', async () => {
    const docker = fakeDocker({ exitAfter: null, stdout: 'Vyrtel 0.1.2\n\nReady.\n' });
    const started = Date.now();
    await assert.rejects(refusal(docker.exec, 60_000), gateError(/started and became ready on a bind mount/));
    assert.ok(Date.now() - started < 5_000, 'does not wait for the deadline');
    assert.equal(docker.calls.at(-1), 'rm -f refused');
  });

  test('the hint the gate expects is the one the server prints', () => {
    const access = repoFile('crates/server/src/access.rs');
    assert.match(access, /pub const IMAGE_UID: u32 = 65532;/);
    assert.match(access, /hint: the data directory \{dir\} must be writable/);
    assert.match(access, /The Docker image runs as uid\/gid \{IMAGE_UID\}/);
    assert.match(access, /sudo chown -R \{IMAGE_UID\}:\{IMAGE_UID\}/);
    assert.match(repoFile('docs/configuration.md'), /\n## Docker and bind mounts\n/);
  });
});

/** The body of one job in a workflow: the lines after `  <job>:` up to the next job. */
function jobBody(workflow, job) {
  const start = workflow.indexOf(`\n  ${job}:\n`);
  assert.ok(start >= 0, `job ${job} exists`);
  const rest = workflow.slice(start + job.length + 5);
  const end = rest.search(/\n {2}[\w-]+:\n/);
  return end < 0 ? rest : rest.slice(0, end);
}

describe('AC1: the release checks the image on every published architecture', () => {
  const workflow = repoFile('.github/workflows/release.yml');

  test('image-smoke runs natively on amd64 and arm64, before anything is pushed', () => {
    const smoke = jobBody(workflow, 'image-smoke');
    assert.match(smoke, /needs: \[preflight, verify\]/);
    assert.match(smoke, /platform: linux\/amd64, runner: ubuntu-latest/);
    assert.match(smoke, /platform: linux\/arm64, runner: ubuntu-24\.04-arm/);
    assert.match(smoke, /release\.mjs verify-image vyrtel:release-candidate --version "\$\{\{ needs\.preflight\.outputs\.version \}\}" --platform "\$\{\{ matrix\.platform \}\}" --bind-mount on --record tested/);
    assert.match(smoke, /VYRTEL_VERSION=\$\{\{ needs\.preflight\.outputs\.version \}\}/);
    assert.match(jobBody(workflow, 'image'), /needs: \[preflight, image-smoke\]/);
  });

  test('each platform pushes by digest only after its smoke test, then proves the digest is the tested image', () => {
    const smoke = jobBody(workflow, 'image-smoke');
    const tested = smoke.indexOf('verify-image vyrtel:release-candidate');
    const push = smoke.indexOf('push-by-digest=true');
    const check = smoke.indexOf('verify-pushed-image');
    assert.ok(tested >= 0 && push > tested && check > push, 'smoke test, then push, then check');
    assert.doesNotMatch(smoke, /\n {10}tags:[^\n]*image_tags/, 'no release tag is pushed per platform');
    assert.match(smoke, /verify-pushed-image "\$\{\{ needs\.preflight\.outputs\.image_repo \}\}@\$\{\{ steps\.push\.outputs\.digest \}\}" [^\n]*--tested tested/);
    // The tested and pushed builds use the same labels and build arguments.
    assert.equal((smoke.match(/labels: \$\{\{ steps\.meta\.outputs\.labels \}\}/g) ?? []).length, 2);
    assert.equal((smoke.match(/build-args: VYRTEL_VERSION=\$\{\{ needs\.preflight\.outputs\.version \}\}/g) ?? []).length, 2);
  });

  test('the release tags point at the tested digests without a rebuild, and are checked against them', () => {
    const image = jobBody(workflow, 'image');
    assert.doesNotMatch(image, /build-push-action/, 'image does not build');
    assert.match(image, /pattern: image-tested-\*/);
    assert.match(image, /for f in tested\/\*\.digest; do args\+=\("\$REPO@\$\(cat "\$f"\)"\); done/);
    assert.match(image, new RegExp(`-eq ${PLATFORMS.length} \\]`), 'one tested digest per platform');
    assert.match(image, /docker buildx imagetools create "\$\{args\[@\]\}"/);
    assert.match(image, /if: env\.PUBLISH == 'true'\n {8}run: node scripts\/release\/release\.mjs verify-pushed-image "\$\{\{ needs\.preflight\.outputs\.image_ref \}\}" [^\n]*--tested tested/);
  });

  test('CI smoke-tests the image through the same gate', () => {
    const ci = jobBody(repoFile('.github/workflows/ci.yml'), 'docker');
    assert.match(ci, /release\.mjs verify-image vyrtel:ci .*--bind-mount on/);
  });

  test('the pushed manifest must carry every platform, each non-root and labelled', () => {
    const oci = (arch, label = '0.1.2') => {
      return { os: 'linux', architecture: arch, config: ociConfig({ label, arch }) };
    };
    const both = { 'linux/amd64': oci('amd64'), 'linux/arm64': oci('arm64') };
    assert.deepEqual(checkManifest(both, '0.1.2').map((c) => c.platform), ['linux/amd64', 'linux/arm64']);
    assert.throws(() => checkManifest({ 'linux/amd64': oci('amd64') }, '0.1.2'), gateError(/no linux\/arm64 variant \(has linux\/amd64\)/));
    assert.throws(() => checkManifest({ ...both, 'linux/arm64': oci('arm64', 'dev') }, '0.1.2'), gateError(/is "dev", expected "0\.1\.2"/));
  });

  test('the published image must have exactly the tested layers on every tested platform', () => {
    const layers = { 'linux/amd64': ['sha256:a1', 'sha256:a2'], 'linux/arm64': ['sha256:b1', 'sha256:b2'] };
    const oci = (arch, diffIds) => ({ os: 'linux', architecture: arch, config: ociConfig({ arch }), rootfs: { type: 'layers', diff_ids: diffIds } });
    const published = { 'linux/amd64': oci('amd64', layers['linux/amd64']), 'linux/arm64': oci('arm64', layers['linux/arm64']) };
    const tested = Object.fromEntries(Object.entries(layers).map(([platform, diffIds]) => [platform, { platform, diffIds }]));
    assert.ok(checkManifest(published, '0.1.2', PLATFORMS, tested).every((c) => c.sameAsTested));

    const rebuilt = { ...published, 'linux/arm64': oci('arm64', ['sha256:b1', 'sha256:different']) };
    assert.throws(() => checkManifest(rebuilt, '0.1.2', PLATFORMS, tested), gateError(/pushed linux\/arm64 image is not the one that was smoke-tested/));
    const extraLayer = { ...published, 'linux/amd64': oci('amd64', [...layers['linux/amd64'], 'sha256:a3']) };
    assert.throws(() => checkManifest(extraLayer, '0.1.2', PLATFORMS, tested), gateError(/linux\/amd64 image is not the one/));
  });

  test('a single-platform push inspects as one config; it is keyed by its platform', () => {
    const single = { os: 'linux', architecture: 'arm64', config: ociConfig({ arch: 'arm64' }), rootfs: { diff_ids: ['sha256:b1'] } };
    assert.deepEqual(Object.keys(imagesByPlatform(single)), ['linux/arm64']);
    const map = { 'linux/amd64': single };
    assert.equal(imagesByPlatform(map), map);
    const r = testedRecord({ Id: 'sha256:img', RootFS: { Layers: ['sha256:b1'] } }, 'linux/arm64');
    assert.deepEqual(r, { platform: 'linux/arm64', imageId: 'sha256:img', diffIds: ['sha256:b1'] });
    assert.equal(recordName('linux/arm64'), 'linux-arm64.json');
    assert.throws(() => testedRecord({ RootFS: { Layers: [] } }, 'linux/amd64'), gateError(/no layers/));
  });

  test('the host platform maps to Docker names', () => {
    assert.equal(hostPlatform('x64'), 'linux/amd64');
    assert.equal(hostPlatform('arm64'), 'linux/arm64');
  });
});

test('real image (set VYRTEL_IMAGE; needs Docker)', { skip: !process.env.VYRTEL_IMAGE && 'VYRTEL_IMAGE not set', timeout: 300_000 }, async () => {
  const lines = await verifyImage(process.env.VYRTEL_IMAGE, process.env.VYRTEL_IMAGE_VERSION ?? workspaceVersion(), {
    platform: process.env.VYRTEL_IMAGE_PLATFORM ?? hostPlatform(),
    log: () => {},
  });
  assert.ok(lines.some((l) => l.startsWith('persist: 3 events')), lines.join('\n'));
});
