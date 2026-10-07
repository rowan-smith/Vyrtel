import { useEffect, useState } from 'react';

import { api } from './api';

type Signal = 'logs' | 'traces' | 'metrics';

/**
 * Whether the server has stored any events of `signal` at all: `null` while unknown, then a
 * boolean. Pages use it to show a single "nothing here yet" state instead of empty query tools.
 * While empty it re-checks every `pollMs`, so the page switches over once data arrives. If the
 * check itself fails the page is shown normally (it will surface the error itself).
 */
export function useHasData(signal: Signal, pollMs = 5000): boolean | null {
  const [has, setHas] = useState<boolean | null>(null);

  useEffect(() => {
    let stopped = false;
    let timer: number | undefined;
    const check = () => {
      api
        .info()
        .then((i) => {
          if (stopped) return;
          const any = (i.eventCounts?.[signal] ?? 0) > 0;
          setHas(any);
          if (!any) timer = window.setTimeout(check, pollMs);
        })
        .catch(() => {
          if (!stopped) setHas(true);
        });
    };
    check();
    return () => {
      stopped = true;
      window.clearTimeout(timer);
    };
  }, [signal, pollMs]);

  return has;
}
