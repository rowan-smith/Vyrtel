// A deliberately tiny History-API router: the app has a handful of routes,
// which is not worth a routing dependency.

import { createContext, useCallback, useContext, useEffect, useMemo, useState } from 'react';
import type { MouseEvent, ReactNode } from 'react';

interface Location {
  path: string;
  search: URLSearchParams;
}

interface RouterValue {
  location: Location;
  navigate: (to: string, opts?: { replace?: boolean }) => void;
}

const RouterContext = createContext<RouterValue | null>(null);

function current(): Location {
  return { path: window.location.pathname, search: new URLSearchParams(window.location.search) };
}

export function RouterProvider({ children }: { children: ReactNode }) {
  const [location, setLocation] = useState<Location>(current);

  useEffect(() => {
    const onPop = () => setLocation(current());
    window.addEventListener('popstate', onPop);
    return () => window.removeEventListener('popstate', onPop);
  }, []);

  const navigate = useCallback((to: string, opts?: { replace?: boolean }) => {
    if (to === window.location.pathname + window.location.search) return;
    if (opts?.replace) window.history.replaceState(null, '', to);
    else window.history.pushState(null, '', to);
    setLocation(current());
  }, []);

  const value = useMemo(() => ({ location, navigate }), [location, navigate]);
  return <RouterContext.Provider value={value}>{children}</RouterContext.Provider>;
}

export function useRouter(): RouterValue {
  const r = useContext(RouterContext);
  if (!r) throw new Error('useRouter outside RouterProvider');
  return r;
}

/** Match `/traces/:id` style patterns. Returns params or null. */
export function matchPath(pattern: string, path: string): Record<string, string> | null {
  const p = pattern.split('/').filter(Boolean);
  const s = path.split('/').filter(Boolean);
  if (p.length !== s.length) return null;
  const params: Record<string, string> = {};
  for (let i = 0; i < p.length; i++) {
    if (p[i].startsWith(':')) params[p[i].slice(1)] = decodeURIComponent(s[i]);
    else if (p[i] !== s[i]) return null;
  }
  return params;
}

export function Link({
  to,
  children,
  className,
  title,
}: {
  to: string;
  children: ReactNode;
  className?: string;
  title?: string;
}) {
  const { navigate } = useRouter();
  const onClick = (e: MouseEvent<HTMLAnchorElement>) => {
    if (e.metaKey || e.ctrlKey || e.shiftKey || e.button !== 0) return;
    e.preventDefault();
    navigate(to);
  };
  return (
    <a href={to} onClick={onClick} className={className} title={title}>
      {children}
    </a>
  );
}

export function logsLink(query: string, range?: string): string {
  const p = new URLSearchParams();
  if (query) p.set('q', query);
  if (range) p.set('range', range);
  const s = p.toString();
  return `/logs${s ? `?${s}` : ''}`;
}
