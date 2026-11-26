import { expect, test } from '@playwright/test';

import { E2E_PASSWORD } from '../../playwright.config';

// IAM-02 (#77): without a session every route shows the sign-in form, and the API
// refuses on its own: the browser's routing is not what protects the data.
test.use({ storageState: { cookies: [], origins: [] } });

test('every route needs a login, and the server refuses without one', async ({ page, request }) => {
  for (const path of ['/', '/logs', '/traces', '/metrics', '/dashboards', '/alerts', '/settings']) {
    await page.goto(path);
    await expect(page.getByRole('button', { name: 'Sign in' }), path).toBeVisible();
    await expect(page.getByRole('navigation', { name: 'Main' }), path).toHaveCount(0);
  }

  // Independent of the UI: query and admin endpoints refuse an unauthenticated client.
  expect((await request.post('/api/v1/query/logs', { data: { query: '' } })).status()).toBe(401);
  expect((await request.get('/api/v1/api-keys')).status()).toBe(401);
  expect((await request.get('/api/v1/system/config')).status()).toBe(401);

  // A wrong password keeps the form, with an error and no session.
  await page.goto('/settings');
  await page.getByLabel('Password').fill('admin');
  await page.getByRole('button', { name: 'Sign in' }).click();
  await expect(page.getByRole('alert')).toHaveText('invalid username or password');
  await expect(page.getByRole('navigation', { name: 'Main' })).toHaveCount(0);

  // The right one opens the route asked for, with no API key involved.
  await page.getByLabel('Password').fill(E2E_PASSWORD);
  await page.getByRole('button', { name: 'Sign in' }).click();
  await expect(page.getByRole('navigation', { name: 'Main' })).toBeVisible();
  await expect(page).toHaveURL(/\/settings$/);

  // Signing out returns to the form.
  await page.getByRole('button', { name: /Sign out/ }).click();
  await expect(page.getByRole('button', { name: 'Sign in' })).toBeVisible();
  await page.goto('/logs');
  await expect(page.getByRole('button', { name: 'Sign in' })).toBeVisible();
});
