import { useEffect, useMemo, useRef, useState, type MouseEvent } from 'react';
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import ReactECharts from 'echarts-for-react';
import { Link } from 'react-router-dom';
import {
  createFilter,
  deleteFilter,
  fetchEvents,
  fetchQuery,
  getToken,
  listFilters,
} from '../api';
import StacktraceView, {
  resolveStacktrace,
  isStacktraceKey,
} from '../components/StacktraceView';
import {
  appendClause,
  fieldClause,
  presetRange,
  type AggregationResult,
  type Event,
  type LogLevel,
  type TimePreset,
} from '../types';

const LEVELS: { id: LogLevel; label: string }[] = [
  { id: 'trace', label: 'Trace' },
  { id: 'debug', label: 'Debug' },
  { id: 'information', label: 'Info' },
  { id: 'warning', label: 'Warning' },
  { id: 'error', label: 'Error' },
  { id: 'fatal', label: 'Fatal' },
];

const PRESETS: { id: TimePreset; label: string }[] = [
  { id: '5m', label: '5 minutes' },
  { id: '15m', label: '15 minutes' },
  { id: '30m', label: '30 minutes' },
  { id: '1h', label: '1 hour' },
  { id: '6h', label: '6 hours' },
  { id: '24h', label: '24 hours' },
  { id: '7d', label: '7 days' },
];

const DEFAULT_SERVICES = ['api', 'billing', 'database', 'worker', 'auth', 'gateway', 'scheduler'];

type MenuState = {
  x: number;
  y: number;
  field: string;
  value: unknown;
};

interface CustomFilterItem {
  id: string;
  field: string;
  op: string;
  value: string;
}

function chartInterval(preset: TimePreset): string {
  switch (preset) {
    case '5m':
    case '15m':
      return '1m';
    case '30m':
    case '1h':
      return '5m';
    case '6h':
      return '15m';
    case '24h':
      return '1h';
    case '7d':
      return '6h';
    default:
      return '5m';
  }
}

function formatLogTimestamp(iso: string): string {
  try {
    const d = new Date(iso);
    const h = String(d.getHours()).padStart(2, '0');
    const m = String(d.getMinutes()).padStart(2, '0');
    const s = String(d.getSeconds()).padStart(2, '0');
    const ms = String(d.getMilliseconds()).padStart(3, '0');
    return `${h}:${m}:${s}.${ms}`;
  } catch {
    return iso;
  }
}

function LevelIndicator({ level }: { level?: LogLevel | null }) {
  const norm = (level ?? 'information').toLowerCase();
  let label = 'INFO';
  let colorVar = 'var(--info)';

  if (norm === 'error') {
    label = 'ERROR';
    colorVar = 'var(--error)';
  } else if (norm === 'warning' || norm === 'warn') {
    label = 'WARN';
    colorVar = 'var(--warn)';
  } else if (norm === 'information' || norm === 'info') {
    label = 'INFO';
    colorVar = 'var(--info)';
  } else if (norm === 'debug') {
    label = 'DEBUG';
    colorVar = 'var(--debug)';
  } else if (norm === 'trace') {
    label = 'TRACE';
    colorVar = 'var(--trace)';
  } else if (norm === 'fatal') {
    label = 'FATAL';
    colorVar = 'var(--fatal)';
  }

  return (
    <div className="event-level-indicator" style={{ color: colorVar }}>
      <span className="event-level-dot" style={{ backgroundColor: colorVar }} />
      <span className="event-level-text">{label}</span>
    </div>
  );
}

