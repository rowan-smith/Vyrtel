import { screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { expect, it, vi } from 'vitest';

import type { ApiKey, IngestAuth } from '../../src/lib/types';
import { ingestNotice, ingestSummary, modePrompt, revokePrompt, SettingsPage } from '../../src/pages/SettingsPage';
import { mockFetch, renderWithRouter } from './helpers';

type Mode = IngestAuth['mode'];

/** What the server reports for `keys` under `mode`, latching like it does. */
function server(initialMode: Mode, initial: ApiKey[]) {
  let keys = initial;
  let mode = initialMode;
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
      ingestAuth: ingestAuth(),
    }),
    add: (k: ApiKey) => {
      keys = [...keys, k];
      latched ||= k.scope === 'ingest';
    },
    remove: (id: number) => {
      keys = keys.filter((k) => k.id !== id);
    },
    setMode: (m: Mode) => {
      mode = m;
      return { ingestAuth: ingestAuth() };
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
  expect(ingestSummary(open)).toBe('open (mode "open")');

  expect(ingestNotice(open)).toMatch(/Switching back to "automatic" protects it again/);
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

it('changes the ingestion mode from Settings, confirming before it opens ingestion', async () => {
  const s = server('automatic', [key(2, 'ci', 'ingest')]);
  const calls = mockFetch({
    '/api/v1/api-keys': () => s.list(),
    '/api/v1/ingest-auth': (body) => s.setMode((body as { mode: Mode }).mode),
  });
  const confirm = vi.spyOn(window, 'confirm').mockReturnValue(false);
  renderWithRouter(<SettingsPage />, '/settings?tab=keys');
  const select = await screen.findByLabelText('Ingestion mode');
  expect(select).toHaveValue('automatic');

  // Cancelled: nothing is sent.
  await userEvent.selectOptions(select, 'open');
  expect(confirm).toHaveBeenLastCalledWith(expect.stringMatching(/^Open ingestion\?/));
  expect(calls.some((c) => c.url.endsWith('/api/v1/ingest-auth'))).toBe(false);
  expect(select).toHaveValue('automatic');

  confirm.mockReturnValue(true);
  await userEvent.selectOptions(select, 'open');
  await waitFor(() =>
    expect(screen.getByTestId('ingest-protection')).toHaveTextContent(/Ingestion is open \(mode "open"\)/),
  );
  expect(calls.find((c) => c.url.endsWith('/api/v1/ingest-auth'))?.body).toEqual({ mode: 'open' });
  expect(select).toHaveValue('open');

  // Back to automatic: protected again by the latch, after confirming.
  await userEvent.selectOptions(select, 'automatic');
  expect(confirm).toHaveBeenLastCalledWith(expect.stringMatching(/^Require API keys/));
  await waitFor(() => expect(screen.getByTestId('ingest-protection')).toHaveTextContent(/requires an API key/));
});

it('keeps the mode and shows the error when changing it fails', async () => {
  const s = server('automatic', []);
  mockFetch({
    '/api/v1/api-keys': () => s.list(),
    '/api/v1/ingest-auth': () =>
      new Response(JSON.stringify({ error: { code: 'unavailable', message: 'too many changes' } }), { status: 503 }),
  });
  vi.spyOn(window, 'confirm').mockReturnValue(true);
  renderWithRouter(<SettingsPage />, '/settings?tab=keys');
  const select = await screen.findByLabelText('Ingestion mode');
  await userEvent.selectOptions(select, 'required');
  expect(await screen.findByText(/too many changes/)).toBeInTheDocument();
  expect(select).toHaveValue('automatic');
  expect(select).not.toBeDisabled();
});

it('asks before each change that widens or narrows who can send', () => {
  const base: IngestAuth = {
    mode: 'automatic',
    effective: 'open',
    latched: false,
    ingestKeys: 0,
    protectedWithoutKeys: false,
  };
  expect(modePrompt('open', base)).toMatch(/Open ingestion/);
  expect(modePrompt('required', base)).toMatch(/Require API keys/);
  // Required → automatic on an unlatched store opens ingestion: it asks.
  const required: IngestAuth = { ...base, mode: 'required', effective: 'protected', protectedWithoutKeys: true };
  expect(modePrompt('automatic', required)).toMatch(/^Open ingestion\?.*until an ingest key is created/);
  // ...but not when the latch keeps it protected.
  expect(modePrompt('automatic', { ...required, latched: true })).toBeNull();
  expect(modePrompt('required', { ...required, mode: 'automatic', latched: true })).toBeNull();
  expect(modePrompt('automatic', { ...base, mode: 'open', latched: true })).toMatch(/Require API keys/);
  expect(modePrompt('automatic', { ...base, mode: 'open' })).toBeNull();
  expect(modePrompt('open', { ...required, latched: true })).toMatch(/whatever keys exist/);
  // Every pair of modes, latched or not: a change that ends protection
  // always asks.
  const modes = ['automatic', 'required', 'open'] as const;
  const protects = (m: Mode, latched: boolean) => m === 'required' || (m === 'automatic' && latched);
  for (const latched of [false, true]) {
    for (const from of modes) {
      for (const to of modes) {
        if (from === to) continue;
        const state: IngestAuth = {
          ...base,
          mode: from,
          latched,
          effective: protects(from, latched) ? 'protected' : 'open',
        };
        if (protects(from, latched) && !protects(to, latched)) {
          expect(modePrompt(to, state), `${from} → ${to}, latched=${latched}`).toMatch(/^Open ingestion\?/);
        }
      }
    }
  }
});

it('confirms before Required → Automatic opens an unlatched install, and changes nothing when cancelled', async () => {
  const s = server('required', []);
  const calls = mockFetch({
    '/api/v1/api-keys': () => s.list(),
    '/api/v1/ingest-auth': (body) => s.setMode((body as { mode: Mode }).mode),
  });
  const confirm = vi.spyOn(window, 'confirm').mockReturnValue(false);
  renderWithRouter(<SettingsPage />, '/settings?tab=keys');
  const select = await screen.findByLabelText('Ingestion mode');
  expect(select).toHaveValue('required');

  await userEvent.selectOptions(select, 'automatic');
  expect(confirm).toHaveBeenLastCalledWith(expect.stringMatching(/^Open ingestion\?/));
  expect(calls.some((c) => c.url.endsWith('/api/v1/ingest-auth'))).toBe(false);
  expect(select).toHaveValue('required');
  expect(screen.getByTestId('ingest-protection')).toHaveTextContent(/requires an API key/);

  confirm.mockReturnValue(true);
  await userEvent.selectOptions(select, 'automatic');
  await waitFor(() => expect(screen.getByTestId('ingest-protection')).toHaveTextContent(/Ingestion is open/));
  expect(calls.find((c) => c.url.endsWith('/api/v1/ingest-auth'))?.body).toEqual({ mode: 'automatic' });
  expect(select).toHaveValue('automatic');
});
