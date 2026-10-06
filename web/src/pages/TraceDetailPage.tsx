import { useEffect, useMemo, useState } from 'react';

import { api } from '../lib/api';
import type { TelemetryEvent, TraceResponse } from '../lib/types';
import { formatDateTime, formatDuration } from '../lib/format';
import { Link, logsLink } from '../lib/router';
import { LogRow } from '../components/LogRow';
import { PropertyTree } from '../components/PropertyTree';
import { Empty, ErrorBanner, Spinner } from '../components/common';

interface Row {
  span: TelemetryEvent;
  depth: number;
}

/** Spans in tree order (parents before children, siblings by start). */
export function spanTree(spans: TelemetryEvent[]): Row[] {
  const byId = new Map(spans.map((s) => [s.spanId, s]));
  const children = new Map<string, TelemetryEvent[]>();
  const roots: TelemetryEvent[] = [];
  for (const s of spans) {
    if (s.parentSpanId && byId.has(s.parentSpanId)) {
      const list = children.get(s.parentSpanId) ?? [];
      list.push(s);
      children.set(s.parentSpanId, list);
    } else {
      roots.push(s);
    }
  }
  const byStart = (a: TelemetryEvent, b: TelemetryEvent) => Date.parse(a.timestamp) - Date.parse(b.timestamp) || a.id.localeCompare(b.id);
  const out: Row[] = [];
  const seen = new Set<string>();
  const walk = (s: TelemetryEvent, depth: number) => {
    if (seen.has(s.id)) return; // guard against cycles in bad data
    seen.add(s.id);
    out.push({ span: s, depth });
    for (const c of (children.get(s.spanId ?? '') ?? []).sort(byStart)) walk(c, depth + 1);
  };
  for (const r of roots.sort(byStart)) walk(r, 0);
  return out;
}

/**
 * Nanoseconds since `baseSecs`. ISO strings carry nanoseconds but
 * Date.parse keeps only milliseconds, and absolute epoch nanoseconds exceed
 * a double's exact range — so we work relative to a base second.
 */
export function nanosSince(iso: string, baseSecs: number): number {
  const m = /\.(\d+)Z$/.exec(iso);
  const frac = m ? Number((m[1] + '000000000').slice(0, 9)) : 0;
  return (Math.floor(Date.parse(iso) / 1000) - baseSecs) * 1e9 + frac;
}

