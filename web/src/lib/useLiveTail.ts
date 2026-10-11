import { useEffect, useRef, useState } from 'react';

import type { TelemetryEvent } from './types';

export type LiveStatus = 'off' | 'connecting' | 'live' | 'error';

/** Backoff for reconnecting after EventSource gives up on its own. */
export const RECONNECT_MIN_MS = 1000;
export const RECONNECT_MAX_MS = 30_000;

/**
 * Subscribe to `/api/v1/live/logs` while `enabled`. Incoming events are
 * buffered and delivered at most every `flushMs`, so a burst of thousands
 * of events causes a handful of renders, not thousands.
 *
 * When the server shuts down it sends `shutdown` and ends the stream;
 * EventSource then reconnects by itself. If a reconnect is answered with an
 * HTTP error (a restarting server or proxy returns 503), EventSource stops
 * for good, so a new one is opened with capped exponential backoff.
 */
export function useLiveTail(opts: {
  enabled: boolean;
  query: string;
  onEvents: (events: TelemetryEvent[]) => void;
  flushMs?: number;
}): { status: LiveStatus; dropped: number } {
  const { enabled, query, flushMs = 250 } = opts;
  const [status, setStatus] = useState<LiveStatus>('off');
  const [dropped, setDropped] = useState(0);
  const onEvents = useRef(opts.onEvents);
  onEvents.current = opts.onEvents;

  useEffect(() => {
    if (!enabled) {
      setStatus('off');
      return;
    }
    const url = `/api/v1/live/logs?${new URLSearchParams({ query }).toString()}`;
    let source: EventSource | undefined;
    let retry: number | undefined;
    let delay = RECONNECT_MIN_MS;
    let buffer: TelemetryEvent[] = [];
    const timer = window.setInterval(() => {
      if (buffer.length) {
        const batch = buffer;
        buffer = [];
        onEvents.current(batch);
      }
    }, flushMs);

    const connect = () => {
      setStatus('connecting');
      const es = new EventSource(url);
      source = es;
      es.addEventListener('ready', () => {
        delay = RECONNECT_MIN_MS;
        setStatus('live');
      });
      es.addEventListener('event', (e) => {
        try {
          buffer.push(JSON.parse((e as MessageEvent).data));
        } catch {
          /* ignore malformed frames */
        }
      });
      const skip = (e: Event) => setDropped((d) => d + (Number((e as MessageEvent).data) || 0));
      es.addEventListener('dropped', skip);
      es.addEventListener('lagged', skip);
      // The server is stopping; the stream ends and EventSource reconnects.
      es.addEventListener('shutdown', () => setStatus('connecting'));
      es.onerror = () => {
        if (es.readyState !== EventSource.CLOSED) {
          // EventSource reconnects by itself; reflect the state meanwhile.
          setStatus('connecting');
          return;
        }
        setStatus('error');
        es.close();
        retry = window.setTimeout(connect, delay);
        delay = Math.min(delay * 2, RECONNECT_MAX_MS);
      };
    };
    connect();
    return () => {
      window.clearInterval(timer);
      window.clearTimeout(retry);
      source?.close();
    };
  }, [enabled, query, flushMs]);

  return { status, dropped };
}
