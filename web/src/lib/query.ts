import type { Json } from './types';

/** A field name usable unquoted in the query language. */
const IDENT = /^[\p{L}_@$][\p{L}\p{N}_.\-@$]*$/u;
const KEYWORDS = new Set(['and', 'or', 'not', 'contains', 'true', 'false', 'null']);

/** Render a literal in query syntax. */
export function literal(v: Json): string {
  if (v === null) return 'null';
  if (typeof v === 'number' || typeof v === 'boolean') return String(v);
  if (typeof v === 'string') return `"${v.replace(/\\/g, '\\\\').replace(/"/g, '\\"').replace(/\n/g, '\\n')}"`;
  return `"${JSON.stringify(v).replace(/\\/g, '\\\\').replace(/"/g, '\\"')}"`;
}

export function isPlainField(name: string): boolean {
  return IDENT.test(name) && !KEYWORDS.has(name.toLowerCase());
}

/** `field = value` (or `!=`) for the filter panel / property actions. */
export function clause(field: string, value: Json, negate = false): string | null {
  if (!isPlainField(field)) return null;
  return `${field} ${negate ? '!=' : '='} ${literal(value)}`;
}

/** Combine the current query with another clause using `and`. */
export function andQuery(current: string, extra: string): string {
  const q = current.trim();
  if (!q) return extra;
  if (q.includes(extra)) return q;
  // Parenthesise queries containing `or` so precedence stays as intended.
  const needsParens = /\bor\b/i.test(q) && !(q.startsWith('(') && q.endsWith(')'));
  return `${needsParens ? `(${q})` : q} and ${extra}`;
}

export interface RangePreset {
  key: string;
  label: string;
  ms: number | null;
}

export const RANGE_PRESETS: RangePreset[] = [
  { key: '5m', label: 'Last 5 minutes', ms: 5 * 60_000 },
  { key: '15m', label: 'Last 15 minutes', ms: 15 * 60_000 },
  { key: '1h', label: 'Last hour', ms: 3_600_000 },
  { key: '6h', label: 'Last 6 hours', ms: 6 * 3_600_000 },
  { key: '24h', label: 'Last 24 hours', ms: 24 * 3_600_000 },
  { key: '7d', label: 'Last 7 days', ms: 7 * 86_400_000 },
  { key: '30d', label: 'Last 30 days', ms: 30 * 86_400_000 },
  { key: 'all', label: 'All time', ms: null },
];

/** Absolute window for a preset, evaluated now. */
export function rangeWindow(key: string, now = Date.now()): { from?: string; to?: string } {
  const p = RANGE_PRESETS.find((r) => r.key === key) ?? RANGE_PRESETS[2];
  if (p.ms == null) return {};
  return { from: new Date(now - p.ms).toISOString() };
}

/** Bounded window for charts (falls back to 30 days for "all"). */
export function boundedWindow(key: string, now = Date.now()): { from: string; to: string } {
  const p = RANGE_PRESETS.find((r) => r.key === key) ?? RANGE_PRESETS[2];
  const ms = p.ms ?? 30 * 86_400_000;
  return { from: new Date(now - ms).toISOString(), to: new Date(now + 1).toISOString() };
}
