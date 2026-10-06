import type { Json, Level } from './types';

export function formatBytes(n: number | null | undefined): string {
  if (n == null || !Number.isFinite(n)) return '–';
  const units = ['B', 'KB', 'MB', 'GB', 'TB'];
  let v = n;
  let i = 0;
  while (v >= 1024 && i < units.length - 1) {
    v /= 1024;
    i++;
  }
  return i === 0 ? `${n} B` : `${v.toFixed(v >= 100 ? 0 : 1)} ${units[i]}`;
}

export function formatCount(n: number | null | undefined): string {
  if (n == null || !Number.isFinite(n)) return '–';
  if (n >= 1e9) return `${(n / 1e9).toFixed(1)}B`;
  if (n >= 1e6) return `${(n / 1e6).toFixed(1)}M`;
  if (n >= 1e4) return `${(n / 1e3).toFixed(1)}K`;
  return n.toLocaleString('en-US');
}

export function formatDuration(ms: number): string {
  if (!Number.isFinite(ms)) return '–';
  if (ms < 1) return `${(ms * 1000).toFixed(0)} µs`;
  if (ms < 1000) return `${ms < 10 ? ms.toFixed(1) : ms.toFixed(0)} ms`;
  if (ms < 60_000) return `${(ms / 1000).toFixed(2)} s`;
  return `${(ms / 60_000).toFixed(1)} min`;
}

export function formatUptime(secs: number): string {
  const d = Math.floor(secs / 86400);
  const h = Math.floor((secs % 86400) / 3600);
  const m = Math.floor((secs % 3600) / 60);
  if (d > 0) return `${d}d ${h}h`;
  if (h > 0) return `${h}h ${m}m`;
  return `${m}m ${secs % 60}s`;
}

const pad = (n: number, w = 2) => String(n).padStart(w, '0');

/** `10:42:17.231` in local time. */
export function formatTime(iso: string): string {
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return iso;
  return `${pad(d.getHours())}:${pad(d.getMinutes())}:${pad(d.getSeconds())}.${pad(d.getMilliseconds(), 3)}`;
}

/** `2026-10-06 10:42:17.231` in local time. */
export function formatDateTime(iso: string | number): string {
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return String(iso);
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())} ${formatTime(d.toISOString())}`;
}

export function isToday(iso: string): boolean {
  const d = new Date(iso);
  const n = new Date();
  return d.getFullYear() === n.getFullYear() && d.getMonth() === n.getMonth() && d.getDate() === n.getDate();
}

export const LEVELS: Level[] = ['Trace', 'Debug', 'Information', 'Warning', 'Error', 'Fatal'];

export function levelShort(level?: string): string {
  switch (level) {
    case 'Trace':
      return 'TRACE';
    case 'Debug':
      return 'DEBUG';
    case 'Information':
      return 'INFO';
    case 'Warning':
      return 'WARN';
    case 'Error':
      return 'ERROR';
    case 'Fatal':
      return 'FATAL';
    default:
      return level?.toUpperCase() ?? '';
  }
}

export function levelClass(level?: string): string {
  return `lvl-${(level ?? 'none').toLowerCase()}`;
}

/** Display a property value. Strings are shown raw; others as JSON. */
export function valueText(v: Json): string {
  if (typeof v === 'string') return v;
  return JSON.stringify(v);
}
