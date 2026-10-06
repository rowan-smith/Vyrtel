import { expect, test } from '@playwright/test';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

import { loadFixture } from './fixtures';

// Regenerates docs/screenshots/*.png. Skipped unless SCREENSHOTS=1:
//   SCREENSHOTS=1 npx playwright test screenshots
const out = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..', '..', '..', 'docs', 'screenshots');

test.skip(!process.env.SCREENSHOTS, 'set SCREENSHOTS=1 to regenerate documentation screenshots');

test('documentation screenshots', async ({ page, request }) => {
  await page.setViewportSize({ width: 1400, height: 860 });
  await page.emulateMedia({ colorScheme: 'dark' });
  await request.post('/api/v1/events', { headers: { 'content-type': 'application/x-ndjson' }, data: loadFixture('logs.ndjson') });
  await request.post('/v1/traces', { headers: { 'content-type': 'application/json' }, data: loadFixture('otlp-traces.json') });
  await request.post('/v1/logs', { headers: { 'content-type': 'application/json' }, data: loadFixture('otlp-logs.json') });
  await request.post('/v1/metrics', { headers: { 'content-type': 'application/json' }, data: loadFixture('otlp-metrics.json') });

  await page.goto('/logs');
  const row = page.getByTestId('log-row').filter({ hasText: 'Payment provider timed out' });
  await row.getByRole('button').first().click();
  await page.getByRole('button', { name: /results/ }).click();
  await expect(page.getByTestId('diagnostics')).toBeVisible();
  await page.screenshot({ path: path.join(out, 'logs.png') });

  await page.goto('/traces/4bf92f3577b34da6a3ce929d0e0e4736');
  await page.getByTestId('span-row').nth(1).getByRole('button').click();
  await page.screenshot({ path: path.join(out, 'trace.png') });

  await page.goto('/settings');
  await expect(page.getByTestId('storage-stats')).toBeVisible();
  await page.screenshot({ path: path.join(out, 'storage.png') });
});
