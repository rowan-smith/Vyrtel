import type { Json, TelemetryEvent } from './types';

/** Fields every log event has (or commonly has), in display order. */
export const BUILTIN_COLUMNS = ['timestamp', 'level', 'service', 'message', 'environment', 'traceId', 'spanId'] as const;

/** Columns offered in the side panel's "Pinned" list (deliberately short for now). */
export const AVAILABLE_COLUMNS = ['timestamp', 'level', 'message'];

/** Columns shown in the log list until the user pins something else. */
export const DEFAULT_COLUMNS = ['timestamp', 'level', 'message'];

const LABELS: Record<string, string> = {
  timestamp: 'Timestamp',
  level: 'Level',
  service: 'Service',
  message: 'Message',
  environment: 'Environment',
  traceId: 'Trace ID',
  spanId: 'Span ID',
  'exception.type': 'Exception',
};

const ACRONYMS = new Set(['id', 'http', 'https', 'url', 'uri', 'api', 'ip', 'db', 'sql', 'os', 'cpu', 'ui', 'tls', 'dns']);

/** `customerId` → "Customer ID", `http.statusCode` → "HTTP status code". */
export function columnLabel(key: string): string {
  if (LABELS[key]) return LABELS[key];
  const words = key
    .replace(/([a-z0-9])([A-Z])/g, '$1 $2')
    .split(/[\s._-]+/)
    .filter(Boolean)
    .map((w) => w.toLowerCase());
  return words
    .map((w, i) => (ACRONYMS.has(w) ? w.toUpperCase() : i === 0 ? w[0].toUpperCase() + w.slice(1) : w))
    .join(' ');
}

/** A field of an event by column key: built-in fields first, then (nested or dotted) properties. */
export function columnValue(event: TelemetryEvent, key: string): Json | undefined {
  switch (key) {
    case 'timestamp':
      return event.timestamp;
    case 'level':
      return event.level;
    case 'service':
      return event.service;
    case 'message':
      return event.message;
    case 'environment':
      return event.environment;
    case 'traceId':
      return event.traceId;
    case 'spanId':
      return event.spanId;
    case 'exception.type':
      return event.exception?.type;
  }
  if (key in event.properties) return event.properties[key];
  let cur: Json | undefined = event.properties;
  for (const part of key.split('.')) {
    if (cur === null || typeof cur !== 'object' || Array.isArray(cur)) return undefined;
    cur = (cur as Record<string, Json>)[part];
  }
  return cur;
}

/** Pinned columns in display order: timestamp, level, service, extras, then message last. */
export function orderColumns(pinned: string[]): string[] {
  const set = new Set(pinned);
  const extras = pinned.filter((k) => !['timestamp', 'level', 'service', 'message'].includes(k));
  return [...['timestamp', 'level', 'service'].filter((k) => set.has(k)), ...extras, ...(set.has('message') ? ['message'] : [])];
}

/** CSS grid template for a row of these columns; the message (or the last column) takes the rest. */
export function columnTemplate(cols: string[]): string {
  return cols
    .map((k, i) => {
      if (k === 'message' || i === cols.length - 1) return 'minmax(0, 1fr)';
      if (k === 'timestamp') return 'max-content';
      if (k === 'level') return '52px';
      if (k === 'service') return 'minmax(60px, 140px)';
      if (k === 'traceId') return 'minmax(80px, 260px)';
      return 'minmax(60px, 170px)';
    })
    .join(' ');
}

const KEY = 'vyrtel.columns';

export function loadColumns(): string[] {
  try {
    const v = JSON.parse(localStorage.getItem(KEY) ?? 'null');
    // Only keep columns that are still offered, so nothing gets stuck on screen.
    const kept = Array.isArray(v) ? v.filter((x) => typeof x === 'string' && AVAILABLE_COLUMNS.includes(x)) : [];
    if (kept.length) return kept;
  } catch {
    /* storage unavailable or corrupt */
  }
  return DEFAULT_COLUMNS;
}

export function saveColumns(cols: string[]) {
  try {
    localStorage.setItem(KEY, JSON.stringify(cols));
  } catch {
    /* storage unavailable */
  }
}
