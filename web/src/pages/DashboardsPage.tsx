import { useCallback, useEffect, useState } from 'react';

import { api } from '../lib/api';
import type { Agg, Dashboard, Json, Panel, PanelKind, TelemetryEvent } from '../lib/types';
import { formatCount, formatDateTime } from '../lib/format';
import { boundedWindow } from '../lib/query';
import { Link, logsLink, useRouter } from '../lib/router';
import { HistogramChart, LineChart } from '../components/Charts';
import { RangeSelect } from '../components/QueryBar';
import { Empty, ErrorBanner, Modal, Spinner } from '../components/common';
import { AGGS } from './MetricsPage';

export function DashboardsPage() {
  const { navigate } = useRouter();
  const [list, setList] = useState<Dashboard[] | null>(null);
  const [error, setError] = useState<unknown>(null);
  const [name, setName] = useState('');

  const load = useCallback(() => {
    api
      .dashboards()
      .then((r) => setList(r.dashboards))
      .catch(setError);
  }, []);
  useEffect(load, [load]);

  const create = async () => {
    if (!name.trim()) return;
    try {
      const d = await api.createDashboard(name.trim());
      navigate(`/dashboards/${d.id}`);
    } catch (e) {
      setError(e);
    }
  };

  return (
    <div className="page narrow">
      <div className="page-head">
        <h1>Dashboards</h1>
        <form
          className="inline-form"
          onSubmit={(e) => {
            e.preventDefault();
            create();
          }}
        >
          <input className="input" aria-label="New dashboard name" placeholder="New dashboard name" value={name} onChange={(e) => setName(e.target.value)} />
          <button className="btn btn-primary" type="submit" disabled={!name.trim()}>
            Create
          </button>
        </form>
      </div>
      <ErrorBanner error={error} />
      {list === null && <Spinner />}
      {list?.length === 0 && <Empty title="No dashboards yet">Create one to pin log counts, metrics and saved queries.</Empty>}
      <ul className="card-list">
        {list?.map((d) => (
          <li key={d.id} className="card">
            <Link to={`/dashboards/${d.id}`}>
              <strong>{d.name}</strong>
            </Link>
            <span className="muted small">
              {d.panels.length} panel{d.panels.length === 1 ? '' : 's'} · updated {formatDateTime(d.updatedAt)}
            </span>
          </li>
        ))}
      </ul>
    </div>
  );
}

const KIND_LABELS: Record<PanelKind, string> = {
  log_count: 'Log count over time',
  metric_line: 'Metric line chart',
  single_stat: 'Single statistic',
  saved_query: 'Saved log query',
};

type Draft = { title: string; kind: PanelKind; query: string; metric: string; agg: Agg; groupBy: string; width: number };

function draftOf(p?: Panel): Draft {
  const c = p?.config ?? {};
  const s = (k: string) => (typeof c[k] === 'string' ? (c[k] as string) : '');
  return {
    title: p?.title ?? '',
    kind: p?.kind ?? 'log_count',
    query: s('query'),
    metric: s('metric'),
    agg: (s('agg') as Agg) || 'avg',
    groupBy: s('groupBy'),
    width: p?.width ?? 6,
  };
}

function configOf(d: Draft): { [k: string]: Json } {
  switch (d.kind) {
    case 'metric_line':
      return { metric: d.metric, agg: d.agg, ...(d.groupBy ? { groupBy: d.groupBy } : {}), ...(d.query ? { query: d.query } : {}) };
    case 'single_stat':
      return d.metric ? { metric: d.metric, agg: d.agg } : { query: d.query };
    default:
      return { query: d.query };
  }
}

