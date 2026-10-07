import { useEffect, useState } from 'react';

import { api } from '../lib/api';
import type { Agg, MetricResponse } from '../lib/types';
import { boundedWindow } from '../lib/query';
import { useRouter } from '../lib/router';
import { useHasData } from '../lib/useHasData';
import { LineChart, seriesColor } from '../components/Charts';
import { RangeSelect } from '../components/QueryBar';
import { Empty, ErrorBanner, Spinner } from '../components/common';

export const AGGS: Agg[] = ['avg', 'sum', 'count', 'min', 'max', 'last'];

export function MetricsPage() {
  const hasData = useHasData('metrics');
  if (hasData === null) return <div className="page" />;
  if (!hasData) return <NoMetrics />;
  return <MetricsView />;
}

function NoMetrics() {
  return (
    <div className="page">
      <Empty title="No metrics yet">
        Send OTLP metrics to <code>/v1/metrics</code> (counters, gauges and histograms are supported).
      </Empty>
    </div>
  );
}

function MetricsView() {
  const { location, navigate } = useRouter();
  const [names, setNames] = useState<string[] | null>(null);
  const name = location.search.get('name') ?? '';
  const agg = (location.search.get('agg') as Agg) ?? 'avg';
  const range = location.search.get('range') ?? '1h';
  const groupBy = location.search.get('groupBy') ?? '';
  const filter = location.search.get('q') ?? '';
  const [draft, setDraft] = useState({ groupBy, filter });
  const [data, setData] = useState<MetricResponse | null>(null);
  const [error, setError] = useState<unknown>(null);
  const [loading, setLoading] = useState(false);

  useEffect(() => setDraft({ groupBy, filter }), [groupBy, filter]);

  useEffect(() => {
    api
      .metricNames()
      .then((r) => setNames(r.metrics.map((m) => m.name)))
      .catch((e) => {
        setError(e);
        setNames([]);
      });
  }, []);

  const set = (patch: Record<string, string>) => {
    const p = new URLSearchParams({ name, agg, range, groupBy, q: filter, ...patch });
    for (const [k, v] of [...p.entries()]) if (!v) p.delete(k);
    navigate(`/metrics?${p}`);
  };

  useEffect(() => {
    if (!name && names && names.length) set({ name: names[0] });
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [names]);

  useEffect(() => {
    if (!name) return;
    const ctrl = new AbortController();
    setLoading(true);
    setError(null);
    api
      .metricQuery({ name, agg, groupBy: groupBy || undefined, query: filter || undefined, ...boundedWindow(range) }, ctrl.signal)
      .then(setData)
      .catch((e) => {
        if (e.name !== 'AbortError') setError(e);
      })
      .finally(() => {
        if (!ctrl.signal.aborted) setLoading(false);
      });
    return () => ctrl.abort();
  }, [name, agg, range, groupBy, filter]);

  if (names && names.length === 0 && !error) return <NoMetrics />;

  return (
    <div className="page">
      <form
        className="toolbar metric-toolbar"
        onSubmit={(e) => {
          e.preventDefault();
          set({ groupBy: draft.groupBy.trim(), q: draft.filter.trim() });
        }}
      >
        <label>
          <span className="label">Metric</span>
          <select className="select" aria-label="Metric" value={name} onChange={(e) => set({ name: e.target.value })}>
            {(names ?? []).map((n) => (
              <option key={n}>{n}</option>
            ))}
          </select>
        </label>
        <label>
          <span className="label">Aggregation</span>
          <select className="select" aria-label="Aggregation" value={agg} onChange={(e) => set({ agg: e.target.value })}>
            {AGGS.map((a) => (
              <option key={a}>{a}</option>
            ))}
          </select>
        </label>
        <label>
          <span className="label">Group by</span>
          <input className="input" placeholder="attribute, e.g. route" value={draft.groupBy} onChange={(e) => setDraft({ ...draft, groupBy: e.target.value })} />
        </label>
        <label className="grow">
          <span className="label">Filter</span>
          <input className="input" placeholder='route = "/checkout"' value={draft.filter} onChange={(e) => setDraft({ ...draft, filter: e.target.value })} />
        </label>
        <RangeSelect value={range} onChange={(r) => set({ range: r })} />
        <button className="btn btn-primary" type="submit">
          Apply
        </button>
      </form>
      <ErrorBanner error={error} query={filter} />
      {loading && <Spinner />}
      {data && (
        <div className="card">
          <div className="card-head">
            <h2>
              {agg}({data.name}) {data.unit && <span className="muted">{data.unit}</span>}
            </h2>
            <span className="muted small">
              {data.kind} · step {Math.round(data.stepMs / 1000)}s
            </span>
          </div>
          {data.description && <p className="muted">{data.description}</p>}
          <LineChart series={data.series} unit={data.unit} />
          {data.series.length > 1 && (
            <ul className="legend">
              {data.series.map((s, i) => (
                <li key={s.group ?? i}>
                  <span className="swatch" style={{ background: seriesColor(i) }} /> {s.group}
                </li>
              ))}
            </ul>
          )}
        </div>
      )}
    </div>
  );
}
