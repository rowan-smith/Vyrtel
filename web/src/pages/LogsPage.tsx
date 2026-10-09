import { useCallback, useEffect, useRef, useState } from 'react';
import type { CSSProperties } from 'react';

import { api } from '../lib/api';
import type { Diagnostics, Facet, Json, TelemetryEvent } from '../lib/types';
import { columnTemplate, loadColumns, orderColumns, saveColumns } from '../lib/columns';
import { andQuery, clause, filteredFields, rangeWindow, setFieldFilter } from '../lib/query';
import { useRouter } from '../lib/router';
import { useHasData } from '../lib/useHasData';
import { useLiveTail } from '../lib/useLiveTail';
import { DiagnosticsPanel } from '../components/Diagnostics';
import { LiveToggle, QueryBar, RangeSelect } from '../components/QueryBar';
import { LogsSidebar } from '../components/LogsSidebar';
import { LogRow } from '../components/LogRow';
import { Empty, ErrorBanner, Spinner } from '../components/common';

const PAGE = 200;
/** Live mode keeps at most this many rows on screen. */
const MAX_ROWS = 1000;

export function LogsPage() {
  const hasData = useHasData('logs');
  if (hasData === null) return <div className="page" />;
  if (!hasData) {
    return (
      <div className="page">
        <Empty title="No logs yet">
          Send JSON or NDJSON events to <code>/api/v1/events</code>, or OTLP logs to <code>/v1/logs</code>. See the README quick start.
        </Empty>
      </div>
    );
  }
  return <LogsView />;
}

function LogsView() {
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
  const [columns, setColumns] = useState<string[]>(loadColumns);
  const [facets, setFacets] = useState<{ fields: Facet[]; sampled: number }>({ fields: [], sampled: 0 });
  const [facetsLoading, setFacetsLoading] = useState(false);
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
    return () => ctrl.abort();
  }, [query, range, runId]);

  useEffect(() => {
    const ctrl = new AbortController();
    setFacetsLoading(true);
    const win = rangeWindow(range);
    // A field you've filtered on lists its values as if that one filter weren't applied (but the
    // others still are), so you can see and switch to the alternatives.
    const own = filteredFields(query);
    Promise.all([
      api.facets('logs', { query, sample: 2000, ...win }, ctrl.signal),
      ...own.map((field) => api.facets('logs', { query: setFieldFilter(query, field, null), sample: 2000, ...win }, ctrl.signal)),
    ])
      .then(([main, ...others]) => {
        const fields = main.fields.map((f) => {
          const i = own.indexOf(f.field);
          return (i >= 0 && others[i].fields.find((x) => x.field === f.field)) || f;
        });
        setFacets({ fields, sampled: main.sampled });
      })
      .catch(() => setFacets({ fields: [], sampled: 0 }))
      .finally(() => {
        if (!ctrl.signal.aborted) setFacetsLoading(false);
      });
    return () => ctrl.abort();
  }, [query, range, runId]);

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

  /** Side-panel filters: one value per field; `null` clears it. */
  const setFilter = (field: string, value: Json | null) => setUrl(setFieldFilter(query, field, value), range);

  const changeColumns = (cols: string[]) => {
    setColumns(cols);
    saveColumns(cols);
  };
  const shownColumns = orderColumns(columns);

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

  // With nothing to show, show only the "no events" message: no "0 results"
  // diagnostics line or empty filter panel.
  const hasResults = events.length > 0;

  return (
    <div className="page logs-page">
      <div className="toolbar">
        <QueryBar
          query={query}
          onRun={run}
          running={loading}
        >
          <RangeSelect value={range} onChange={(r) => setUrl(query, r)} />
          <LiveToggle live={live} status={liveStatus} onChange={setLive} />
        </QueryBar>
      </div>
      <ErrorBanner error={error} query={query} />
      <div className={`logs-layout ${hasResults ? 'with-side' : ''}`}>
        <section className="logs-main" aria-label="Log events">
          {(hasResults || loading) && (
            <div className="results-head">
              {hasResults && <DiagnosticsPanel diagnostics={diagnostics} count={events.length} />}
              {loading && <Spinner />}
              {live && dropped > 0 && <span className="muted small">{dropped} live events skipped (too fast to display)</span>}
            </div>
          )}
          <div className="log-list" data-testid="log-list" style={{ '--log-cols': columnTemplate(shownColumns) } as CSSProperties}>
            {events.map((e) => (
              <LogRow
                key={e.id}
                event={e}
                expanded={expanded.has(e.id)}
                onToggle={() => toggle(e.id)}
                onFilter={addFilter}
                isNew={newIds.has(e.id)}
                columns={shownColumns}
              />
            ))}
          </div>
          {!loading && !error && events.length === 0 && (
            <Empty title="No matching events">
              {query ? 'Try a wider time range or a simpler query.' : 'Nothing in this time range. Try a wider one.'}
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
        {hasResults && (
          <LogsSidebar
            facets={facets.fields}
            sampled={facets.sampled}
            loading={facetsLoading}
            query={query}
            columns={columns}
            onColumnsChange={changeColumns}
            onFilter={setFilter}
          />
        )}
      </div>
    </div>
  );
}