function PropertyTable({
  event,
  onValueClick,
}: {
  event: Event;
  onValueClick: (field: string, value: unknown, ev: MouseEvent) => void;
}) {
  const rows: [string, unknown][] = [
    ['timestamp', event.timestamp],
    ['level', (event.level ?? 'info').toUpperCase()],
    ['service', event.service ?? '—'],
    ['message', event.message ?? '—'],
  ];
  if (event.traceId) rows.push(['trace_id', event.traceId]);
  if (event.spanId) rows.push(['span_id', event.spanId]);
  if (event.parentSpanId) rows.push(['parent_span_id', event.parentSpanId]);
  if (event.durationNs != null) {
    rows.push(['duration_ms', Math.round((event.durationNs / 1_000_000) * 100) / 100]);
  }
  for (const [k, v] of Object.entries(event.attributes ?? {})) {
    if (isStacktraceKey(k)) continue;
    rows.push([k, v]);
  }

  return (
    <div className="prop-grid">
      {rows.map(([k, v]) => {
        const isLevel = k === 'level';
        const isTrace = k === 'trace_id' && typeof v === 'string';
        const strVal = typeof v === 'string' ? v : JSON.stringify(v);
        const levelLower = String(v).toLowerCase();
        let valColor: string | undefined;
        if (isLevel) {
          if (levelLower.includes('error')) valColor = 'var(--error)';
          else if (levelLower.includes('warn')) valColor = 'var(--warn)';
          else if (levelLower.includes('info')) valColor = 'var(--info)';
          else if (levelLower.includes('debug')) valColor = 'var(--debug)';
          else if (levelLower.includes('trace')) valColor = 'var(--trace)';
        }

        return (
          <div key={k} className="prop-row">
            <span className="prop-key">{k}</span>
            <span
              className={`prop-val${isTrace ? ' prop-val-link' : ''}`}
              style={valColor ? { color: valColor, fontWeight: 600 } : undefined}
              onClick={(ev) => {
                if (isTrace) return;
                onValueClick(k, v, ev);
              }}
            >
              {isTrace ? (
                <Link to={`/traces/${encodeURIComponent(v as string)}`} className="trace-link">
                  {strVal}
                </Link>
              ) : (
                strVal
              )}
            </span>
          </div>
        );
      })}
    </div>
  );
}

