// Small dependency-free SVG charts. They size to their container width via
// viewBox and preserveAspectRatio="none" for the plot area.

import type { HistogramBucket, MetricSeries } from '../lib/types';
import { formatCount, formatDateTime } from '../lib/format';

const LEVEL_COLORS = ['var(--c-none)', 'var(--c-trace)', 'var(--c-debug)', 'var(--c-info)', 'var(--c-warn)', 'var(--c-error)', 'var(--c-fatal)'];

/** Count-over-time bars, stacked by level when level counts are present. */
export function HistogramChart({ buckets, height = 64 }: { buckets: HistogramBucket[]; height?: number }) {
  const max = Math.max(1, ...buckets.map((b) => b.count));
  const w = 1000;
  const bw = w / Math.max(1, buckets.length);
  return (
    <svg className="histogram" viewBox={`0 0 ${w} ${height}`} preserveAspectRatio="none" role="img" aria-label="Events over time" height={height}>
      {buckets.map((b, i) => {
        const x = i * bw;
        const title = `${formatDateTime(b.start)} — ${formatCount(b.count)} events`;
        if (!b.levels) {
          const h = (b.count / max) * (height - 2);
          return (
            <rect key={i} x={x + 0.5} y={height - h} width={Math.max(bw - 1, 0.5)} height={h} className="bar">
              <title>{title}</title>
            </rect>
          );
        }
        let y = height;
        return (
          <g key={i}>
            <title>{title}</title>
            {b.levels.map((n, li) => {
              if (!n) return null;
              const h = (n / max) * (height - 2);
              y -= h;
              return <rect key={li} x={x + 0.5} y={y} width={Math.max(bw - 1, 0.5)} height={h} fill={LEVEL_COLORS[li]} />;
            })}
          </g>
        );
      })}
    </svg>
  );
}

// Aurora series palette (--series-0..7 in styles.css, with light-mode variants).
const SERIES_COUNT = 8;

export function seriesColor(i: number) {
  return `var(--series-${i % SERIES_COUNT})`;
}

/** Multi-series line chart with a simple Y axis. */
export function LineChart({ series, height = 220, unit }: { series: MetricSeries[]; height?: number; unit?: string }) {
  const w = 1000;
  const padL = 56;
  const padB = 20;
  const values = series.flatMap((s) => s.points.map((p) => p.value).filter((v): v is number => v != null));
  if (!values.length) return <div className="muted pad">No data points in this range.</div>;
  const times = series.flatMap((s) => s.points.map((p) => Date.parse(p.ts)));
  const t0 = Math.min(...times);
  const t1 = Math.max(...times);
  const min = Math.min(0, ...values);
  const max = Math.max(...values);
  const span = max - min || 1;
  const x = (t: number) => padL + ((t - t0) / Math.max(1, t1 - t0)) * (w - padL - 8);
  const y = (v: number) => 6 + (1 - (v - min) / span) * (height - padB - 12);
  const ticks = [min, min + span / 2, max];
  return (
    <svg className="linechart" viewBox={`0 0 ${w} ${height}`} role="img" aria-label="Metric chart" width="100%" height={height}>
      {ticks.map((t, i) => (
        <g key={i}>
          <line x1={padL} x2={w} y1={y(t)} y2={y(t)} className="grid" />
          <text x={padL - 6} y={y(t) + 4} textAnchor="end" className="axis">
            {formatCount(Math.round(t * 100) / 100)}
            {unit && i === 2 ? ` ${unit}` : ''}
          </text>
        </g>
      ))}
      <text x={padL} y={height - 4} className="axis">
        {formatDateTime(new Date(t0).toISOString())}
      </text>
      <text x={w - 8} y={height - 4} textAnchor="end" className="axis">
        {formatDateTime(new Date(t1).toISOString())}
      </text>
      {series.map((s, si) => {
        // Break the line where buckets have no data.
        const parts: string[] = [];
        let cur = '';
        for (const p of s.points) {
          if (p.value == null) {
            if (cur) parts.push(cur);
            cur = '';
            continue;
          }
          cur += `${cur ? 'L' : 'M'}${x(Date.parse(p.ts)).toFixed(1)},${y(p.value).toFixed(1)}`;
        }
        if (cur) parts.push(cur);
        return (
          <g key={si}>
            {parts.map((d, i) => (
              <path key={i} d={d} fill="none" stroke={seriesColor(si)} strokeWidth={2} vectorEffect="non-scaling-stroke" />
            ))}
            {s.points.map((p, i) =>
              p.value == null ? null : (
                <circle key={i} cx={x(Date.parse(p.ts))} cy={y(p.value)} r={2.5} fill={seriesColor(si)}>
                  <title>{`${s.group ?? ''} ${formatDateTime(p.ts)}: ${p.value}`}</title>
                </circle>
              ),
            )}
          </g>
        );
      })}
    </svg>
  );
}
