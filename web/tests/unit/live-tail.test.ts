import { act, renderHook } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { RECONNECT_MAX_MS, RECONNECT_MIN_MS, useLiveTail } from '../../src/lib/useLiveTail';
import { FakeEventSource, logEvent } from './helpers';

const CONNECTING = 0;

function setup() {
  const onEvents = vi.fn();
  const hook = renderHook(() => useLiveTail({ enabled: true, query: 'level = Error', onEvents }));
  return { ...hook, onEvents };
}

/** The browser's own reconnect after a stream ends (readyState CONNECTING). */
function streamEnded(es: FakeEventSource) {
  es.readyState = CONNECTING;
  act(() => es.onerror?.());
}

/** EventSource gave up for good, e.g. a 503 while the server restarts. */
function gaveUp(es: FakeEventSource) {
  es.readyState = FakeEventSource.CLOSED;
  act(() => es.onerror?.());
}

beforeEach(() => {
  FakeEventSource.instances = [];
  vi.stubGlobal('EventSource', FakeEventSource);
  vi.useFakeTimers();
});

afterEach(() => {
  vi.useRealTimers();
  vi.unstubAllGlobals();
});

// REL-05 AC4: a server shutdown ends the stream; the client shows it is
// reconnecting and is live again once the restarted server answers.
describe('useLiveTail across a server restart', () => {
  it('delivers buffered events, shows reconnecting on shutdown and recovers on the same source', () => {
    const { result, onEvents } = setup();
    const es = FakeEventSource.latest()!;
    expect(es.url).toBe('/api/v1/live/logs?query=level+%3D+Error');
    expect(result.current.status).toBe('connecting');
    act(() => es.emit('ready', {}));
    expect(result.current.status).toBe('live');

    act(() => {
      es.emit('event', logEvent({ id: 'a', message: 'before shutdown' }));
      es.emit('shutdown', {});
    });
    expect(result.current.status).toBe('connecting');
    act(() => vi.advanceTimersByTime(250));
    expect(onEvents).toHaveBeenCalledWith([expect.objectContaining({ message: 'before shutdown' })]);

    streamEnded(es);
    expect(result.current.status).toBe('connecting');
    expect(FakeEventSource.instances).toHaveLength(1);
    act(() => es.emit('ready', {}));
    expect(result.current.status).toBe('live');
  });

  it('reopens with capped exponential backoff after EventSource gives up', () => {
    const { result } = setup();
    gaveUp(FakeEventSource.latest()!);
    expect(result.current.status).toBe('error');
    expect(FakeEventSource.latest()!.closed).toBe(true);

    // Waits RECONNECT_MIN_MS, then doubles up to the cap.
    let delay = RECONNECT_MIN_MS;
    for (let attempt = 2; attempt <= 8; attempt++) {
      act(() => vi.advanceTimersByTime(delay - 1));
      expect(FakeEventSource.instances).toHaveLength(attempt - 1);
      act(() => vi.advanceTimersByTime(1));
      expect(FakeEventSource.instances).toHaveLength(attempt);
      expect(result.current.status).toBe('connecting');
      gaveUp(FakeEventSource.latest()!);
      delay = Math.min(delay * 2, RECONNECT_MAX_MS);
    }
    expect(delay).toBe(RECONNECT_MAX_MS);

    // Success resets the backoff.
    act(() => vi.advanceTimersByTime(delay));
    const es = FakeEventSource.latest()!;
    act(() => es.emit('ready', {}));
    expect(result.current.status).toBe('live');
    gaveUp(es);
    const count = FakeEventSource.instances.length;
    act(() => vi.advanceTimersByTime(RECONNECT_MIN_MS));
    expect(FakeEventSource.instances).toHaveLength(count + 1);
  });

  it('stops retrying once unmounted', () => {
    const { unmount } = setup();
    gaveUp(FakeEventSource.latest()!);
    unmount();
    act(() => vi.advanceTimersByTime(RECONNECT_MAX_MS));
    expect(FakeEventSource.instances).toHaveLength(1);
  });
});
