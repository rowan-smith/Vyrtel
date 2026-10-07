import { useCallback, useEffect, useRef, useState } from 'react';

import { api } from '../lib/api';
import type { Diagnostics, Facet, HistogramBucket, Json, TelemetryEvent } from '../lib/types';
import { andQuery, boundedWindow, clause, rangeWindow } from '../lib/query';
import { useRouter } from '../lib/router';
import { useLiveTail } from '../lib/useLiveTail';
import { DiagnosticsPanel } from '../components/Diagnostics';
import { FilterPanel } from '../components/FilterPanel';
import { HistogramChart } from '../components/Charts';
import { LiveToggle, QueryBar, RangeSelect } from '../components/QueryBar';
import { LogRow } from '../components/LogRow';
import { Empty, ErrorBanner, Spinner } from '../components/common';

const PAGE = 200;
/** Live mode keeps at most this many rows on screen. */
const MAX_ROWS = 1000;

function loadPref(key: string, fallback: boolean): boolean {
  try {
    const v = localStorage.getItem(key);
    return v == null ? fallback : v === '1';
  } catch {
    return fallback;
  }
}

function savePref(key: string, v: boolean) {
  try {
    localStorage.setItem(key, v ? '1' : '0');
  } catch {
    /* storage unavailable */
  }
}

