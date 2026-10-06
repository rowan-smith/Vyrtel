import { expect, test } from '@playwright/test';

import { loadFixture } from './fixtures';

test('search, inspect, live tail and trace navigation', async ({ page, request }) => {
  // Ingest fixture data: native NDJSON logs + OTLP spans sharing a trace id.
  const logs = await request.post('/api/v1/events', {
    headers: { 'content-type': 'application/x-ndjson' },
    data: loadFixture('logs.ndjson'),
  });
  expect(logs.ok()).toBeTruthy();
  const spans = await request.post('/v1/traces', {
    headers: { 'content-type': 'application/json' },
    data: loadFixture('otlp-traces.json'),
  });
  expect(spans.ok()).toBeTruthy();

  // Open Logs and search for errors.
  await page.goto('/logs');
  await expect(page.getByRole('link', { name: 'Logs' })).toBeVisible();
  const query = page.getByRole('textbox', { name: 'Query' });
  await query.fill('level = Error');
  await query.press('Enter');
  await expect(page).toHaveURL(/q=level/);

  const rows = page.getByTestId('log-row');
  await expect(rows).toHaveCount(2);
  await expect(rows.filter({ hasText: 'Payment provider timed out' })).toHaveCount(1);
  await expect(rows.filter({ hasText: 'User 481 logged in' })).toHaveCount(0);

  // Expand the event and check structured properties render vertically.
  await rows.filter({ hasText: 'Payment provider timed out' }).getByRole('button').first().click();
  const details = page.getByTestId('log-details');
  await expect(details.getByTestId('prop-customerId')).toContainText('481');
  await expect(details.getByTestId('prop-paymentProvider')).toContainText('stripe');
  await expect(details.getByTestId('prop-http.statusCode')).toContainText('504');
  await expect(details.getByTestId('stacktrace')).toContainText('StripeClient.Charge');

  // Paused: a newly ingested event must not appear.
  const live = page.getByRole('button', { name: /^(Paused|Live|Connecting…)$/ });
  await expect(live).toHaveText(/Paused/);
  await expect(live).toHaveAttribute('aria-pressed', 'false');
  await request.post('/api/v1/events', { data: { level: 'Error', message: 'E2E paused event', service: 'e2e' } });
  await page.waitForTimeout(1500);
  await expect(page.getByText('E2E paused event')).toHaveCount(0);
  await expect(rows).toHaveCount(2);

  // Live: new matching events appear automatically; non-matching do not.
  await live.click();
  await expect(live).toHaveAttribute('aria-pressed', 'true');
  await expect(live).toHaveText(/Live/);
  await request.post('/api/v1/events', {
    data: [
      { level: 'Error', message: 'E2E live event', service: 'e2e' },
      { level: 'Information', message: 'E2E info event', service: 'e2e' },
    ],
  });
  await expect(page.getByText('E2E live event')).toBeVisible();
  await expect(page.getByText('E2E info event')).toHaveCount(0);

  // Pause again: results stay where they are.
  await live.click();
  await expect(live).toHaveText(/Paused/);
  await expect(page.getByText('E2E live event')).toBeVisible();

  // Open the related trace from the log event.
  await details.getByRole('link', { name: 'View trace' }).click();
  await expect(page).toHaveURL(/\/traces\/4bf92f3577b34da6a3ce929d0e0e4736/);
  await expect(page.getByRole('heading', { name: /POST \/checkout/ })).toBeVisible();
  const spanRows = page.getByTestId('span-row');
  await expect(spanRows).toHaveCount(3);
  await expect(spanRows.nth(1)).toContainText('charge card');
  await expect(page.getByRole('region', { name: 'Related logs' })).toContainText('Payment provider timed out');
});

test('query errors are explained in the UI', async ({ page }) => {
  await page.goto('/logs');
  const query = page.getByRole('textbox', { name: 'Query' });
  await query.fill('level = "Error" and');
  await query.press('Enter');
  await expect(page.getByRole('alert')).toContainText("Expected expression after 'and'");
});
