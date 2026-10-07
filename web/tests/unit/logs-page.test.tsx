import { act, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { LogsPage } from '../../src/pages/LogsPage';
import { FakeEventSource, logEvent, mockFetch, renderWithRouter } from './helpers';

const diag = {
  elapsedMs: 1.5,
  segmentsConsidered: 23,
  segmentsSkipped: 19,
  segmentsSkippedByTime: 10,
  segmentsSkippedByIndex: 9,
  segmentsNotNeeded: 0,
  blocksConsidered: 40,
  blocksSkipped: 23,
  blocksRead: 17,
  eventsExamined: 4812,
  eventsMatched: 2,
  bytesRead: 1843210,
  unsealedEventsScanned: 0,
  indexes: [
    { field: 'timestamp', kind: 'time' },
    { field: 'level', kind: 'bitmap' },
    { field: 'customerId', kind: 'none' },
  ],
};

function setup(error?: boolean, logCount = 2) {
  const first = logEvent({ id: 'a1', message: 'first event' });
  const second = logEvent({ id: 'a2', message: 'second event', level: 'Information', exception: undefined });
  const calls = mockFetch({
    '/api/v1/system/info': () => ({ eventCounts: { logs: logCount, traces: 0, metrics: 0 } }),
    '/api/v1/query/logs': (body) => {
      if (error) {
        return new Response(JSON.stringify({ error: { code: 'invalid_query', message: "Expected a value after '='", position: 8 } }), { status: 400 });
      }
      const q = (body as { query: string }).query;
      return { events: q.includes('nothing') ? [] : [first, second], diagnostics: diag, continuationToken: null };
    },
    '/api/v1/query/logs/histogram': () => ({ buckets: [{ start: '2026-10-06T10:00:00Z', count: 2, levels: [0, 0, 0, 1, 0, 1, 0] }], total: 2, stepMs: 60000 }),
    '/api/v1/query/logs/facets': () => ({
      sampled: 2,
      fields: [{ field: 'service', count: 2, truncated: false, values: [{ value: 'payments', count: 2 }] }],
    }),
  });
  return calls;
}

beforeEach(() => {
  FakeEventSource.instances = [];
  vi.stubGlobal('EventSource', FakeEventSource);
  try {
    localStorage.clear();
  } catch {
    /* ignore */
  }
});

afterEach(() => {
  vi.useRealTimers();
});

describe('LogsPage', () => {
  it('runs the query from the URL and renders rows and diagnostics', async () => {
    const calls = setup();
    renderWithRouter(<LogsPage />, '/logs?q=level%20%3D%20Error');
    expect(await screen.findByText('first event')).toBeInTheDocument();
    expect(screen.getByText('second event')).toBeInTheDocument();
    const search = calls.find((c) => c.url === '/api/v1/query/logs');
    expect((search?.body as { query: string }).query).toBe('level = Error');

    await userEvent.click(screen.getByRole('button', { name: /results/ }));
    const d = screen.getByTestId('diagnostics');
    expect(d).toHaveTextContent('Segments considered23');
    expect(d).toHaveTextContent('Blocks read17');
    expect(d).toHaveTextContent('1.8 MB');
    expect(d).toHaveTextContent('level');
    expect(d).toHaveTextContent('no index (scanned)');
  });

  it('expands a row on click', async () => {
    setup();
    renderWithRouter(<LogsPage />, '/logs');
    await userEvent.click(await screen.findByRole('button', { name: /first event/ }));
    const details = screen.getByTestId('log-details');
    expect(within(details).getByTestId('prop-customerId')).toHaveTextContent('481');
  });

  it('adds a filter from the filter panel to the query', async () => {
    const calls = setup();
    renderWithRouter(<LogsPage />, '/logs?q=level%20%3D%20Error');
    await screen.findByText('first event');
    const panel = await screen.findByRole('complementary', { name: 'Filters' });
    await userEvent.click(await within(panel).findByRole('button', { name: /payments/ }));
    await waitFor(() => expect(new URLSearchParams(window.location.search).get('q')).toBe('level = Error and service = "payments"'));
    await waitFor(() =>
      expect(calls.some((c) => c.url === '/api/v1/query/logs' && (c.body as { query: string }).query === 'level = Error and service = "payments"')).toBe(true),
    );
  });

  it('shows query errors', async () => {
    setup(true);
    renderWithRouter(<LogsPage />, '/logs?q=level%20%3D');
    expect(await screen.findByRole('alert')).toHaveTextContent("Expected a value after '='");
  });

  it('live mode streams new events; paused mode keeps results stationary', async () => {
    setup();
    renderWithRouter(<LogsPage />, '/logs');
    await screen.findByText('first event');
    expect(FakeEventSource.instances).toHaveLength(0);

    // Go live.
    await userEvent.click(screen.getByRole('button', { name: /^live$/i }));
    const es = FakeEventSource.latest()!;
    expect(es.url).toContain('/api/v1/live/logs');
    act(() => es.emit('ready', {}));
    expect(screen.getByRole('button', { name: /^live$/i })).toHaveAttribute('aria-pressed', 'true');
    act(() => es.emit('event', logEvent({ id: 'live1', message: 'arrived live' })));
    expect(await screen.findByText('arrived live')).toBeInTheDocument();
    // Newest first.
    const rows = screen.getAllByTestId('log-row');
    expect(rows[0]).toHaveTextContent('arrived live');

    // Pause: the stream closes, existing results stay.
    await userEvent.click(screen.getByRole('button', { name: /^live$/i }));
    expect(es.closed).toBe(true);
    act(() => es.emit('event', logEvent({ id: 'late', message: 'should not appear' })));
    await new Promise((r) => setTimeout(r, 400));
    expect(screen.queryByText('should not appear')).not.toBeInTheDocument();
    expect(screen.getByText('arrived live')).toBeInTheDocument();
    expect(screen.getByText('first event')).toBeInTheDocument();
  });

  it('shows only the "no logs yet" message when no logs exist at all', async () => {
    const calls = setup(false, 0);
    renderWithRouter(<LogsPage />, '/logs');
    expect(await screen.findByText('No logs yet')).toBeInTheDocument();
    // No query tools, and no searches against an empty store.
    expect(screen.queryByRole('textbox', { name: 'Query' })).not.toBeInTheDocument();
    expect(screen.queryByRole('button', { name: 'Live' })).not.toBeInTheDocument();
    expect(calls.some((c) => c.url.startsWith('/api/v1/query/'))).toBe(false);
  });

  it('shows only the empty state when nothing matches', async () => {
    setup();
    renderWithRouter(<LogsPage />, '/logs?q=message%20%3D%20%22nothing%22');
    expect(await screen.findByText('No matching events')).toBeInTheDocument();
    // No histogram band, "0 results" diagnostics line or empty filter panel alongside it.
    expect(screen.queryByRole('img', { name: 'Events over time' })).not.toBeInTheDocument();
    expect(screen.queryByTestId('diagnostics')).not.toBeInTheDocument();
    expect(screen.queryByRole('complementary', { name: 'Filters' })).not.toBeInTheDocument();
    // The query bar stays so the query or time range can be changed.
    expect(screen.getByRole('textbox', { name: 'Query' })).toBeInTheDocument();
  });
});
