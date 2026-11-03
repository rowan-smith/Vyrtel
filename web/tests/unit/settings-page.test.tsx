import { screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { expect, it, vi } from 'vitest';

import type { ApiKey, IngestAuth } from '../../src/lib/types';
import { ingestNotice, ingestSummary, revokePrompt, SettingsPage } from '../../src/pages/SettingsPage';
import { mockFetch, renderWithRouter } from './helpers';

type Mode = IngestAuth['mode'];

/** What the server reports for `keys` under `mode`, latching like it does. */
function server(mode: Mode, initial: ApiKey[]) {
  let keys = initial;
  let latched = keys.some((k) => k.scope === 'ingest');
  const ingestAuth = (): IngestAuth => {
    const ingestKeys = keys.filter((k) => k.scope === 'ingest').length;
    const prot = mode === 'required' || (mode === 'automatic' && latched);
    return {
      mode,
      effective: prot ? 'protected' : 'open',
      latched,
      ingestKeys,
      protectedWithoutKeys: prot && ingestKeys === 0,
    };
  };
  return {
    list: () => ({
      apiKeys: keys,
      ingestKeyRequired: ingestAuth().effective === 'protected',
      ingestAuth: ingestAuth(),
    }),
    add: (k: ApiKey) => {
      keys = [...keys, k];
      latched ||= k.scope === 'ingest';
    },
    remove: (id: number) => {
      keys = keys.filter((k) => k.id !== id);
    },
  };
}

const key = (id: number, name: string, scope: 'ingest' | 'admin'): ApiKey => ({
  id,
  name,
  prefix: `vyr_${String(id).repeat(8)}`,
  scope,
  createdAt: 0,
  lastUsedAt: null,
});

it('shows ingestion protected for good once an ingest key exists, and warns before the last revoke', async () => {
  const s = server('automatic', [key(1, 'automation', 'admin')]);
  mockFetch({
    '/api/v1/api-keys': (body) => {
      if (!body) return s.list();
      const k = key(2, 'ci', 'ingest');
      s.add(k);
      return { ...k, key: 'vyr_secret' };
    },
    '/api/v1/api-keys/2': () => {
      s.remove(2);
      return new Response(null, { status: 204 });
    },
  });
  const confirm = vi.spyOn(window, 'confirm').mockReturnValue(true);
  renderWithRouter(<SettingsPage />, '/settings?tab=keys');

  expect(await screen.findByTestId('ingest-protection')).toHaveTextContent(/Ingestion is open/);

  await userEvent.type(screen.getByLabelText('Key name'), 'ci');
  await userEvent.click(screen.getByRole('button', { name: 'Create key' }));
  await waitFor(() => expect(screen.getByTestId('ingest-protection')).toHaveTextContent(/requires an API key/));
  expect(screen.getByTestId('ingest-protection')).toHaveTextContent(/stays protected when keys are revoked/);

  await userEvent.click(screen.getAllByRole('button', { name: 'Revoke' })[1]);
  expect(confirm).toHaveBeenLastCalledWith(expect.stringMatching(/last ingest key\. Ingestion stays protected/));
  await waitFor(() => expect(screen.getByTestId('ingest-protection')).toHaveTextContent(/no ingest key exists/));
  expect(screen.getByTestId('ingest-protection')).toHaveTextContent(/Create a replacement ingest key/);
  expect(screen.getByTestId('ingest-protection')).not.toHaveTextContent(/Ingestion is open/);
});

it('revokes nothing when the confirmation is cancelled, and shows a failed revoke', async () => {
  const s = server('automatic', [key(2, 'ci', 'ingest')]);
  const revoked = vi.fn(
    () => new Response(JSON.stringify({ error: { code: 'unavailable', message: 'busy' } }), { status: 503 }),
  );
  mockFetch({ '/api/v1/api-keys': () => s.list(), '/api/v1/api-keys/2': revoked });
  const confirm = vi.spyOn(window, 'confirm').mockReturnValue(false);
  renderWithRouter(<SettingsPage />, '/settings?tab=keys');

  await userEvent.click(await screen.findByRole('button', { name: 'Revoke' }));
  expect(revoked).not.toHaveBeenCalled();

  confirm.mockReturnValue(true);
  await userEvent.click(screen.getByRole('button', { name: 'Revoke' }));
  await waitFor(() => expect(revoked).toHaveBeenCalled());
  expect(await screen.findByText(/busy/)).toBeInTheDocument();
  expect(screen.getByText('ci')).toBeInTheDocument();
});

it('describes every ingestion state', () => {
  const a = (over: Partial<IngestAuth>): IngestAuth => ({
    mode: 'automatic',
    effective: 'open',
    latched: false,
    ingestKeys: 0,
    protectedWithoutKeys: false,
    ...over,
  });
  const fresh = a({});
  const latched = a({ effective: 'protected', latched: true, ingestKeys: 2 });
  const keyless = a({ effective: 'protected', latched: true, protectedWithoutKeys: true });
  const open = a({ mode: 'open', latched: true });
  const required = a({ mode: 'required', effective: 'protected', protectedWithoutKeys: true });

  expect(ingestSummary(fresh)).toBe('open (no ingest key yet)');
  expect(ingestSummary(latched)).toBe('API key required');
  expect(ingestSummary(keyless)).toBe('API key required — no ingest key exists');
  expect(ingestSummary(required)).toBe('API key required — no ingest key exists');
  expect(ingestSummary(open)).toBe('open (ingest.auth_mode = "open")');

  expect(ingestNotice(open)).toMatch(/back to "automatic" protects it again/);
  expect(ingestNotice(a({ mode: 'open' }))).not.toMatch(/protects it again/);
  for (const state of [fresh, latched, keyless, open, required]) {
    expect(ingestNotice(state)).not.toMatch(/opens it again|open again/);
  }

  const ci = key(2, 'ci', 'ingest');
  expect(revokePrompt(ci, { ...latched, ingestKeys: 1 })).toMatch(/last ingest key/);
  expect(revokePrompt(ci, latched)).toBe('Revoke key "ci"?');
  expect(revokePrompt(key(1, 'automation', 'admin'), { ...latched, ingestKeys: 1 })).toBe('Revoke key "automation"?');
  expect(revokePrompt(ci, { ...open, ingestKeys: 1 })).toBe('Revoke key "ci"?');
});
