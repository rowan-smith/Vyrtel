import { useEffect, useState } from 'react';

import { api } from '../lib/api';
import type { Diagnostics, TraceSummary } from '../lib/types';
import { formatDateTime, formatDuration } from '../lib/format';
import { rangeWindow } from '../lib/query';
import { Link, useRouter } from '../lib/router';
import { useHasData } from '../lib/useHasData';
import { DiagnosticsPanel } from '../components/Diagnostics';
import { QueryBar, RangeSelect } from '../components/QueryBar';
import { Empty, ErrorBanner, Spinner } from '../components/common';

export function TracesPage() {
  const hasData = useHasData('traces');
  if (hasData === null) return <div className="page" />;
  if (!hasData) {
    return (
      <div className="page">
        <Empty title="No traces yet">
          Send spans with OTLP/HTTP to <code>/v1/traces</code>.
        </Empty>
      </div>
    );
  }
  return <TracesView />;
}

function TracesView() {
  const { location, navigate } = useRouter();
  const query = location.search.get('q') ?? '';
  const range = location.search.get('range') ?? '1h';
  const [traces, setTraces] = useState<TraceSummary[]>([]);
  const [diagnostics, setDiagnostics] = useState<Diagnostics | null>(null);
  const [error, setError] = useState<unknown>(null);
  const [loading, setLoading] = useState(false);
  const [runId, setRunId] = useState(0);

  useEffect(() => {
    const ctrl = new AbortController();
    setLoading(true);
    setError(null);
    api
      .searchTraces({ query, limit: 100, ...rangeWindow(range) }, ctrl.signal)
      .then((r) => {
        setTraces(r.traces);
        setDiagnostics(r.diagnostics);
      })
      .catch((e) => {
        if (e.name !== 'AbortError') {
          setError(e);
          setTraces([]);
        }
      })
      .finally(() => {
        if (!ctrl.signal.aborted) setLoading(false);
      });
    return () => ctrl.abort();
  }, [query, range, runId]);

  const setUrl = (q: string, r: string) => {
    const p = new URLSearchParams();
    if (q) p.set('q', q);
    if (r !== '1h') p.set('range', r);
    navigate(`/traces${p.toString() ? `?${p}` : ''}`);
  };

  const maxDuration = Math.max(1, ...traces.map((t) => t.durationMs));

  return (
    <div className="page">
      <div className="toolbar">
        <QueryBar
          query={query}
          placeholder='status = Error or durationMs > 500 or service = "payments"'
          onRun={(q) => (q === query ? setRunId((n) => n + 1) : setUrl(q, range))}
          running={loading}
        >
          <RangeSelect value={range} onChange={(r) => setUrl(query, r)} />
        </QueryBar>
      </div>
      <ErrorBanner error={error} query={query} />
      {/* With no traces, show only the "no traces" message (no "0 results" diagnostics line). */}
      {(traces.length > 0 || loading) && (
        <div className="results-head">
          {traces.length > 0 && <DiagnosticsPanel diagnostics={diagnostics} count={traces.length} />}
          {loading && <Spinner />}
        </div>
      )}
      {traces.length > 0 && (
        <table className="table traces-table">
          <thead>
            <tr>
              <th>Start</th>
              <th>Trace</th>
              <th>Services</th>
              <th>Spans</th>
              <th className="num">Duration</th>
              <th>Status</th>
            </tr>
          </thead>
          <tbody>
            {traces.map((t) => (
              <tr key={t.traceId} data-testid="trace-row">
                <td className="nowrap">{formatDateTime(t.start)}</td>
                <td>
                  <Link to={`/traces/${t.traceId}`} className="trace-link">
                    <strong>{t.rootName ?? '(unknown root)'}</strong>
                    <span className="muted"> {t.rootService}</span>
                  </Link>
                  <div className="muted small mono">{t.traceId}</div>
                </td>
                <td>
                  {t.services.map((s) => (
                    <span key={s} className="tag">
                      {s}
                    </span>
                  ))}
                </td>
                <td className="num">{t.spanCount}</td>
                <td className="num">
                  <div className="duration-cell">
                    <div className="duration-bar" style={{ width: `${(t.durationMs / maxDuration) * 100}%` }} />
                    <span>{formatDuration(t.durationMs)}</span>
                  </div>
                </td>
                <td>{t.errorCount > 0 ? <span className="badge badge-error">{t.errorCount} error{t.errorCount > 1 ? 's' : ''}</span> : <span className="badge badge-ok">OK</span>}</td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
      {!loading && !error && traces.length === 0 && (
        <Empty title="No traces found">{query ? 'Try a wider time range or a simpler query.' : 'Nothing in this time range. Try a wider one.'}</Empty>
      )}
    </div>
  );
}