function PanelEditor({ panel, metrics, onSave, onClose }: { panel?: Panel; metrics: string[]; onSave: (d: Draft) => Promise<void>; onClose: () => void }) {
  const [d, setD] = useState<Draft>(() => draftOf(panel));
  const [error, setError] = useState<unknown>(null);
  const usesMetric = d.kind === 'metric_line' || (d.kind === 'single_stat' && d.metric !== '');
  return (
    <Modal title={panel ? 'Edit panel' : 'Add panel'} onClose={onClose}>
      <form
        className="form"
        onSubmit={async (e) => {
          e.preventDefault();
          try {
            await onSave(d);
          } catch (err) {
            setError(err);
          }
        }}
      >
        <label>
          <span className="label">Title</span>
          <input className="input" required value={d.title} onChange={(e) => setD({ ...d, title: e.target.value })} />
        </label>
        <label>
          <span className="label">Type</span>
          <select className="select" value={d.kind} onChange={(e) => setD({ ...d, kind: e.target.value as PanelKind })}>
            {Object.entries(KIND_LABELS).map(([k, v]) => (
              <option key={k} value={k}>
                {v}
              </option>
            ))}
          </select>
        </label>
        {(d.kind === 'metric_line' || d.kind === 'single_stat') && (
          <label>
            <span className="label">Metric {d.kind === 'single_stat' && <span className="muted">(leave empty to count logs)</span>}</span>
            <select className="select" value={d.metric} onChange={(e) => setD({ ...d, metric: e.target.value })}>
              {d.kind === 'single_stat' && <option value="">— count log events —</option>}
              {metrics.map((m) => (
                <option key={m}>{m}</option>
              ))}
            </select>
          </label>
        )}
        {usesMetric && (
          <label>
            <span className="label">Aggregation</span>
            <select className="select" value={d.agg} onChange={(e) => setD({ ...d, agg: e.target.value as Agg })}>
              {AGGS.map((a) => (
                <option key={a}>{a}</option>
              ))}
            </select>
          </label>
        )}
        {d.kind === 'metric_line' && (
          <label>
            <span className="label">Group by (optional)</span>
            <input className="input" value={d.groupBy} onChange={(e) => setD({ ...d, groupBy: e.target.value })} />
          </label>
        )}
        {!usesMetric && (
          <label>
            <span className="label">Log query</span>
            <input className="input mono" value={d.query} placeholder='level = "Error"' onChange={(e) => setD({ ...d, query: e.target.value })} />
          </label>
        )}
        <label>
          <span className="label">Width</span>
          <select className="select" value={d.width} onChange={(e) => setD({ ...d, width: Number(e.target.value) })}>
            <option value={3}>Quarter</option>
            <option value={4}>Third</option>
            <option value={6}>Half</option>
            <option value={12}>Full</option>
          </select>
        </label>
        <ErrorBanner error={error} query={d.query} />
        <div className="form-actions">
          <button type="button" className="btn" onClick={onClose}>
            Cancel
          </button>
          <button type="submit" className="btn btn-primary">
            Save
          </button>
        </div>
      </form>
    </Modal>
  );
}

function PanelBody({ panel, range, refresh }: { panel: Panel; range: string; refresh: number }) {
  const [state, setState] = useState<{ loading: boolean; error?: unknown; data?: unknown }>({ loading: true });
  const c = panel.config;
  const query = typeof c.query === 'string' ? c.query : '';
  const metric = typeof c.metric === 'string' ? c.metric : '';
  const agg = (typeof c.agg === 'string' ? c.agg : 'avg') as Agg;
  const groupBy = typeof c.groupBy === 'string' ? c.groupBy : undefined;

  useEffect(() => {
    const ctrl = new AbortController();
    const win = boundedWindow(range);
    let p: Promise<unknown>;
    switch (panel.kind) {
      case 'log_count':
        p = api.histogram('logs', { query, buckets: 40, ...win }, ctrl.signal);
        break;
      case 'metric_line':
        p = api.metricQuery({ name: metric, agg, groupBy, query: query || undefined, ...win }, ctrl.signal);
        break;
      case 'single_stat':
        p = metric
          ? api.metricQuery({ name: metric, agg, ...win, stepMs: Math.max(1000, Date.parse(win.to) - Date.parse(win.from)) }, ctrl.signal)
          : api.count('logs', { query, ...win }, ctrl.signal);
        break;
      case 'saved_query':
        p = api.searchLogs({ query, limit: 10, ...win }, ctrl.signal);
        break;
    }
    setState((s) => ({ ...s, loading: true }));
    p.then((data) => setState({ loading: false, data })).catch((error) => {
      if (error.name !== 'AbortError') setState({ loading: false, error });
    });
    return () => ctrl.abort();
  }, [panel.kind, query, metric, agg, groupBy, range, refresh]);

  if (state.error) return <ErrorBanner error={state.error} />;
  if (!state.data) return <Spinner />;
  const data = state.data as Record<string, unknown>;
  switch (panel.kind) {
    case 'log_count':
      return (
        <div>
          <div className="panel-total">{formatCount(data.total as number)} events</div>
          <HistogramChart buckets={data.buckets as never} height={90} />
        </div>
      );
    case 'metric_line':
      return <LineChart series={data.series as never} unit={data.unit as string} height={180} />;
    case 'single_stat': {
      let value: number | null;
      if (metric) {
        const pts = ((data.series as { points: { value: number | null }[] }[]) ?? []).flatMap((s) => s.points);
        const vals = pts.map((p) => p.value).filter((v): v is number => v != null);
        value = vals.length ? vals[vals.length - 1] : null;
      } else {
        value = data.count as number;
      }
      return (
        <div className="big-stat" data-testid="single-stat">
          {value == null ? '–' : formatCount(Math.round(value * 100) / 100)}
          <div className="muted small">{metric ? `${agg}(${metric})` : query || 'all logs'}</div>
        </div>
      );
    }
    case 'saved_query': {
      const events = data.events as TelemetryEvent[];
      return (
        <div>
          <ul className="mini-logs">
            {events.map((e) => (
              <li key={e.id}>
                <span className="muted">{formatDateTime(e.timestamp)}</span> <span className={`lvl-dot lvl-${(e.level ?? '').toLowerCase()}`} /> {e.message}
              </li>
            ))}
          </ul>
          <Link to={logsLink(query, range)} className="small">
            Open in Logs →
          </Link>
        </div>
      );
    }
  }
}