export function TraceDetailPage({ traceId }: { traceId: string }) {
  const [trace, setTrace] = useState<TraceResponse | null>(null);
  const [error, setError] = useState<unknown>(null);
  const [selected, setSelected] = useState<string | null>(null);
  const [logs, setLogs] = useState<TelemetryEvent[]>([]);
  const [expandedLog, setExpandedLog] = useState<string | null>(null);

  useEffect(() => {
    setTrace(null);
    setError(null);
    api.trace(traceId).then(setTrace).catch(setError);
    api
      .searchLogs({ query: `traceId = "${traceId}"`, limit: 200, direction: 'forward' })
      .then((r) => setLogs(r.events))
      .catch(() => setLogs([]));
  }, [traceId]);

  const rows = useMemo(() => (trace ? spanTree(trace.spans) : []), [trace]);
  if (error) return <div className="page"><ErrorBanner error={error} /></div>;
  if (!trace) return <div className="page"><Spinner /></div>;

  const base = Math.floor(Date.parse(trace.spans[0].timestamp) / 1000);
  const nanos = (iso: string) => nanosSince(iso, base);
  const start = Math.min(...trace.spans.map((s) => nanos(s.timestamp)));
  const end = Math.max(...trace.spans.map((s) => nanos(s.timestamp) + (s.durationNanos ?? 0)));
  const total = Math.max(1, end - start);
  const s = trace.summary;

  return (
    <div className="page trace-page">
      <div className="trace-head">
        <Link to="/traces" className="muted small">
          ← Traces
        </Link>
        <h1>
          {s.rootName ?? 'Trace'} <span className="muted">{s.rootService}</span>
        </h1>
        <div className="trace-meta">
          <span className="mono">{trace.traceId}</span>
          <span>{formatDateTime(s.start)}</span>
          <span>{formatDuration(s.durationMs)}</span>
          <span>{s.spanCount} spans</span>
          <span>{s.services.join(', ')}</span>
          {s.errorCount > 0 && <span className="badge badge-error">{s.errorCount} errors</span>}
          <Link className="btn btn-small" to={logsLink(`traceId = "${trace.traceId}"`, '30d')}>
            Related logs ({logs.length})
          </Link>
        </div>
      </div>

      <div className="waterfall" role="tree" aria-label="Span hierarchy">
        <div className="wf-scale">
          <span>0</span>
          <span>{formatDuration(total / 2e6)}</span>
          <span>{formatDuration(total / 1e6)}</span>
        </div>
        {rows.map(({ span, depth }) => {
          const off = ((nanos(span.timestamp) - start) / total) * 100;
          const width = Math.max(0.3, ((span.durationNanos ?? 0) / total) * 100);
          const isErr = span.status?.code === 'Error';
          const open = selected === span.id;
          return (
            <div key={span.id} className={`wf-row ${open ? 'is-open' : ''}`} role="treeitem" aria-level={depth + 1} aria-expanded={open} data-testid="span-row">
              <button type="button" className="wf-line" onClick={() => setSelected(open ? null : span.id)}>
                <span className="wf-name" style={{ paddingLeft: `${depth * 16}px` }}>
                  <span className={`wf-dot ${isErr ? 'err' : ''}`} />
                  <span className="wf-service">{span.service}</span> {span.name}
                </span>
                <span className="wf-track">
                  <span className={`wf-bar ${isErr ? 'err' : ''}`} style={{ left: `${off}%`, width: `${width}%` }} />
                  {/* Label after the bar, or inside its end when the bar reaches the edge. */}
                  <span
                    className={`wf-dur ${off + width > 85 ? 'inside' : ''}`}
                    style={off + width > 85 ? { right: `${100 - off - width}%` } : { left: `${off + width}%` }}
                  >
                    {formatDuration(span.durationMs ?? 0)}
                  </span>
                </span>
              </button>
              {open && (
                <div className="wf-details">
                  <div className="detail-grid">
                    <div>
                      <span className="detail-label">Span ID</span> <code>{span.spanId}</code>
                    </div>
                    {span.parentSpanId && (
                      <div>
                        <span className="detail-label">Parent</span> <code>{span.parentSpanId}</code>
                      </div>
                    )}
                    <div>
                      <span className="detail-label">Kind</span> {span.spanKind}
                    </div>
                    <div>
                      <span className="detail-label">Status</span> {span.status?.code}
                      {span.status?.message && ` — ${span.status.message}`}
                    </div>
                    <div>
                      <span className="detail-label">Start</span> {formatDateTime(span.timestamp)}
                    </div>
                  </div>
                  <div className="detail-label">Attributes</div>
                  <PropertyTree fields={span.properties} />
                  {span.events && span.events.length > 0 && (
                    <>
                      <div className="detail-label">Events</div>
                      <ul className="span-events">
                        {span.events.map((ev, i) => (
                          <li key={i}>
                            <strong>{ev.name}</strong> <span className="muted">{formatDateTime(ev.timestamp)}</span>
                            {Object.keys(ev.attributes).length > 0 && <PropertyTree fields={ev.attributes} />}
                          </li>
                        ))}
                      </ul>
                    </>
                  )}
                  <div className="detail-label">Resource</div>
                  <PropertyTree fields={span.resource} />
                </div>
              )}
            </div>
          );
        })}
      </div>

      <section className="related-logs" aria-label="Related logs">
        <h2>Related logs</h2>
        {logs.length === 0 ? (
          <Empty title="No logs with this trace id" />
        ) : (
          <div className="log-list">
            {logs.map((e) => (
              <LogRow key={e.id} event={e} expanded={expandedLog === e.id} onToggle={() => setExpandedLog(expandedLog === e.id ? null : e.id)} />
            ))}
          </div>
        )}
      </section>
    </div>
  );
}
