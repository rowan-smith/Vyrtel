// Types mirroring the HTTP API (docs/api.md).

export type Json = null | boolean | number | string | Json[] | { [key: string]: Json };
export type Fields = { [key: string]: Json };

export type Level = 'Trace' | 'Debug' | 'Information' | 'Warning' | 'Error' | 'Fatal';

export interface ExceptionInfo {
  type?: string;
  message?: string;
  stackTrace?: string;
}

export interface SpanStatus {
  code: 'Unset' | 'Ok' | 'Error';
  message?: string;
}

export interface SpanEventItem {
  timestamp: string;
  name: string;
  attributes: Fields;
}

export interface TelemetryEvent {
  id: string;
  signal: 'log' | 'span' | 'metric';
  timestamp: string;
  observedTimestamp: string;
  service?: string;
  environment?: string;
  traceId?: string;
  spanId?: string;
  // logs
  level?: Level;
  message?: string;
  messageTemplate?: string;
  exception?: ExceptionInfo;
  // spans / metrics
  name?: string;
  parentSpanId?: string;
  spanKind?: string;
  durationMs?: number;
  durationNanos?: number;
  status?: SpanStatus;
  events?: SpanEventItem[];
  links?: { traceId: string; spanId: string; attributes: Fields }[];
  value?: number;
  unit?: string;
  properties: Fields;
  resource: Fields;
}

export interface IndexUse {
  field: string;
  kind: 'time' | 'bitmap' | 'bloom' | 'zonemap' | 'none';
}

export interface Diagnostics {
  elapsedMs: number;
  segmentsConsidered: number;
  segmentsSkipped: number;
  segmentsSkippedByTime: number;
  segmentsSkippedByIndex: number;
  segmentsNotNeeded: number;
  blocksConsidered: number;
  blocksSkipped: number;
  blocksRead: number;
  eventsExamined: number;
  eventsMatched: number;
  bytesRead: number;
  unsealedEventsScanned: number;
  indexes: IndexUse[];
}

export interface SearchResponse {
  events: TelemetryEvent[];
  diagnostics: Diagnostics;
  continuationToken: string | null;
}

export interface HistogramBucket {
  start: string;
  count: number;
  levels?: number[];
}

export interface HistogramResponse {
  from: string;
  to: string;
  stepMs: number;
  total: number;
  buckets: HistogramBucket[];
  diagnostics: Diagnostics;
}

export interface FacetValue {
  value: Json;
  count: number;
}

export interface Facet {
  field: string;
  count: number;
  values: FacetValue[];
  truncated: boolean;
}

export interface FacetsResponse {
  sampled: number;
  fields: Facet[];
}

export interface TraceSummary {
  traceId: string;
  rootName?: string;
  rootService?: string;
  start: string;
  durationMs: number;
  spanCount: number;
  services: string[];
  errorCount: number;
}

export interface TraceResponse {
  traceId: string;
  summary: TraceSummary;
  spans: TelemetryEvent[];
}

export interface MetricPoint {
  ts: string;
  value: number | null;
}

export interface MetricSeries {
  group: string | null;
  points: MetricPoint[];
}

export type Agg = 'sum' | 'count' | 'min' | 'max' | 'avg' | 'last';

export interface MetricResponse {
  name: string;
  agg: Agg;
  stepMs: number;
  unit?: string;
  kind?: string;
  description?: string;
  series: MetricSeries[];
}

export type PanelKind = 'log_count' | 'metric_line' | 'single_stat' | 'saved_query';

export interface Panel {
  id: number;
  dashboardId: number;
  title: string;
  kind: PanelKind;
  config: { [key: string]: Json };
  position: number;
  width: number;
  height: number;
}

export interface Dashboard {
  id: number;
  name: string;
  description?: string | null;
  createdAt: number;
  updatedAt: number;
  panels: Panel[];
}

export type AlertStatus = 'OK' | 'FIRING' | 'ERROR';

export interface AlertInput {
  name: string;
  query: string;
  op: string;
  threshold: number;
  windowSecs: number;
  intervalSecs: number;
  webhookUrl?: string | null;
  enabled: boolean;
}

export interface Alert extends AlertInput {
  id: number;
  createdAt: number;
  updatedAt: number;
  state: {
    status: AlertStatus;
    lastValue: number | null;
    lastEvaluatedAt: number | null;
    lastTransitionAt: number | null;
    lastError: string | null;
    lastNotificationAt: number | null;
    lastNotificationError: string | null;
  };
  history?: { from: string; to: string; value: number | null; message: string | null; at: number }[];
}

export interface ApiKey {
  id: number;
  name: string;
  prefix: string;
  scope: 'ingest' | 'admin';
  createdAt: number;
  lastUsedAt: number | null;
  key?: string;
}

export interface SignalStorage {
  events: number;
  segments: number;
  rawBytes: number;
  storedBytes: number;
  segmentBytes: number;
  indexBytes: number;
  walBytes: number;
  compressionRatio: number | null;
  activeEvents: number;
  queueDepth: number;
  queueCapacity: number;
  oldest: string | null;
  newest: string | null;
  healthy: boolean;
}

export interface StorageStats {
  rawBytes: number;
  storedBytes: number;
  segmentBytes: number;
  indexBytes: number;
  walBytes: number;
  metadataBytes: number;
  eventCount: number;
  segmentCount: number;
  compressionRatio: number | null;
  indexOverhead: number | null;
  receivedBytes: number;
  signals: { logs: SignalStorage; traces: SignalStorage; metrics: SignalStorage };
  indexCache: { budgetBytes: number; usedBytes: number; entries: number; hits: number; misses: number };
}

export interface SystemInfo {
  version: string;
  uptimeSeconds: number;
  startedAt: string;
  dataDir: string;
  durability: string;
  authEnabled: boolean;
  eventCounts: { logs: number; traces: number; metrics: number };
  storageBytes: number;
  memory: {
    limitBytes: number;
    rssBytes: number | null;
    indexCacheUsedBytes: number;
    budgets: { [key: string]: number | number[] };
  };
  queue: { [signal: string]: { depth: number; capacity: number } };
  liveSubscribers: number;
}

export interface QueryFieldStats {
  signal: string;
  field: string;
  index: string;
  queries: number;
  bytesRead: number;
  segmentsScanned: number;
  segmentsSkipped: number;
  totalMs: number;
  lastSeen: number;
}

export interface AuthState {
  authEnabled: boolean;
  user: string | null;
}