export function DashboardPage({ id }: { id: number }) {
  const { navigate } = useRouter();
  const [dash, setDash] = useState<Dashboard | null>(null);
  const [error, setError] = useState<unknown>(null);
  const [editing, setEditing] = useState<Panel | 'new' | null>(null);
  const [metrics, setMetrics] = useState<string[]>([]);
  const [range, setRange] = useState('1h');
  const [refresh, setRefresh] = useState(0);
  const [renaming, setRenaming] = useState<string | null>(null);

  const load = useCallback(() => {
    api.dashboard(id).then(setDash).catch(setError);
  }, [id]);
  useEffect(load, [load]);
  useEffect(() => {
    api
      .metricNames()
      .then((r) => setMetrics(r.metrics.map((m) => m.name)))
      .catch(() => setMetrics([]));
  }, []);
  useEffect(() => {
    const t = window.setInterval(() => setRefresh((n) => n + 1), 60_000);
    return () => window.clearInterval(t);
  }, []);

  if (error) return <div className="page"><ErrorBanner error={error} /></div>;
  if (!dash) return <div className="page"><Spinner /></div>;

  const save = async (d: Draft) => {
    const body = { title: d.title, kind: d.kind, config: configOf(d), width: d.width };
    if (editing === 'new') await api.addPanel(dash.id, body);
    else if (editing) await api.updatePanel(dash.id, editing.id, body);
    setEditing(null);
    load();
  };

  const move = async (p: Panel, delta: number) => {
    const ordered = [...dash.panels];
    const i = ordered.findIndex((x) => x.id === p.id);
    const j = i + delta;
    if (j < 0 || j >= ordered.length) return;
    [ordered[i], ordered[j]] = [ordered[j], ordered[i]];
    await Promise.all(ordered.map((x, pos) => (x.position === pos ? null : api.updatePanel(dash.id, x.id, { title: x.title, kind: x.kind, config: x.config, position: pos }))));
    load();
  };

  return (
    <div className="page">
      <div className="page-head">
        {renaming == null ? (
          <h1>
            {dash.name}{' '}
            <button className="link-btn small" onClick={() => setRenaming(dash.name)}>
              Rename
            </button>
          </h1>
        ) : (
          <form
            className="inline-form"
            onSubmit={async (e) => {
              e.preventDefault();
              await api.renameDashboard(dash.id, renaming);
              setRenaming(null);
              load();
            }}
          >
            <input className="input" aria-label="Dashboard name" value={renaming} onChange={(e) => setRenaming(e.target.value)} autoFocus />
            <button className="btn btn-primary">Save</button>
            <button type="button" className="btn" onClick={() => setRenaming(null)}>
              Cancel
            </button>
          </form>
        )}
        <div className="head-actions">
          <RangeSelect value={range} onChange={setRange} />
          <button className="btn" onClick={() => setRefresh((n) => n + 1)}>
            Refresh
          </button>
          <button className="btn btn-primary" onClick={() => setEditing('new')}>
            Add panel
          </button>
          <button
            className="btn btn-danger"
            onClick={async () => {
              if (!window.confirm(`Delete dashboard "${dash.name}"?`)) return;
              await api.deleteDashboard(dash.id);
              navigate('/dashboards');
            }}
          >
            Delete
          </button>
        </div>
      </div>
      {dash.panels.length === 0 && <Empty title="This dashboard is empty">Add a panel to get started.</Empty>}
      <div className="grid">
        {dash.panels.map((p, i) => (
          <div key={p.id} className="card panel" style={{ gridColumn: `span ${p.width}` }} data-testid="panel">
            <div className="card-head">
              <h3>{p.title}</h3>
              <div className="panel-actions">
                <button className="icon-btn" aria-label="Move left" disabled={i === 0} onClick={() => move(p, -1)}>
                  ←
                </button>
                <button className="icon-btn" aria-label="Move right" disabled={i === dash.panels.length - 1} onClick={() => move(p, 1)}>
                  →
                </button>
                <button className="icon-btn" aria-label="Edit panel" onClick={() => setEditing(p)}>
                  ✎
                </button>
                <button
                  className="icon-btn"
                  aria-label="Remove panel"
                  onClick={async () => {
                    await api.deletePanel(dash.id, p.id);
                    load();
                  }}
                >
                  ×
                </button>
              </div>
            </div>
            <PanelBody panel={p} range={range} refresh={refresh} />
          </div>
        ))}
      </div>
      {editing && <PanelEditor panel={editing === 'new' ? undefined : editing} metrics={metrics} onSave={save} onClose={() => setEditing(null)} />}
    </div>
  );
}
