import { render } from '@testing-library/react';
import type { ReactElement } from 'react';
import { vi } from 'vitest';

import { RouterProvider } from '../../src/lib/router';
import type { TelemetryEvent } from '../../src/lib/types';

export function renderWithRouter(ui: ReactElement, path = '/') {
  window.history.replaceState(null, '', path);
  return render(ui, { wrapper: RouterProvider });
}

export function logEvent(overrides: Partial<TelemetryEvent> = {}): TelemetryEvent {
  return {
    id: '0000000000000001',
    signal: 'log',
    timestamp: '2026-10-06T10:42:17.231Z',
    observedTimestamp: '2026-10-06T10:42:17.300Z',
    service: 'payments',
    environment: 'production',
    traceId: '4bf92f3577b34da6a3ce929d0e0e4736',
    spanId: '00f067aa0ba902b7',
    level: 'Error',
    message: 'Payment provider timed out',
    properties: { customerId: 481, paymentProvider: 'stripe', http: { statusCode: 504, method: 'POST' }, tags: ['a', 'b'], note: null },
    resource: { 'host.name': 'web-1' },
    exception: {
      type: 'TimeoutException',
      message: 'Payment provider timed out',
      stackTrace: 'TimeoutException: Payment provider timed out\n   at Payments.Charge()\n   at Payments.Pay()',
    },
    ...overrides,
  };
}

/** Minimal controllable EventSource for live-tail tests. */
export class FakeEventSource {
  static instances: FakeEventSource[] = [];
  url: string;
  readyState = 1;
  onerror: (() => void) | null = null;
  closed = false;
  private listeners = new Map<string, ((e: MessageEvent) => void)[]>();

  constructor(url: string) {
    this.url = url;
    FakeEventSource.instances.push(this);
  }

  addEventListener(type: string, fn: (e: MessageEvent) => void) {
    this.listeners.set(type, [...(this.listeners.get(type) ?? []), fn]);
  }

  emit(type: string, data: unknown) {
    for (const fn of this.listeners.get(type) ?? []) fn(new MessageEvent(type, { data: typeof data === 'string' ? data : JSON.stringify(data) }));
  }

  close() {
    this.closed = true;
    this.readyState = 2;
  }

  static CLOSED = 2;
  static latest(): FakeEventSource | undefined {
    return FakeEventSource.instances[FakeEventSource.instances.length - 1];
  }
}

type Handler = (body: unknown, url: string) => unknown;

/** Route fetch calls by path; unknown paths return 404. */
export function mockFetch(routes: Record<string, Handler>) {
  const calls: { url: string; body: unknown }[] = [];
  const fn = vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
    const url = String(input);
    const body = init?.body ? JSON.parse(String(init.body)) : undefined;
    calls.push({ url, body });
    const path = url.split('?')[0];
    const h = routes[path];
    if (!h) return new Response(JSON.stringify({ error: { code: 'not_found', message: 'not found' } }), { status: 404 });
    const out = h(body, url);
    if (out instanceof Response) return out;
    return new Response(JSON.stringify(out), { status: 200, headers: { 'content-type': 'application/json' } });
  });
  vi.stubGlobal('fetch', fn);
  return calls;
}
