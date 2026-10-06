import { useMemo, useState } from 'react';
import { useQuery } from '@tanstack/react-query';
import { Link, useParams } from 'react-router-dom';
import { getTrace } from '../api';
import StacktraceView, {
  resolveStacktrace,
  isStacktraceKey,
} from '../components/StacktraceView';
import { formatDuration, formatTime, type Event } from '../types';

export default function TraceDetailPage() {
  const { traceId = '' } = useParams();
  const [selected, setSelected] = useState<Event | null>(null);
  const [expandedLogId, setExpandedLogId] = useState<string | null>(null);

  const traceQuery = useQuery({
    queryKey: ['trace', traceId],
    queryFn: () => getTrace(traceId),
    enabled: !!traceId,
  });

  const spans = traceQuery.data?.spans ?? [];
  const logs = useMemo(() => {
    const all = traceQuery.data?.logs ?? [];
    if (!selected?.spanId) return all;
    return all.filter((l) => !l.spanId || l.spanId === selected.spanId);
  }, [traceQuery.data, selected]);

  const root = useMemo(() => {
    return (
      spans.find((s) => !s.parentSpanId || s.parentSpanId === '') ??
      spans[0] ??
      null
    );
  }, [spans]);

  const { minTs, totalNs } = useMemo(() => {
    if (spans.length === 0) return { minTs: 0, totalNs: 1 };
    const starts = spans.map((s) => new Date(s.timestamp).getTime());
    const min = Math.min(...starts);
    const maxEnd = Math.max(
      ...spans.map((s) => new Date(s.timestamp).getTime() + (s.durationNs ?? 0) / 1e6),
    );
    const total = Math.max(1, (maxEnd - min) * 1e6);
    return { minTs: min, totalNs: root?.durationNs ?? total };
  }, [spans, root]);

  return (
    <div className="page">
      <div className="page-header">
        <div>
          <div className="muted">
            <Link to="/traces">Traces</Link> / {traceId}
          </div>
          <h1 className="page-title" style={{ marginTop: 4 }}>
            {root?.message ?? 'Trace'}{' '}
            <span className="muted">{formatDuration(root?.durationNs ?? totalNs)}</span>
          </h1>
        </div>
      </div>

      <div className="waterfall">
        {spans.map((span) => {
          const startMs = new Date(span.timestamp).getTime() - minTs;
          const startPct = Math.min(100, (startMs * 1e6 * 100) / totalNs);
          const widthPct = Math.max(
            0.4,
            Math.min(100 - startPct, ((span.durationNs ?? 0) * 100) / totalNs),
          );
          return (
            <div
              key={span.id}
              className={`span-row${selected?.id === span.id ? ' selected' : ''}`}
              onClick={() => setSelected(span)}
            >
              <div className="span-name" title={span.message ?? ''}>
                {span.message ?? span.spanId}
                <div className="muted" style={{ fontSize: 11 }}>
                  {span.service}
                </div>
              </div>
              <div className="span-dur">{formatDuration(span.durationNs)}</div>
              <div className="span-bar-track">
                <div className="span-bar" style={{ left: `${startPct}%`, width: `${widthPct}%` }} />
              </div>
            </div>
          );
        })}
        {spans.length === 0 && <div className="empty">No spans in this trace.</div>}
      </div>

      {selected && (
        <div className="settings-section" style={{ maxWidth: 'none' }}>
          <h2>Span</h2>
          <div className="form-row"><span className="muted">Operation</span><span>{selected.message}</span></div>
          <div className="form-row"><span className="muted">Service</span><span>{selected.service}</span></div>
          <div className="form-row"><span className="muted">Duration</span><span>{formatDuration(selected.durationNs)}</span></div>
          <div className="form-row"><span className="muted">Start</span><span>{formatTime(selected.timestamp)}</span></div>
          <div className="form-row"><span className="muted">Span ID</span><span className="muted">{selected.spanId}</span></div>
          <div className="form-row"><span className="muted">Parent</span><span className="muted">{selected.parentSpanId ?? '—'}</span></div>
          {Object.entries(selected.attributes ?? {})
            .filter(([k]) => !isStacktraceKey(k))
            .map(([k, v]) => (
              <div key={k} className="form-row">
                <span className="muted">{k}</span>
                <span>{typeof v === 'string' ? v : JSON.stringify(v)}</span>
              </div>
            ))}
          {(() => {
            const stack = resolveStacktrace(selected);
            if (!stack) return null;
            return (
              <div style={{ marginTop: '0.75rem' }}>
                <div className="props-title">Stacktrace</div>
                <StacktraceView stacktrace={stack} />
              </div>
            );
          })()}
        </div>
      )}

      <div className="props-title">Related Logs</div>
      <div className="event-list" style={{ maxHeight: 280 }}>
        {logs.map((log) => {
          const isExpanded = expandedLogId === log.id;
          const logStack = resolveStacktrace(log);
          return (
            <div key={log.id} style={{ display: 'contents' }}>
              <div
                className={`event-row${isExpanded ? ' selected' : ''}`}
                style={{ cursor: 'pointer' }}
                onClick={() => setExpandedLogId((prev) => (prev === log.id ? null : log.id))}
              >
                <div className="event-time">{formatTime(log.timestamp)}</div>
                <div className={`event-level level-${log.level ?? 'trace'}`}>
                  {log.level?.slice(0, 3).toUpperCase()}
                </div>
                <div className="event-service">{log.service}</div>
                <div className="event-message">{log.message}</div>
              </div>
              {isExpanded && (
                <div className="event-details" style={{ margin: '0 0.5rem 0.5rem' }}>
                  <div className="props-title">Properties</div>
                  <table className="prop-table">
                    <tbody>
                      {Object.entries(log.attributes ?? {})
                        .filter(([k]) => !isStacktraceKey(k))
                        .map(([k, v]) => (
                          <tr key={k}>
                            <td className="prop-key">{k}</td>
                            <td>
                              <span className="prop-val">
                                {typeof v === 'string' ? v : JSON.stringify(v)}
                              </span>
                            </td>
                          </tr>
                        ))}
                    </tbody>
                  </table>
                  {logStack && (
                    <div style={{ marginTop: '0.6rem' }}>
                      <div className="props-title">Stacktrace</div>
                      <StacktraceView stacktrace={logStack} />
                    </div>
                  )}
                </div>
              )}
            </div>
          );
        })}
        {logs.length === 0 && <div className="empty">No related logs.</div>}
      </div>
    </div>
  );
}
