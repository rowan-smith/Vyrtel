import { useEffect, useRef, useState } from 'react';

import type { TelemetryEvent } from './types';

export type LiveStatus = 'off' | 'connecting' | 'live' | 'error';

/**
 * Subscribe to `/api/v1/live/logs` while `enabled`. Incoming events are
 * buffered and delivered at most every `flushMs`, so a burst of thousands
 * of events causes a handful of renders, not thousands.
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
    setStatus('connecting');
    const url = `/api/v1/live/logs?${new URLSearchParams({ query }).toString()}`;
    const source = new EventSource(url);
    let buffer: TelemetryEvent[] = [];
    const timer = window.setInterval(() => {
      if (buffer.length) {
        const batch = buffer;
        buffer = [];
        onEvents.current(batch);
      }
    }, flushMs);

    source.addEventListener('ready', () => setStatus('live'));
    source.addEventListener('event', (e) => {
      try {
        buffer.push(JSON.parse((e as MessageEvent).data));
      } catch {
        /* ignore malformed frames */
      }
    });
    const skip = (e: Event) => setDropped((d) => d + (Number((e as MessageEvent).data) || 0));
    source.addEventListener('dropped', skip);
    source.addEventListener('lagged', skip);
    source.onerror = () => {
      // EventSource reconnects by itself; reflect the state meanwhile.
      setStatus(source.readyState === EventSource.CLOSED ? 'error' : 'connecting');
    };
    return () => {
      window.clearInterval(timer);
      source.close();
    };
  }, [enabled, query, flushMs]);

  return { status, dropped };
}
