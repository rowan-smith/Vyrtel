import type { FullConfig } from '@playwright/test';
import { request } from '@playwright/test';

import { E2E_PASSWORD, STORAGE_STATE } from '../../playwright.config';

/** Sign in once; every spec's `page` and `request` reuse the session cookie. */
export default async function globalSetup(config: FullConfig) {
  const baseURL = config.projects[0].use.baseURL;
  const ctx = await request.newContext({ baseURL });
  const r = await ctx.post('/api/v1/auth/login', { data: { username: 'admin', password: E2E_PASSWORD } });
  if (!r.ok()) throw new Error(`e2e login failed: ${r.status()} ${await r.text()}`);
  await ctx.storageState({ path: STORAGE_STATE });
  await ctx.dispose();
}
