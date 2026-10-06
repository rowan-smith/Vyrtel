import { useMemo, useState } from 'react';
import { useQuery } from '@tanstack/react-query';
import ReactECharts from 'echarts-for-react';
import { fetchMetricSeries, listMetrics } from '../api';
import { formatTime, presetRange, type TimePreset } from '../types';

export default function MetricsPage() {
  const [preset, setPreset] = useState<TimePreset>('6h');
  const [selected, setSelected] = useState<string | null>(null);
  const range = useMemo(() => presetRange(preset), [preset]);

  const metricsQuery = useQuery({
    queryKey: ['metrics', range.from, range.to],
    queryFn: () => listMetrics({ from: range.from, to: range.to }),
  });

  const metrics = metricsQuery.data?.metrics ?? [];
  const active = selected ?? metrics[0]?.name ?? null;

  const seriesQuery = useQuery({
    queryKey: ['metric-series', active, range.from, range.to],
    queryFn: () =>
      fetchMetricSeries({
        name: active!,
        from: range.from,
        to: range.to,
      }),
    enabled: !!active,
  });

  const chartOption = useMemo(() => {
    const points = seriesQuery.data?.points ?? [];
    return {
      backgroundColor: 'transparent',
      grid: { left: 48, right: 16, top: 20, bottom: 32 },
      xAxis: {
        type: 'category',
        data: points.map((p) => formatTime(p.timestamp)),
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
          type: 'line',
          data: points.map((p) => p.value),
          smooth: true,
          showSymbol: false,
          itemStyle: { color: '#6ea8d8' },
          areaStyle: { color: 'rgba(110,168,216,0.12)' },
        },
      ],
      tooltip: { trigger: 'axis' },
    };
  }, [seriesQuery.data]);

  return (
    <div className="page">
      <div className="page-header">
        <h1 className="page-title">Metrics</h1>
        <select className="select" value={preset} onChange={(e) => setPreset(e.target.value as TimePreset)}>
          <option value="1h">Last hour</option>
          <option value="6h">Last 6 hours</option>
          <option value="24h">Last 24 hours</option>
          <option value="7d">Last 7 days</option>
        </select>
      </div>

      <div style={{ display: 'grid', gridTemplateColumns: '320px 1fr', gap: '0.75rem', minHeight: 0, flex: 1 }}>
        <div className="trace-list">
          <table className="table">
            <thead>
              <tr>
                <th>Metric</th>
                <th>Last</th>
              </tr>
            </thead>
            <tbody>
              {metrics.map((m) => (
                <tr
                  key={m.name}
                  onClick={() => setSelected(m.name)}
                  style={active === m.name ? { background: 'var(--bg-active)' } : undefined}
                >
                  <td>
                    <div style={{ fontFamily: 'var(--mono)', fontSize: 12 }}>{m.name}</div>
                    <div className="muted" style={{ fontSize: 11 }}>
                      {m.service ?? '—'} · {m.pointCount} pts
                    </div>
                  </td>
                  <td style={{ fontFamily: 'var(--mono)' }}>
                    {m.lastValue != null ? formatMetric(m.lastValue, m.unit) : '—'}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
          {metrics.length === 0 && !metricsQuery.isLoading && (
            <div className="empty">No metrics in this range.</div>
          )}
        </div>

        <div className="widget" style={{ minHeight: 360 }}>
          <div className="widget-title">{active ?? 'Select a metric'}</div>
          {active && <ReactECharts option={chartOption} style={{ height: 320 }} opts={{ renderer: 'canvas' }} />}
          {!active && <div className="empty">Choose a metric to plot.</div>}
        </div>
      </div>
    </div>
  );
}

function formatMetric(value: number, unit?: string | null): string {
  const formatted =
    Math.abs(value) >= 1_000_000
      ? `${(value / 1_000_000).toFixed(2)}M`
      : Math.abs(value) >= 1000
        ? `${(value / 1000).toFixed(2)}k`
        : Number.isInteger(value)
          ? String(value)
          : value.toFixed(2);
  if (!unit || unit === '1') return formatted;
  return `${formatted} ${unit}`;
}
