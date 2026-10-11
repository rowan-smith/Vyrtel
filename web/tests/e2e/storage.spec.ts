import { expect, test } from '@playwright/test';
import fs from 'node:fs';
import path from 'node:path';

// REL-04 against the real binary: readiness and storage diagnostics while the
// data directory is writable, and while it really is not (no fault injection).
const dataDir = process.env.VYRTEL_E2E_DATA!;

test('readiness and storage diagnostics report a healthy write path', async ({ request }) => {
  // An event of its own: the shared fixtures would change counts other specs assert on.
  const ingest = await request.post('/api/v1/events', {
    data: { level: 'Information', message: 'E2E storage check', service: 'e2e' },
  });
  expect(ingest.ok()).toBeTruthy();

  const ready = await request.get('/ready');
  expect(ready.status()).toBe(200);
  expect(await ready.json()).toEqual({ status: 'ready', storageWritable: true });

  const storage = await (await request.get('/api/v1/system/storage')).json();
  expect(storage.writeProbe).toEqual({ writable: true, fault: null });
  for (const signal of ['logs', 'traces', 'metrics']) {
    expect(storage.signals[signal]).toMatchObject({ healthy: true, ingestHealthy: true, faults: [] });
  }
});

test('a read-only data directory makes readiness degraded until it is writable again', async ({ request }) => {
  // Windows has no mode bits, and root ignores them.
  test.skip(process.platform === 'win32', 'needs POSIX permissions');
  test.skip(process.getuid?.() === 0, 'root bypasses permissions');

  const tmp = path.join(dataDir, 'tmp');
  const marker = `E2E before read-only ${Date.now()}`;
  const before = await request.post('/api/v1/events', { data: { level: 'Information', message: marker, service: 'e2e' } });
  expect(before.ok()).toBeTruthy();

  fs.chmodSync(tmp, 0o555);
  try {
    // Readiness results are reused for up to a second.
    await expect
      .poll(async () => (await request.get('/ready')).status(), { timeout: 5_000 })
      .toBe(503);
    const body = await (await request.get('/ready')).json();
    // Unauthenticated: which signals, never why or where.
    expect(body).toEqual({ status: 'degraded', storageWritable: false, signals: ['logs', 'traces', 'metrics'] });

    const res = await request.get('/api/v1/system/storage');
    const text = await res.text();
    expect(text).not.toContain(dataDir);
    const probe = JSON.parse(text).writeProbe;
    expect(probe.writable).toBe(false);
    expect(probe.fault).toMatchObject({ operation: 'probe', reason: 'permissionDenied', path: 'tmp/.write-probe' });
  } finally {
    fs.chmodSync(tmp, 0o755);
  }

  await expect
    .poll(async () => (await request.get('/ready')).status(), { timeout: 5_000 })
    .toBe(200);
  // Data acknowledged before the fault is still there.
  const result = await request.post('/api/v1/query/logs', { data: { query: `message = "${marker}"`, limit: 10 } });
  expect(result.ok()).toBeTruthy();
  expect((await result.json()).events).toHaveLength(1);
});
