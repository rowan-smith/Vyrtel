import type {
  AboutInfo,
  Account,
  AggregationResult,
  AlertOperator,
  AlertView,
  ApiErrorBody,
  ApiKeyInfo,
  AppSettings,
  CreatedApiKey,
  Dashboard,
  DashboardDetail,
  DashboardWidget,
  EventsResponse,
  LoginResponse,
  MetricSeries,
  MetricSummary,
  SavedFilter,
  TraceDetail,
  TraceSummary,
  Visualization,
  WidgetPosition,
} from './types';

const TOKEN_KEY = 'observatory_token';

export function getToken(): string | null {
  return localStorage.getItem(TOKEN_KEY);
}

export function setToken(token: string | null) {
  if (token) localStorage.setItem(TOKEN_KEY, token);
  else localStorage.removeItem(TOKEN_KEY);
}

async function request<T>(path: string, init?: RequestInit): Promise<T> {
  const headers: Record<string, string> = {
    'Content-Type': 'application/json',
    ...(init?.headers as Record<string, string> | undefined),
  };
  const token = getToken();
  if (token) {
    headers['X-Observatory-Token'] = token;
  }

  const res = await fetch(path, {
    ...init,
    headers,
  });
  if (!res.ok) {
    let body: ApiErrorBody | undefined;
    try {
      body = await res.json();
    } catch {
      /* ignore */
    }
    const err = new Error(body?.error?.message ?? res.statusText) as Error & {
      api?: ApiErrorBody['error'];
      status?: number;
    };
    err.api = body?.error;
    err.status = res.status;
    if (res.status === 401 && !path.startsWith('/api/auth/login')) {
      setToken(null);
      if (!window.location.pathname.startsWith('/login')) {
        window.location.href = '/login';
      }
    }
    throw err;
  }
  if (res.status === 204) return undefined as T;
  return res.json();
}

export function login(username: string, password: string): Promise<LoginResponse> {
  return request('/api/auth/login', {
    method: 'POST',
    body: JSON.stringify({ username, password }),
  });
}

export function logout(): Promise<void> {
  return request('/api/auth/logout', { method: 'POST' });
}

export function fetchMe(): Promise<Account> {
  return request('/api/auth/me');
}

export function listAccounts(): Promise<Account[]> {
  return request('/api/accounts');
}

export function createAccount(body: {
  username: string;
  password: string;
  displayName?: string;
}): Promise<Account> {
  return request('/api/accounts', {
    method: 'POST',
    body: JSON.stringify(body),
  });
}

export function listApiKeys(): Promise<ApiKeyInfo[]> {
  return request('/api/accounts/api-keys');
}

export function createApiKey(name: string): Promise<CreatedApiKey> {
  return request('/api/accounts/api-keys', {
    method: 'POST',
    body: JSON.stringify({ name }),
  });
}

export function deleteApiKey(id: string): Promise<void> {
  return request(`/api/accounts/api-keys/${id}`, { method: 'DELETE' });
}

export function fetchEvents(params: {
  q?: string;
  from?: string;
  to?: string;
  limit?: number;
  cursor?: string;
}): Promise<EventsResponse> {
  const sp = new URLSearchParams();
  if (params.q) sp.set('q', params.q);
  if (params.from) sp.set('from', params.from);
  if (params.to) sp.set('to', params.to);
  if (params.limit) sp.set('limit', String(params.limit));
  if (params.cursor) sp.set('cursor', params.cursor);
  return request(`/api/events?${sp}`);
}

export function fetchQuery(params: {
  q: string;
  from?: string;
  to?: string;
}): Promise<{ result: AggregationResult } | EventsResponse> {
  const sp = new URLSearchParams();
  sp.set('q', params.q);
  if (params.from) sp.set('from', params.from);
  if (params.to) sp.set('to', params.to);
  return request(`/api/query?${sp}`);
}

export function listFilters(): Promise<SavedFilter[]> {
  return request('/api/filters');
}

export function createFilter(name: string, query: string): Promise<SavedFilter> {
  return request('/api/filters', {
    method: 'POST',
    body: JSON.stringify({ name, query }),
  });
}