export default function LogsPage() {
  const queryClient = useQueryClient();
  const inputRef = useRef<HTMLInputElement>(null);
  const [draft, setDraft] = useState('');
  const [query, setQuery] = useState('');
  const [preset, setPreset] = useState<TimePreset>('1h');
  const [levelFilters, setLevelFilters] = useState<Set<LogLevel>>(new Set());
  const [serviceFilters, setServiceFilters] = useState<Set<string>>(new Set());
  const [serviceSearch, setServiceSearch] = useState('');
  const [customFilters, setCustomFilters] = useState<CustomFilterItem[]>([]);
  const [customCombinator, setCustomCombinator] = useState<'and' | 'or'>('and');
  const [live, setLive] = useState(false);
  const [showChart, setShowChart] = useState(true);
  const [filterPanel, setFilterPanel] = useState(true);
  const [expanded, setExpanded] = useState<string | null>(null);
  const [detailTab, setDetailTab] = useState<'properties' | 'raw' | 'json'>('properties');
  const [copiedId, setCopiedId] = useState<string | null>(null);
  const [stackExpanded, setStackExpanded] = useState<Record<string, boolean>>({});
  const [menu, setMenu] = useState<MenuState | null>(null);
  const [liveEvents, setLiveEvents] = useState<Event[]>([]);
  const [frozenEvents, setFrozenEvents] = useState<Event[] | null>(null);
  const [queryError, setQueryError] = useState<{
    message: string;
    position?: number;
    query: string;
  } | null>(null);

  const range = useMemo(() => presetRange(preset), [preset]);

  const effectiveQuery = useMemo(() => {
    let q = appendClause(query.trim(), 'event_type = log');
    if (levelFilters.size > 0) {
      const parts = [...levelFilters].map((l) => `level = ${l}`);
      q = appendClause(q, `(${parts.join(' or ')})`);
    }
    if (serviceFilters.size > 0) {
      const parts = [...serviceFilters].map((s) => `service = "${s}"`);
      q = appendClause(q, `(${parts.join(' or ')})`);
    }
    if (customFilters.length > 0) {
      const valid = customFilters.filter((f) => f.field && f.value);
      if (valid.length > 0) {
        const parts = valid.map((f) => {
          const val = Number.isNaN(Number(f.value)) ? `"${f.value}"` : f.value;
          return `${f.field} ${f.op} ${val}`;
        });
        q = appendClause(q, `(${parts.join(` ${customCombinator} `)})`);
      }
    }
    return q;
  }, [query, levelFilters, serviceFilters, customFilters, customCombinator]);

  const eventsQuery = useQuery({
    queryKey: ['logs', effectiveQuery, range.from, range.to],
    queryFn: async () => {
      try {
        const res = await fetchEvents({
          q: effectiveQuery,
          from: range.from,
          to: range.to,
          limit: 200,
        });
        setQueryError(null);
        return res;
      } catch (err) {
        const e = err as Error & { api?: { message: string; position?: number } };
        if (e.api) {
          setQueryError({
            message: e.api.message,
            position: e.api.position,
            query: effectiveQuery,
          });
        }
        throw err;
      }
    },
  });

  const baseEvents = eventsQuery.data?.events ?? [];

  const availableServices = useMemo(() => {
    const s = new Set<string>(DEFAULT_SERVICES);
    baseEvents.forEach((e) => {
      if (e.service) s.add(e.service);
    });
    liveEvents.forEach((e) => {
      if (e.service) s.add(e.service);
    });
    return Array.from(s);
  }, [baseEvents, liveEvents]);

  const filteredServicesList = useMemo(() => {
    if (!serviceSearch.trim()) return availableServices;
    const term = serviceSearch.toLowerCase();
    return availableServices.filter((s) => s.toLowerCase().includes(term));
  }, [availableServices, serviceSearch]);

  const chartQuery = useQuery({
    queryKey: ['logs-chart', effectiveQuery, range.from, range.to, preset],
    queryFn: async () => {
      const interval = chartInterval(preset);
      const res = await fetchQuery({
        q: `${effectiveQuery} | count by time(${interval})`,
        from: range.from,
        to: range.to,
      });
      return res as { result: AggregationResult };
    },
    enabled: showChart,
  });

  const filtersQuery = useQuery({
    queryKey: ['filters'],
    queryFn: listFilters,
  });

  const saveMutation = useMutation({
    mutationFn: ({ name, q }: { name: string; q: string }) => createFilter(name, q),
    onSuccess: () => queryClient.invalidateQueries({ queryKey: ['filters'] }),
  });

  const deleteMutation = useMutation({
    mutationFn: deleteFilter,
    onSuccess: () => queryClient.invalidateQueries({ queryKey: ['filters'] }),
  });

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (
        e.key === '/' &&
        !(e.target instanceof HTMLInputElement || e.target instanceof HTMLTextAreaElement)
      ) {
        e.preventDefault();
        inputRef.current?.focus();
      }
      if (e.key === 'Escape') {
        setExpanded(null);
        setMenu(null);
      }
      if ((e.ctrlKey || e.metaKey) && e.key === 'Enter') {
        runQuery();
      }
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  });

  useEffect(() => {
    if (!live) return;
    const token = getToken();
    const url = token
      ? `/api/events/stream?token=${encodeURIComponent(token)}`
      : '/api/events/stream';
    const es = new EventSource(url);
    es.addEventListener('event', (msg) => {
      try {
        const event = JSON.parse((msg as MessageEvent).data) as Event;
        if (event.eventType && event.eventType !== 'log') return;
        if (levelFilters.size > 0 && (!event.level || !levelFilters.has(event.level))) return;
        if (serviceFilters.size > 0 && (!event.service || !serviceFilters.has(event.service))) {
          return;
        }
        setLiveEvents((prev) => [event, ...prev].slice(0, 300));
        setFrozenEvents(null);
      } catch {
        /* ignore */
      }
    });
    return () => es.close();
  }, [live, levelFilters, serviceFilters]);

  const runQuery = () => {
    setQuery(draft);
    setLiveEvents([]);
    setFrozenEvents(null);
    setExpanded(null);
  };

  const toggleLive = () => {
    if (live) {
      const seen = new Set(liveEvents.map((e) => e.id));
      const snap =
        liveEvents.length > 0
          ? [...liveEvents, ...baseEvents.filter((e) => !seen.has(e.id))]
          : baseEvents;
      setFrozenEvents(snap);
      setLive(false);
    } else {
      setFrozenEvents(null);
      setLive(true);
    }
  };

  const displayEvents = useMemo(() => {
    if (!live && frozenEvents) return frozenEvents;
    if (!live || liveEvents.length === 0) return baseEvents;
    const seen = new Set(liveEvents.map((e) => e.id));
    return [...liveEvents, ...baseEvents.filter((e) => !seen.has(e.id))];
  }, [live, frozenEvents, liveEvents, baseEvents]);

  const chartOption = useMemo(() => {
    const result = chartQuery.data?.result;
    const points = result?.points ?? [];
    return {
      backgroundColor: 'transparent',
      animation: false,
      grid: { left: 16, right: 16, top: 4, bottom: 20 },
      xAxis: {
        type: 'category',
        data: points.map((p) => {
          const d = new Date(p.key);
          const hh = String(d.getHours()).padStart(2, '0');
          const mm = String(d.getMinutes()).padStart(2, '0');
          return `${hh}:${mm}`;
        }),
        axisLabel: { color: '#8b939e', fontSize: 10 },
        axisLine: { lineStyle: { color: '#1e293b' } },
        axisTick: { show: false },
      },
      yAxis: {
        type: 'value',
        show: false,
        splitLine: { show: false },
      },
      series: [
        {
          type: 'bar',
          data: points.map((p, idx) => {
            let color = '#3b82f6';
            if (idx % 7 === 2) color = '#ef4444';
            else if (idx % 11 === 4) color = '#f59e0b';
            return {
              value: p.value,
              itemStyle: {
                color,
                borderRadius: [2, 2, 0, 0],
              },
            };
          }),
          barMaxWidth: 10,
        },
      ],
      tooltip: {
        trigger: 'axis',
        backgroundColor: '#0f172a',
        borderColor: '#1e293b',
        textStyle: { color: '#f8fafc', fontSize: 12 },
      },
    };
  }, [chartQuery.data]);

  const applyInclude = (field: string, value: unknown) => {
    const clause = fieldClause(field, '=', value);
    const next = appendClause(draft || query, clause);
    setDraft(next);
    setQuery(next);
    setLiveEvents([]);
    setFrozenEvents(null);
    setMenu(null);
  };

  const applyExclude = (field: string, value: unknown) => {
    const clause = fieldClause(field, '!=', value);
    const next = appendClause(draft || query, clause);
    setDraft(next);
    setQuery(next);
    setLiveEvents([]);
    setFrozenEvents(null);
    setMenu(null);
  };

  const toggleLevel = (level: LogLevel) => {
    setLevelFilters((prev) => {
      const next = new Set(prev);
      if (next.has(level)) next.delete(level);
      else next.add(level);
      return next;
    });
    setLiveEvents([]);
    setFrozenEvents(null);
  };

  const toggleService = (svc: string) => {
    setServiceFilters((prev) => {
      const next = new Set(prev);
      if (next.has(svc)) next.delete(svc);
      else next.add(svc);
      return next;
    });
    setLiveEvents([]);
    setFrozenEvents(null);
  };

  const handleCopyEvent = (event: Event, tab: 'properties' | 'raw' | 'json') => {
    let text = '';
    if (tab === 'raw') {
      text = event.message ?? JSON.stringify(event);
    } else if (tab === 'json') {
      text = JSON.stringify(event, null, 2);
    } else {
      const lines = [
        `timestamp\t${event.timestamp}`,
        `level\t${(event.level ?? 'info').toUpperCase()}`,
        `service\t${event.service ?? ''}`,
        `message\t${event.message ?? ''}`,
      ];
      if (event.traceId) lines.push(`trace_id\t${event.traceId}`);
      if (event.spanId) lines.push(`span_id\t${event.spanId}`);
      for (const [k, v] of Object.entries(event.attributes ?? {})) {
        if (!isStacktraceKey(k)) lines.push(`${k}\t${typeof v === 'string' ? v : JSON.stringify(v)}`);
      }
      const stack = resolveStacktrace(event);
      if (stack) lines.push(`\nStack trace:\n${stack}`);
      text = lines.join('\n');
    }
    navigator.clipboard.writeText(text);
    setCopiedId(event.id);
    setTimeout(() => setCopiedId(null), 1800);
  };

  const addCustomFilter = () => {
    setCustomFilters((prev) => [
      ...prev,
      {
        id: Math.random().toString(36).substring(2, 9),
        field: 'service',
        op: '=',
        value: '',
      },
    ]);
  };

  const updateCustomFilter = (id: string, updates: Partial<CustomFilterItem>) => {
    setCustomFilters((prev) =>
      prev.map((item) => (item.id === id ? { ...item, ...updates } : item)),
    );
  };

  const removeCustomFilter = (id: string) => {
    setCustomFilters((prev) => prev.filter((item) => item.id !== id));
  };

  const handleSaveView = () => {
    const name = window.prompt('Save view as:');
    if (!name) return;
    saveMutation.mutate({ name, q: draft || query || effectiveQuery });
  };

  return (
    <div className={`page logs-page${filterPanel ? ' filters-open' : ''}`}>
      {/* Search Bar Row */}
      <div className="logs-search-row">
        <div className="search-input-wrapper">
          <svg
            className="search-icon"
            width="16"
            height="16"
            viewBox="0 0 24 24"
            fill="none"
            stroke="currentColor"
            strokeWidth="2"
            strokeLinecap="round"
            strokeLinejoin="round"
          >
            <circle cx="11" cy="11" r="8" />
            <line x1="21" y1="21" x2="16.65" y2="16.65" />
          </svg>
          <input
            ref={inputRef}
            className="query-input"
            placeholder='service = "api" and level >= warning'
            value={draft}
            onChange={(e) => setDraft(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === 'Enter' && !e.ctrlKey && !e.metaKey) runQuery();
            }}
          />
        </div>

        <button className="run-btn" title="Run query" onClick={runQuery}>
          <svg width="15" height="15" viewBox="0 0 24 24" fill="currentColor">
            <polygon points="5 3 19 12 5 21 5 3" />
          </svg>
        </button>

        <button
          type="button"
          className={`live-btn${live ? ' on' : ''}`}
          title={live ? 'Live streaming (click to pause)' : 'Paused (click to stream live)'}
          onClick={toggleLive}
          aria-pressed={live}
        >
          <span className="live-dot" />
          <span>Live</span>
        </button>

        <button
          type="button"
          className={`filter-toggle-btn${filterPanel ? ' active' : ''}`}
          title={filterPanel ? 'Collapse filters' : 'Expand filters'}
          onClick={() => setFilterPanel((v) => !v)}
        >
          <svg
            width="16"
            height="16"
            viewBox="0 0 24 24"
            fill="none"
            stroke="currentColor"
            strokeWidth="2"
            strokeLinecap="round"
            strokeLinejoin="round"
          >
            {filterPanel ? <polyline points="15 18 9 12 15 6" /> : <polyline points="9 18 15 12 9 6" />}
          </svg>
        </button>
      </div>

      {queryError && (
        <div className="query-error">
          {queryError.query}
          {'\n'}
          {' '.repeat(queryError.position ?? 0)}^
          {'\n'}
          {queryError.message}
        </div>
      )}

      {/* Histogram Bar Chart */}
      <div className="logs-chart-wrapper">
        <button
          type="button"
          className="chart-collapse-btn"
          title={showChart ? 'Hide chart' : 'Show chart'}
          onClick={() => setShowChart((v) => !v)}
        >
          <svg
            width="14"
            height="14"
            viewBox="0 0 24 24"
            fill="none"
            stroke="currentColor"
            strokeWidth="2"
            strokeLinecap="round"
            strokeLinejoin="round"
          >
            {showChart ? <polyline points="6 9 12 15 18 9" /> : <polyline points="9 18 15 12 9 6" />}
          </svg>
        </button>

        {showChart && (
          <div className="logs-chart-inner">
            {chartQuery.isLoading && <div className="muted chart-empty">Loading chart…</div>}
            {!chartQuery.isLoading && (chartQuery.data?.result?.points?.length ?? 0) === 0 && (
              <div className="muted chart-empty">No chart data</div>
            )}
            {(chartQuery.data?.result?.points?.length ?? 0) > 0 && (
              <ReactECharts option={chartOption} style={{ height: '100%', width: '100%' }} />
            )}
          </div>
        )}

        <select
          className="preset-select"
          value={preset}
          onChange={(e) => {
            setPreset(e.target.value as TimePreset);
            setLiveEvents([]);
            setFrozenEvents(null);
          }}
        >
          {PRESETS.map((p) => (
            <option key={p.id} value={p.id}>
              {p.label}
            </option>
          ))}
        </select>
      </div>

      {/* Logs Table and Sidebar */}
      <div className="logs-body" onClick={() => setMenu(null)}>
        <div className="logs-main">
          {/* Table Header */}
          <div className="logs-table-header">
            <div />
            <div>Time</div>
            <div>Level</div>
            <div>Service</div>
            <div>Message</div>
          </div>

          <div className="logs-list">
            {displayEvents.length === 0 && !eventsQuery.isLoading && (
              <div className="empty" style={{ padding: '2rem 1rem' }}>
                No logs match this query.
              </div>
            )}

            {displayEvents.map((event) => {
              const open = expanded === event.id;
              const stack = resolveStacktrace(event);
              const isStackOpen = stackExpanded[event.id] ?? true;

              return (
                <div key={event.id} className={`event-row${open ? ' expanded' : ''}`}>
                  <button
                    type="button"
                    className="event-row-main"
                    onClick={() => setExpanded(open ? null : event.id)}
                  >
                    <div className="event-chevron">
                      <svg
                        width="12"
                        height="12"
                        viewBox="0 0 24 24"
                        fill="none"
                        stroke="currentColor"
                        strokeWidth="2.5"
                        strokeLinecap="round"
                        strokeLinejoin="round"
                      >
                        {open ? <polyline points="6 9 12 15 18 9" /> : <polyline points="9 18 15 12 9 6" />}
                      </svg>
                    </div>
                    <div className="event-time">{formatLogTimestamp(event.timestamp)}</div>
                    <LevelIndicator level={event.level} />
                    <div className="event-service">{event.service ?? '—'}</div>
                    <div className="event-message">{event.message ?? '(no message)'}</div>
                  </button>

                  {open && (
                    <div className="event-details" onClick={(e) => e.stopPropagation()}>
                      {/* Sub-tab Navigation */}
                      <div className="details-tab-bar">
                        <div className="details-tabs">
                          <button
                            type="button"
                            className={`details-tab-btn${detailTab === 'properties' ? ' active' : ''}`}
                            onClick={() => setDetailTab('properties')}
                          >
                            Properties
                          </button>
                          <button
                            type="button"
                            className={`details-tab-btn${detailTab === 'raw' ? ' active' : ''}`}
                            onClick={() => setDetailTab('raw')}
                          >
                            Raw
                          </button>
                          <button
                            type="button"
                            className={`details-tab-btn${detailTab === 'json' ? ' active' : ''}`}
                            onClick={() => setDetailTab('json')}
                          >
                            JSON
                          </button>
                        </div>

                        <button
                          type="button"
                          className="details-copy-btn"
                          onClick={() => handleCopyEvent(event, detailTab)}
                        >
                          {copiedId === event.id ? (
                            <>
                              <svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="#22c55e" strokeWidth="2.5">
                                <polyline points="20 6 9 17 4 12" />
                              </svg>
                              <span style={{ color: '#22c55e' }}>Copied</span>
                            </>
                          ) : (
                            <>
                              <svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
                                <rect x="9" y="9" width="13" height="13" rx="2" ry="2" />
                                <path d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1" />
                              </svg>
                              <span>Copy</span>
                            </>
                          )}
                        </button>
                      </div>

                      {/* Tab 1: Properties */}
                      {detailTab === 'properties' && (
                        <div>
                          <PropertyTable
                            event={event}
                            onValueClick={(field, value, ev) => {
                              setMenu({ x: ev.clientX, y: ev.clientY, field, value });
                            }}
                          />

                          {stack && (
                            <div className="stacktrace-section">
                              <button
                                type="button"
                                className="stacktrace-toggle"
                                onClick={() =>
                                  setStackExpanded((prev) => ({
                                    ...prev,
                                    [event.id]: !isStackOpen,
                                  }))
                                }
                              >
                                <svg
                                  width="12"
                                  height="12"
                                  viewBox="0 0 24 24"
                                  fill="none"
                                  stroke="currentColor"
                                  strokeWidth="2.5"
                                >
                                  {isStackOpen ? (
                                    <polyline points="6 9 12 15 18 9" />
                                  ) : (
                                    <polyline points="9 18 15 12 9 6" />
                                  )}
                                </svg>
                                <span>Stack trace</span>
                              </button>

                              {isStackOpen && (
                                <div className="stacktrace-container">
                                  <StacktraceView stacktrace={stack} />
                                </div>
                              )}
                            </div>
                          )}
                        </div>
                      )}

                      {/* Tab 2: Raw */}
                      {detailTab === 'raw' && (
                        <div className="raw-container">
                          {event.message ?? JSON.stringify(event, null, 2)}
                        </div>
                      )}

                      {/* Tab 3: JSON */}
                      {detailTab === 'json' && (
                        <div className="json-container">
                          {JSON.stringify(event, null, 2)}
                        </div>
                      )}
                    </div>
                  )}
                </div>
              );
            })}
          </div>
        </div>

        {/* Right Filters Sidebar */}
        {filterPanel && (
          <aside className="filters-sidebar">
            <div className="filters-header">
              <h2 className="filters-title">Filters</h2>
              <button
                type="button"
                className="filters-close-btn"
                title="Collapse filters"
                onClick={() => setFilterPanel(false)}
              >
                <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
                  <polyline points="15 18 9 12 15 6" />
                </svg>
              </button>
            </div>

            {/* Levels Section */}
            <div className="filter-group">
              <h3 className="filter-group-title">Levels</h3>
              <div className="filter-checkbox-list">
                {LEVELS.map((l) => (
                  <label key={l.id} className="filter-checkbox-item">
                    <input
                      type="checkbox"
                      checked={levelFilters.has(l.id)}
                      onChange={() => toggleLevel(l.id)}
                    />
                    <span>{l.label}</span>
                  </label>
                ))}
              </div>
            </div>

            {/* Services Section */}
            <div className="filter-group">
              <h3 className="filter-group-title">Services</h3>
              <div className="service-search-wrapper">
                <svg
                  className="service-search-icon"
                  width="13"
                  height="13"
                  viewBox="0 0 24 24"
                  fill="none"
                  stroke="currentColor"
                  strokeWidth="2"
                >
                  <circle cx="11" cy="11" r="8" />
                  <line x1="21" y1="21" x2="16.65" y2="16.65" />
                </svg>
                <input
                  type="text"
                  className="service-search-input"
                  placeholder="Search services..."
                  value={serviceSearch}
                  onChange={(e) => setServiceSearch(e.target.value)}
                />
              </div>

              <div className="filter-checkbox-list">
                {filteredServicesList.map((svc) => (
                  <label key={svc} className="filter-checkbox-item">
                    <input
                      type="checkbox"
                      checked={serviceFilters.has(svc)}
                      onChange={() => toggleService(svc)}
                    />
                    <span>{svc}</span>
                  </label>
                ))}
              </div>
            </div>

            {/* Custom filters Section */}
            <div className="filter-group">
              <h3 className="filter-group-title">Custom filters</h3>
              {customFilters.map((f, idx) => (
                <div key={f.id}>
                  {idx > 0 && (
                    <div className="custom-filter-combinator">
                      <select
                        className="custom-filter-select"
                        style={{ width: '60px', padding: '0.2rem 0.35rem' }}
                        value={customCombinator}
                        onChange={(e) => setCustomCombinator(e.target.value as 'and' | 'or')}
                      >
                        <option value="and">and</option>
                        <option value="or">or</option>
                      </select>
                    </div>
                  )}
                  <div className="custom-filter-row">
                    <select
                      className="custom-filter-select"
                      style={{ width: '38%' }}
                      value={f.field}
                      onChange={(e) => updateCustomFilter(f.id, { field: e.target.value })}
                    >
                      <option value="service">service</option>
                      <option value="level">level</option>
                      <option value="environment">environment</option>
                      <option value="message">message</option>
                      <option value="trace_id">trace_id</option>
                      <option value="http_method">http_method</option>
                      <option value="http_path">http_path</option>
                    </select>

                    <select
                      className="custom-filter-select"
                      style={{ width: '22%' }}
                      value={f.op}
                      onChange={(e) => updateCustomFilter(f.id, { op: e.target.value })}
                    >
                      <option value="=">=</option>
                      <option value="!=">!=</option>
                      <option value=">=">&gt;=</option>
                      <option value=">">&gt;</option>
                      <option value="<=">&lt;=</option>
                      <option value="<">&lt;</option>
                    </select>

                    <input
                      type="text"
                      className="custom-filter-input"
                      style={{ width: '32%' }}
                      value={f.value}
                      placeholder="value"
                      onChange={(e) => updateCustomFilter(f.id, { value: e.target.value })}
                    />

                    <button
                      type="button"
                      className="custom-filter-remove"
                      onClick={() => removeCustomFilter(f.id)}
                    >
                      ×
                    </button>
                  </div>
                </div>
              ))}

              <button type="button" className="add-filter-btn" onClick={addCustomFilter}>
                + Add filter
              </button>
            </div>

            {/* Saved Views Section */}
            {(filtersQuery.data?.length ?? 0) > 0 && (
              <div className="filter-group">
                <h3 className="filter-group-title">Saved views</h3>
                <ul className="settings-list" style={{ margin: 0 }}>
                  {filtersQuery.data?.map((f) => (
                    <li key={f.id} className="settings-list-row">
                      <button
                        type="button"
                        className="btn btn-ghost"
                        style={{ padding: '0.2rem 0.4rem', fontSize: '12px' }}
                        title={f.query}
                        onClick={() => {
                          setDraft(f.query);
                          setQuery(f.query);
                          setLiveEvents([]);
                          setFrozenEvents(null);
                        }}
                      >
                        {f.name}
                      </button>
                      <button
                        type="button"
                        className="custom-filter-remove"
                        onClick={() => {
                          if (window.confirm(`Delete filter “${f.name}”?`)) {
                            deleteMutation.mutate(f.id);
                          }
                        }}
                      >
                        ×
                      </button>
                    </li>
                  ))}
                </ul>
              </div>
            )}

            {/* Save View Button */}
            <button type="button" className="save-view-btn" onClick={handleSaveView}>
              Save as view
            </button>
          </aside>
        )}
      </div>

      {menu && (
        <div
          className="menu"
          style={{ left: menu.x, top: menu.y }}
          onClick={(e) => e.stopPropagation()}
        >
          <button type="button" onClick={() => applyInclude(menu.field, menu.value)}>
            Include
          </button>
          <button type="button" onClick={() => applyExclude(menu.field, menu.value)}>
            Exclude
          </button>
          <button
            type="button"
            onClick={() => {
              navigator.clipboard.writeText(String(menu.value));
              setMenu(null);
            }}
          >
            Copy value
          </button>
        </div>
      )}
    </div>
  );
}
