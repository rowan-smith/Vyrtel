export type LogLevel =
  | 'trace'
  | 'debug'
  | 'information'
  | 'warning'
  | 'error'
  | 'fatal';

export type EventType = 'log' | 'span' | 'metric';

export interface Event {
  id: string;
  timestamp: string;
  eventType: EventType;
  level?: LogLevel | null;
  message?: string | null;
  messageTemplate?: string | null;
  service?: string | null;
  environment?: string | null;
  traceId?: string | null;
  spanId?: string | null;
  parentSpanId?: string | null;
  durationNs?: number | null;
  stacktrace?: string | null;
  attributes: Record<string, unknown>;
}

export interface EventsResponse {
  events: Event[];
  nextCursor?: string | null;
}

export interface ApiErrorBody {
  error: {
    code: string;
    message: string;
    position?: number;
  };
}

export interface SavedFilter {
  id: string;
  name: string;
  query: string;
  createdAt: string;
}

export interface TraceSummary {
  traceId: string;
  timestamp: string;
  rootOperation?: string | null;
  service?: string | null;
  durationNs?: number | null;
  status: string;
  spanCount: number;
}

export interface TraceDetail {
  traceId: string;
  spans: Event[];
  logs: Event[];
}

export interface Dashboard {
  id: string;
  name: string;
  createdAt: string;
}

export interface WidgetPosition {
  x: number;
  y: number;
  w: number;
  h: number;
}

export type Visualization = 'number' | 'line' | 'bar' | 'table';

export interface DashboardWidget {
  id: string;
  dashboardId: string;
  title: string;
  query: string;
  visualization: Visualization;
  position: WidgetPosition;
}

export interface DashboardDetail extends Dashboard {
  widgets: DashboardWidget[];
}

export interface AggregationResult {
  type: 'number' | 'groups' | 'timeSeries' | 'table';
  value?: number;
  points?: { key: string; value: number }[];
  interval?: string;
  rows?: Record<string, unknown>[];
}

export interface AppSettings {
  retentionDays: number | null;
}

export interface AboutInfo {
  name: string;
  version: string;
  eventCount: number;
  storagePath: string;
  development: boolean;
}

export interface Account {
  id: string;
  username: string;
  displayName: string;
  createdAt: string;
}

export interface LoginResponse {
  token: string;
  account: Account;
  expiresAt: string;
}

export interface ApiKeyInfo {
  id: string;
  accountId: string;
  name: string;
  keyPrefix: string;
  createdAt: string;
}

export interface CreatedApiKey extends ApiKeyInfo {
  key: string;
}

export type TimePreset =
  | '5m'
  | '15m'
  | '30m'
  | '1h'
  | '6h'
  | '24h'
  | '7d'
  | 'custom';

export function levelShort(level?: LogLevel | null): string {
  switch (level) {
    case 'trace':
      return 'TRC';
    case 'debug':
      return 'DBG';
    case 'information':
      return 'INF';
    case 'warning':
      return 'WRN';
    case 'error':
      return 'ERR';
    case 'fatal':
      return 'FTL';
    default:
      return '   ';
  }
}

export function formatTime(iso: string): string {
  const d = new Date(iso);
  return d.toLocaleTimeString([], { hour12: false });
}

export function formatDuration(ns?: number | null): string {
  if (ns == null) return '—';
  if (ns < 1_000) return `${ns} ns`;
  if (ns < 1_000_000) return `${(ns / 1_000).toFixed(1)} µs`;
  if (ns < 1_000_000_000) return `${(ns / 1_000_000).toFixed(1)} ms`;
  return `${(ns / 1_000_000_000).toFixed(2)} s`;
}

export function presetRange(preset: TimePreset): { from?: string; to?: string } {
  if (preset === 'custom') return {};
  const to = new Date();
  const from = new Date(to);
  const map: Record<Exclude<TimePreset, 'custom'>, number> = {
    '5m': 5,
    '15m': 15,
    '30m': 30,
    '1h': 60,
    '6h': 360,
    '24h': 1440,
    '7d': 10080,
  };
  from.setMinutes(from.getMinutes() - map[preset]);
  return { from: from.toISOString(), to: to.toISOString() };
}

export function appendClause(existing: string, clause: string): string {
  const q = existing.trim();
  if (!q) return clause;
  return `${q} and ${clause}`;
}

export function quoteValue(value: unknown): string {
  if (typeof value === 'number' || typeof value === 'boolean') return String(value);
  if (value == null) return 'null';
  return `"${String(value).replace(/"/g, '\\"')}"`;
}

export function fieldClause(field: string, op: '=' | '!=', value: unknown): string {
  return `${field} ${op} ${quoteValue(value)}`;
}

export type AlertOperator = 'gt' | 'gte' | 'lt' | 'lte' | 'eq';
export type AlertStatus = 'ok' | 'firing';

export interface AlertView {
  id: string;
  name: string;
  query: string;
  operator: AlertOperator;
  threshold: number;
  enabled: boolean;
  createdAt: string;
  status: AlertStatus;
  value?: number | null;
  message?: string | null;
  lastEvaluatedAt?: string | null;
  lastChangedAt?: string | null;
}

export interface MetricSummary {
  name: string;
  service?: string | null;
  unit?: string | null;
  lastValue?: number | null;
  lastTimestamp?: string | null;
  pointCount: number;
}

export interface MetricSeriesPoint {
  timestamp: string;
  value: number;
  service?: string | null;
  attributes: Record<string, unknown>;
}

export interface MetricSeries {
  name: string;
  points: MetricSeriesPoint[];
}