export function deleteFilter(id: string): Promise<void> {
  return request(`/api/filters/${id}`, { method: 'DELETE' });
}

export function listTraces(params: {
  from?: string;
  to?: string;
  q?: string;
}): Promise<{ traces: TraceSummary[] }> {
  const sp = new URLSearchParams();
  if (params.from) sp.set('from', params.from);
  if (params.to) sp.set('to', params.to);
  if (params.q) sp.set('q', params.q);
  return request(`/api/traces?${sp}`);
}

export function getTrace(traceId: string): Promise<TraceDetail> {
  return request(`/api/traces/${encodeURIComponent(traceId)}`);
}

export function listDashboards(): Promise<Dashboard[]> {
  return request('/api/dashboards');
}

export function getDashboard(id: string): Promise<DashboardDetail> {
  return request(`/api/dashboards/${id}`);
}

export function createDashboard(name: string): Promise<Dashboard> {
  return request('/api/dashboards', {
    method: 'POST',
    body: JSON.stringify({ name }),
  });
}

export function deleteDashboard(id: string): Promise<void> {
  return request(`/api/dashboards/${id}`, { method: 'DELETE' });
}

export function createWidget(
  dashboardId: string,
  body: {
    title: string;
    query: string;
    visualization: Visualization;
    position?: WidgetPosition;
  },
): Promise<DashboardWidget> {
  return request(`/api/dashboards/${dashboardId}/widgets`, {
    method: 'POST',
    body: JSON.stringify(body),
  });
}

export function updateWidget(
  dashboardId: string,
  widgetId: string,
  body: {
    title: string;
    query: string;
    visualization: Visualization;
    position: WidgetPosition;
  },
): Promise<void> {
  return request(`/api/dashboards/${dashboardId}/widgets/${widgetId}`, {
    method: 'PUT',
    body: JSON.stringify(body),
  });
}

export function deleteWidget(dashboardId: string, widgetId: string): Promise<void> {
  return request(`/api/dashboards/${dashboardId}/widgets/${widgetId}`, {
    method: 'DELETE',
  });
}

export function runDashboardQuery(
  dashboardId: string,
  q: string,
  from?: string,
  to?: string,
): Promise<{ result: AggregationResult }> {
  const sp = new URLSearchParams({ q });
  if (from) sp.set('from', from);
  if (to) sp.set('to', to);
  return request(`/api/dashboards/${dashboardId}/query?${sp}`);
}

export function getSettings(): Promise<AppSettings> {
  return request('/api/settings');
}

export function updateSettings(retentionDays: number | null): Promise<AppSettings> {
  return request('/api/settings', {
    method: 'PUT',
    body: JSON.stringify({ retentionDays }),
  });
}

export function getAbout(): Promise<AboutInfo> {
  return request('/api/about');
}

export function listMetrics(params: {
  from?: string;
  to?: string;
}): Promise<{ metrics: MetricSummary[] }> {
  const sp = new URLSearchParams();
  if (params.from) sp.set('from', params.from);
  if (params.to) sp.set('to', params.to);
  return request(`/api/metrics?${sp}`);
}

export function fetchMetricSeries(params: {
  name: string;
  from?: string;
  to?: string;
  service?: string;
}): Promise<MetricSeries> {
  const sp = new URLSearchParams({ name: params.name });
  if (params.from) sp.set('from', params.from);
  if (params.to) sp.set('to', params.to);
  if (params.service) sp.set('service', params.service);
  return request(`/api/metrics/series?${sp}`);
}

export function listAlerts(): Promise<AlertView[]> {
  return request('/api/alerts');
}

export function createAlert(body: {
  name: string;
  query: string;
  operator: AlertOperator;
  threshold: number;
}): Promise<AlertView> {
  return request('/api/alerts', {
    method: 'POST',
    body: JSON.stringify(body),
  });
}

export function deleteAlert(id: string): Promise<void> {
  return request(`/api/alerts/${id}`, { method: 'DELETE' });
}

export function setAlertEnabled(id: string, enabled: boolean): Promise<void> {
  return request(`/api/alerts/${id}/enabled`, {
    method: 'POST',
    body: JSON.stringify({ enabled }),
  });
}
