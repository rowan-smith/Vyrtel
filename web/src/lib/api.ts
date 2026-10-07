import type {
  Agg,
  Alert,
  AlertInput,
  ApiKey,
  AuthState,
  Dashboard,
  FacetsResponse,
  HistogramResponse,
  MetricResponse,
  Panel,
  PanelKind,
  QueryFieldStats,
  SearchResponse,
  StorageStats,
  SystemInfo,
  TraceResponse,
  TraceSummary,
  Diagnostics,
  Json,
} from './types';

/** Error returned by the API: `{"error": {"code", "message", ...}}`. */
export class ApiError extends Error {
  readonly status: number;
  readonly code: string;
  readonly position?: number;

  constructor(status: number, code: string, message: string, position?: number) {
    super(message);
    this.status = status;
    this.code = code;
    this.position = position;
  }
}

/** Called when the server says we are not logged in. */
let onUnauthorized: (() => void) | null = null;
export function setUnauthorizedHandler(fn: (() => void) | null) {
  onUnauthorized = fn;
}

async function request<T>(method: string, path: string, body?: unknown, signal?: AbortSignal): Promise<T> {
  let res: Response;
  try {
    res = await fetch(path, {
      method,
      headers: body === undefined ? undefined : { 'content-type': 'application/json' },
      body: body === undefined ? undefined : JSON.stringify(body),
      credentials: 'same-origin',
      signal,
    });
  } catch (e) {
    if ((e as Error).name === 'AbortError') throw e;
    throw new ApiError(0, 'network', 'Cannot reach the Vyrtel server.');
  }
  if (res.status === 204) return undefined as T;
  const text = await res.text();
  let data: unknown = undefined;
  if (text) {
    try {
      data = JSON.parse(text);
    } catch {
      data = undefined;
    }
  }
  if (!res.ok) {
    const err = (data as { error?: { code?: string; message?: string; position?: number } } | undefined)?.error;
    if (res.status === 401 && onUnauthorized && !path.startsWith('/api/v1/auth/')) onUnauthorized();
    throw new ApiError(res.status, err?.code ?? 'http_error', err?.message ?? `Request failed (${res.status})`, err?.position);
  }
  return data as T;
}

export const get = <T>(path: string, signal?: AbortSignal) => request<T>('GET', path, undefined, signal);
export const post = <T>(path: string, body: unknown, signal?: AbortSignal) => request<T>('POST', path, body, signal);
export const put = <T>(path: string, body: unknown) => request<T>('PUT', path, body);
export const del = (path: string) => request<void>('DELETE', path);

export interface TimeWindow {
  from?: string;
  to?: string;
}

export const api = {
  me: () => get<AuthState>('/api/v1/auth/me'),
  login: (username: string, password: string) => post<AuthState>('/api/v1/auth/login', { username, password }),
  logout: () => post<{ ok: boolean }>('/api/v1/auth/logout', {}),

  searchLogs: (
    q: { query: string; limit?: number; direction?: 'backward' | 'forward'; continuationToken?: string | null } & TimeWindow,
    signal?: AbortSignal,
  ) => post<SearchResponse>('/api/v1/query/logs', q, signal),
  histogram: (signalName: string, q: { query: string; buckets?: number } & TimeWindow, signal?: AbortSignal) =>
    post<HistogramResponse>(`/api/v1/query/${signalName}/histogram`, q, signal),
  count: (signalName: string, q: { query: string } & TimeWindow, signal?: AbortSignal) =>
    post<{ count: number; diagnostics: Diagnostics }>(`/api/v1/query/${signalName}/count`, q, signal),
  facets: (signalName: string, q: { query: string; sample?: number } & TimeWindow, signal?: AbortSignal) =>
    post<FacetsResponse>(`/api/v1/query/${signalName}/facets`, q, signal),

  searchTraces: (q: { query: string; limit?: number } & TimeWindow, signal?: AbortSignal) =>
    post<{ traces: TraceSummary[]; diagnostics: Diagnostics }>('/api/v1/query/traces', q, signal),
  trace: (id: string) => get<TraceResponse>(`/api/v1/traces/${encodeURIComponent(id)}`),

  metricNames: () => get<{ metrics: { name: string }[] }>('/api/v1/metrics'),
  metricQuery: (
    q: { name: string; agg: Agg; query?: string; groupBy?: string; stepMs?: number } & TimeWindow,
    signal?: AbortSignal,
  ) => post<MetricResponse>('/api/v1/query/metrics', q, signal),

  dashboards: () => get<{ dashboards: Dashboard[] }>('/api/v1/dashboards'),
  dashboard: (id: number) => get<Dashboard>(`/api/v1/dashboards/${id}`),
  createDashboard: (name: string) => post<Dashboard>('/api/v1/dashboards', { name }),
  renameDashboard: (id: number, name: string) => put<Dashboard>(`/api/v1/dashboards/${id}`, { name }),
  deleteDashboard: (id: number) => del(`/api/v1/dashboards/${id}`),
  addPanel: (id: number, p: { title: string; kind: PanelKind; config: { [k: string]: Json }; width?: number }) =>
    post<Panel>(`/api/v1/dashboards/${id}/panels`, p),
  updatePanel: (
    id: number,
    panelId: number,
    p: { title: string; kind: PanelKind; config: { [k: string]: Json }; width?: number; position?: number },
  ) => put<Panel>(`/api/v1/dashboards/${id}/panels/${panelId}`, p),
  deletePanel: (id: number, panelId: number) => del(`/api/v1/dashboards/${id}/panels/${panelId}`),

  savedQueries: () => get<{ savedQueries: { id: number; name: string; signal: string; query: string }[] }>('/api/v1/saved-queries'),
  saveQuery: (name: string, query: string) => post('/api/v1/saved-queries', { name, query, signal: 'logs' }),
  deleteSavedQuery: (id: number) => del(`/api/v1/saved-queries/${id}`),

  alerts: () => get<{ alerts: Alert[] }>('/api/v1/alerts'),
  alert: (id: number) => get<Alert>(`/api/v1/alerts/${id}`),
  createAlert: (a: AlertInput) => post<Alert>('/api/v1/alerts', a),
  updateAlert: (id: number, a: AlertInput) => put<Alert>(`/api/v1/alerts/${id}`, a),
  deleteAlert: (id: number) => del(`/api/v1/alerts/${id}`),
  evaluateAlert: (id: number) => post<Alert>(`/api/v1/alerts/${id}/evaluate`, {}),

  apiKeys: () => get<{ apiKeys: ApiKey[] }>('/api/v1/api-keys'),
  createApiKey: (name: string, scope: 'ingest' | 'admin') => post<ApiKey>('/api/v1/api-keys', { name, scope }),
  revokeApiKey: (id: number) => del(`/api/v1/api-keys/${id}`),

  storage: () => get<StorageStats>('/api/v1/system/storage'),
  info: () => get<SystemInfo>('/api/v1/system/info'),
  queryStats: () => get<{ fields: QueryFieldStats[] }>('/api/v1/system/query-stats'),
};
