import { screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { expect, it, vi } from 'vitest';

import { SettingsPage } from '../../src/pages/SettingsPage';
import { mockFetch, renderWithRouter } from './helpers';

it('shows whether ingestion needs a key, and updates it when keys change', async () => {
  let keys = [{ id: 1, name: 'automation', prefix: 'vyr_aaaaaaaa', scope: 'admin', createdAt: 0, lastUsedAt: null }];
  const listed = () => ({ apiKeys: keys, ingestKeyRequired: keys.some((k) => k.scope === 'ingest') });
  mockFetch({
    '/api/v1/api-keys': (body) => {
      if (!body) return listed();
      const k = { id: 2, name: 'ci', prefix: 'vyr_bbbbbbbb', scope: 'ingest', createdAt: 0, lastUsedAt: null };
      keys = [...keys, k];
      return { ...k, key: 'vyr_secret' };
    },
    '/api/v1/api-keys/2': () => {
      keys = keys.filter((k) => k.id !== 2);
      return new Response(null, { status: 204 });
    },
  });
  vi.spyOn(window, 'confirm').mockReturnValue(true);
  renderWithRouter(<SettingsPage />, '/settings?tab=keys');

  expect(await screen.findByTestId('ingest-protection')).toHaveTextContent(/Ingestion is open/);

  await userEvent.type(screen.getByLabelText('Key name'), 'ci');
  await userEvent.click(screen.getByRole('button', { name: 'Create key' }));
  await waitFor(() => expect(screen.getByTestId('ingest-protection')).toHaveTextContent(/requires an API key/));

  await userEvent.click(screen.getAllByRole('button', { name: 'Revoke' })[1]);
  await waitFor(() => expect(screen.getByTestId('ingest-protection')).toHaveTextContent(/Ingestion is open/));
});
