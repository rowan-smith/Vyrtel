import { useMemo, useState } from 'react';
import { useQuery } from '@tanstack/react-query';
import { Link } from 'react-router-dom';
import { listTraces } from '../api';
import { formatDuration, formatTime, presetRange, type TimePreset } from '../types';

const PRESETS: { id: TimePreset; label: string }[] = [
  { id: '1h', label: 'Last hour' },
  { id: '6h', label: 'Last 6 hours' },
  { id: '24h', label: 'Last 24 hours' },
  { id: '7d', label: 'Last 7 days' },
];

export default function TracesPage() {
  const [preset, setPreset] = useState<TimePreset>('1h');
  const range = useMemo(() => presetRange(preset), [preset]);

  const tracesQuery = useQuery({
    queryKey: ['traces', range.from, range.to],
    queryFn: () => listTraces({ from: range.from, to: range.to }),
  });

  return (
    <div className="page">
      <div className="page-header">
        <h1 className="page-title">Traces</h1>
        <select className="select" value={preset} onChange={(e) => setPreset(e.target.value as TimePreset)}>
          {PRESETS.map((p) => (
            <option key={p.id} value={p.id}>
              {p.label}
            </option>
          ))}
        </select>
      </div>
      <div className="trace-list">
        <table className="table">
          <thead>
            <tr>
              <th>Timestamp</th>
              <th>Trace ID</th>
              <th>Root operation</th>
              <th>Service</th>
              <th>Duration</th>
              <th>Status</th>
            </tr>
          </thead>
          <tbody>
            {(tracesQuery.data?.traces ?? []).map((t) => (
              <tr key={t.traceId}>
                <td>{formatTime(t.timestamp)}</td>
                <td>
                  <Link to={`/traces/${encodeURIComponent(t.traceId)}`}>{t.traceId}</Link>
                </td>
                <td>{t.rootOperation ?? '—'}</td>
                <td>{t.service ?? '—'}</td>
                <td>{formatDuration(t.durationNs)}</td>
                <td>{t.status}</td>
              </tr>
            ))}
          </tbody>
        </table>
        {(tracesQuery.data?.traces.length ?? 0) === 0 && !tracesQuery.isLoading && (
          <div className="empty">No traces in this time range.</div>
        )}
      </div>
    </div>
  );
}