export function LogsPage() {
  const { location, navigate } = useRouter();
  const query = location.search.get('q') ?? '';
  const range = location.search.get('range') ?? '1h';

  const [events, setEvents] = useState<TelemetryEvent[]>([]);
  const [token, setToken] = useState<string | null>(null);
  const [diagnostics, setDiagnostics] = useState<Diagnostics | null>(null);
  const [error, setError] = useState<unknown>(null);
  const [loading, setLoading] = useState(false);
  const [expanded, setExpanded] = useState<Set<string>>(new Set());
  const [newIds, setNewIds] = useState<Set<string>>(new Set());
  const [live, setLive] = useState(false);
  const [showFilters, setShowFilters] = useState(() => loadPref('vyrtel.filters', true));
  const [facets, setFacets] = useState<{ fields: Facet[]; sampled: number }>({ fields: [], sampled: 0 });
  const [facetsLoading, setFacetsLoading] = useState(false);
  const [histogram, setHistogram] = useState<HistogramBucket[]>([]);
  const [runId, setRunId] = useState(0);
  const abort = useRef<AbortController | null>(null);

  const setUrl = useCallback(
    (q: string, r: string) => {
      const p = new URLSearchParams();
      if (q) p.set('q', q);
      if (r !== '1h') p.set('range', r);
      const s = p.toString();
      navigate(`/logs${s ? `?${s}` : ''}`);
    },
    [navigate],
  );

  // Run the search whenever query/range change or Run is pressed.
  useEffect(() => {
    abort.current?.abort();
    const ctrl = new AbortController();
    abort.current = ctrl;
    setLoading(true);
    setError(null);
    const win = rangeWindow(range);
    api
      .searchLogs({ query, limit: PAGE, ...win }, ctrl.signal)
      .then((r) => {
        setEvents(r.events);
        setToken(r.continuationToken);
        setDiagnostics(r.diagnostics);
        setNewIds(new Set());
      })
      .catch((e) => {
        if (e.name === 'AbortError') return;
        setError(e);
        setEvents([]);
        setToken(null);
        setDiagnostics(null);
      })
      .finally(() => {
        if (!ctrl.signal.aborted) setLoading(false);
      });
    api
      .histogram('logs', { query, buckets: 80, ...boundedWindow(range) }, ctrl.signal)
      .then((h) => setHistogram(h.buckets))
      .catch(() => setHistogram([]));
    return () => ctrl.abort();
  }, [query, range, runId]);

  useEffect(() => {
    if (!showFilters) return;
    const ctrl = new AbortController();
    setFacetsLoading(true);
    api
      .facets('logs', { query, sample: 2000, ...rangeWindow(range) }, ctrl.signal)
      .then((f) => setFacets({ fields: f.fields, sampled: f.sampled }))
      .catch(() => setFacets({ fields: [], sampled: 0 }))
      .finally(() => {
        if (!ctrl.signal.aborted) setFacetsLoading(false);
      });
    return () => ctrl.abort();
  }, [query, range, runId, showFilters]);

  const onLive = useCallback((incoming: TelemetryEvent[]) => {
    // Newest first, like the result list.
    const fresh = [...incoming].reverse();
    setEvents((prev) => {
      const seen = new Set(prev.map((e) => e.id));
      const add = fresh.filter((e) => !seen.has(e.id));
      return [...add, ...prev].slice(0, MAX_ROWS);
    });
    setNewIds(new Set(fresh.map((e) => e.id)));
  }, []);
  const { status: liveStatus, dropped } = useLiveTail({ enabled: live && !error, query, onEvents: onLive });

  const run = (q: string) => {
    if (q === query) setRunId((n) => n + 1);
    else setUrl(q, range);
  };

  const addFilter = (field: string, value: Json, negate = false) => {
    const c = clause(field, value, negate);
    if (c) setUrl(andQuery(query, c), range);
  };

  const loadMore = async () => {
    if (!token) return;
    setLoading(true);
    try {
      const r = await api.searchLogs({ query, limit: PAGE, continuationToken: token, ...rangeWindow(range) });
      setEvents((prev) => [...prev, ...r.events]);
      setToken(r.continuationToken);
    } catch (e) {
      setError(e);
    } finally {
      setLoading(false);
    }
  };

  const toggle = (id: string) =>
    setExpanded((s) => {
      const n = new Set(s);
      if (n.has(id)) n.delete(id);
      else n.add(id);
      return n;
    });

  return (
    <div className="page logs-page">
      <div className="toolbar">
        <QueryBar query={query} onRun={run} running={loading}>
          <LiveToggle live={live} status={liveStatus} onChange={setLive} />
          <RangeSelect value={range} onChange={(r) => setUrl(query, r)} />
          <button
            type="button"
            className={`btn ${showFilters ? 'btn-active' : ''}`}
            aria-pressed={showFilters}
            onClick={() => {
              setShowFilters((v) => {
                savePref('vyrtel.filters', !v);
                return !v;
              });
            }}
          >
            Filters
          </button>
        </QueryBar>
      </div>
      <ErrorBanner error={error} query={query} />
      <div className={`logs-layout ${showFilters ? 'with-filters' : ''}`}>
        {showFilters && <FilterPanel facets={facets.fields} sampled={facets.sampled} loading={facetsLoading} onSelect={(f, v) => addFilter(f, v)} />}
        <section className="logs-main" aria-label="Log events">
          {histogram.length > 0 && <HistogramChart buckets={histogram} />}
          <div className="results-head">
            <DiagnosticsPanel diagnostics={diagnostics} count={events.length} />
            {loading && <Spinner />}
            {live && dropped > 0 && <span className="muted small">{dropped} live events skipped (too fast to display)</span>}
          </div>
          <div className="log-list" data-testid="log-list">
            {events.map((e) => (
              <LogRow
                key={e.id}
                event={e}
                expanded={expanded.has(e.id)}
                onToggle={() => toggle(e.id)}
                onFilter={addFilter}
                isNew={newIds.has(e.id)}
              />
            ))}
          </div>
          {!loading && !error && events.length === 0 && (
            <Empty title="No matching events">
              {query ? 'Try a wider time range or a simpler query.' : 'Send your first event — see the README quick start.'}
            </Empty>
          )}
          {token && (
            <div className="load-more">
              <button type="button" className="btn" onClick={loadMore} disabled={loading}>
                Load older events
              </button>
            </div>
          )}
        </section>
      </div>
    </div>
  );
}
