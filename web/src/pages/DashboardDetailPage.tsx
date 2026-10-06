import { useMemo, useState } from 'react';
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import ReactECharts from 'echarts-for-react';
import { Link, useParams } from 'react-router-dom';
import {
  createWidget,
  deleteWidget,
  getDashboard,
  runDashboardQuery,
  updateWidget,
} from '../api';
import { presetRange, type AggregationResult, type TimePreset, type Visualization } from '../types';

export default function DashboardDetailPage() {
  const { id = '' } = useParams();
  const qc = useQueryClient();
  const [editing, setEditing] = useState(false);
  const [preset, setPreset] = useState<TimePreset>('1h');
  const range = useMemo(() => presetRange(preset), [preset]);

  const dashboard = useQuery({
    queryKey: ['dashboard', id],
    queryFn: () => getDashboard(id),
    enabled: !!id,
  });

  const createMut = useMutation({
    mutationFn: (body: {
      title: string;
      query: string;
      visualization: Visualization;
    }) => createWidget(id, { ...body, position: { x: 0, y: 0, w: 1, h: 1 } }),
    onSuccess: () => qc.invalidateQueries({ queryKey: ['dashboard', id] }),
  });

  const deleteMut = useMutation({
    mutationFn: (widgetId: string) => deleteWidget(id, widgetId),
    onSuccess: () => qc.invalidateQueries({ queryKey: ['dashboard', id] }),
  });

  const widgets = dashboard.data?.widgets ?? [];

  return (
    <div className="page">
      <div className="page-header">
        <div>
          <div className="muted">
            <Link to="/dashboards">Dashboards</Link>
          </div>
          <h1 className="page-title">{dashboard.data?.name ?? 'Dashboard'}</h1>
        </div>
        <div style={{ display: 'flex', gap: 8 }}>
          <select className="select" value={preset} onChange={(e) => setPreset(e.target.value as TimePreset)}>
            <option value="1h">Last hour</option>
            <option value="6h">Last 6 hours</option>
            <option value="24h">Last 24 hours</option>
            <option value="7d">Last 7 days</option>
          </select>
          <button className={`btn${editing ? ' active' : ''}`} onClick={() => setEditing((v) => !v)}>
            {editing ? 'Done' : 'Edit'}
          </button>
          {editing && (
            <button
              className="btn btn-primary"
              onClick={() => {
                const title = window.prompt('Widget title', 'Errors');
                if (!title) return;
                const query = window.prompt('Query', 'level = error | count');
                if (!query) return;
                const visualization = (window.prompt('Visualization (number|line|bar|table)', 'number') ??
                  'number') as Visualization;
                createMut.mutate({ title, query, visualization });
              }}
            >
              Add Widget
            </button>
          )}
        </div>
      </div>

      <div className="widget-grid">
        {widgets.map((w) => (
          <WidgetCard
            key={w.id}
            dashboardId={id}
            widgetId={w.id}
            title={w.title}
            query={w.query}
            visualization={w.visualization}
            from={range.from}
            to={range.to}
            editing={editing}
            wide={w.visualization === 'line' || w.visualization === 'bar'}
            onDelete={() => deleteMut.mutate(w.id)}
            onEdit={() => {
              const title = window.prompt('Title', w.title);
              if (title == null) return;
              const query = window.prompt('Query', w.query);
              if (query == null) return;
              updateWidget(id, w.id, {
                title,
                query,
                visualization: w.visualization,
                position: w.position,
              }).then(() => qc.invalidateQueries({ queryKey: ['dashboard', id] }));
            }}
          />
        ))}
      </div>
      {widgets.length === 0 && <div className="empty">No widgets. Switch to Edit to add one.</div>}
    </div>
  );
}

function WidgetCard(props: {
  dashboardId: string;
  widgetId: string;
  title: string;
  query: string;
  visualization: Visualization;
  from?: string;
  to?: string;
  editing: boolean;
  wide?: boolean;
  onDelete: () => void;
  onEdit: () => void;
}) {
  const q = useQuery({
    queryKey: ['widget', props.dashboardId, props.widgetId, props.query, props.from, props.to],
    queryFn: () => runDashboardQuery(props.dashboardId, props.query, props.from, props.to),
    refetchInterval: 30_000,
  });

  const result = q.data?.result;

  return (
    <div className={`widget${props.wide ? ' wide' : ''}`}>
      <div className="widget-title" style={{ display: 'flex', justifyContent: 'space-between' }}>
        <span>{props.title}</span>
        {props.editing && (
          <span>
            <button className="btn btn-ghost" onClick={props.onEdit}>
              Edit
            </button>
            <button className="btn btn-ghost" onClick={props.onDelete}>
              Delete
            </button>
          </span>
        )}
      </div>
      {q.isError && <div className="query-error">{(q.error as Error).message}</div>}
      {result && <WidgetViz visualization={props.visualization} result={result} />}
    </div>
  );
}

function WidgetViz({
  visualization,
  result,
}: {
  visualization: Visualization;
  result: AggregationResult;
}) {
  if (visualization === 'number' || result.type === 'number') {
    const value = result.value ?? result.points?.[0]?.value ?? 0;
    return <div className="widget-number">{formatNumber(value)}</div>;
  }

  if (visualization === 'table' || result.type === 'table') {
    const rows = result.rows ?? result.points?.map((p) => ({ key: p.key, value: p.value })) ?? [];
    return (
      <table className="table">
        <tbody>
          {rows.map((row, i) => (
            <tr key={i}>
              {Object.entries(row).map(([k, v]) => (
                <td key={k}>{String(v)}</td>
              ))}
            </tr>
          ))}
        </tbody>
      </table>
    );
  }

  const points = result.points ?? [];
  const option = {
    backgroundColor: 'transparent',
    grid: { left: 40, right: 12, top: 16, bottom: 28 },
    xAxis: {
      type: 'category',
      data: points.map((p) => formatKey(p.key)),
      axisLabel: { color: '#8b939e', fontSize: 10 },
      axisLine: { lineStyle: { color: '#2f353c' } },
    },
    yAxis: {
      type: 'value',
      axisLabel: { color: '#8b939e', fontSize: 10 },
      splitLine: { lineStyle: { color: '#2f353c' } },
    },
    series: [
      {
        type: visualization === 'bar' ? 'bar' : 'line',
        data: points.map((p) => p.value),
        smooth: true,
        showSymbol: false,
        itemStyle: { color: '#6ea8d8' },
        areaStyle: visualization === 'line' ? { color: 'rgba(110,168,216,0.12)' } : undefined,
      },
    ],
    tooltip: { trigger: 'axis' },
  };

  return <ReactECharts option={option} style={{ height: 180 }} opts={{ renderer: 'canvas' }} />;
}

function formatNumber(n: number): string {
  return new Intl.NumberFormat().format(Math.round(n * 100) / 100);
}

function formatKey(key: string): string {
  if (key.includes('T')) {
    try {
      return new Date(key).toLocaleTimeString([], { hour12: false, hour: '2-digit', minute: '2-digit' });
    } catch {
      return key;
    }
  }
  return key;
}
