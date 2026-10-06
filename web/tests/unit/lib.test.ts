import { describe, expect, it } from 'vitest';

import { formatBytes, formatCount, formatDuration, levelShort } from '../../src/lib/format';
import { andQuery, clause, isPlainField, literal, rangeWindow } from '../../src/lib/query';
import { matchPath } from '../../src/lib/router';
import { nanosSince, spanTree } from '../../src/pages/TraceDetailPage';
import { logEvent } from './helpers';

describe('query helpers', () => {
  it('renders literals in query syntax', () => {
    expect(literal('a "quoted" \\ value')).toBe('"a \\"quoted\\" \\\\ value"');
    expect(literal(481)).toBe('481');
    expect(literal(true)).toBe('true');
    expect(literal(null)).toBe('null');
  });

  it('builds clauses only for expressible fields', () => {
    expect(clause('service', 'payments')).toBe('service = "payments"');
    expect(clause('http.statusCode', 500, true)).toBe('http.statusCode != 500');
    expect(clause('has space', 'x')).toBeNull();
    expect(clause('and', 'x')).toBeNull();
    expect(isPlainField('@t')).toBe(true);
  });

  it('combines queries with and, parenthesising or', () => {
    expect(andQuery('', 'a = 1')).toBe('a = 1');
    expect(andQuery('b = 2', 'a = 1')).toBe('b = 2 and a = 1');
    expect(andQuery('b = 2 or c = 3', 'a = 1')).toBe('(b = 2 or c = 3) and a = 1');
    expect(andQuery('a = 1', 'a = 1')).toBe('a = 1');
  });

  it('computes time windows', () => {
    const now = Date.parse('2026-10-06T12:00:00Z');
    expect(rangeWindow('1h', now)).toEqual({ from: '2026-10-06T11:00:00.000Z' });
    expect(rangeWindow('all', now)).toEqual({});
  });
});

describe('formatting', () => {
  it('formats sizes, counts and durations', () => {
    expect(formatBytes(512)).toBe('512 B');
    expect(formatBytes(1843210)).toBe('1.8 MB');
    expect(formatCount(18_400_000)).toBe('18.4M');
    expect(formatCount(4812)).toBe('4,812');
    expect(formatDuration(41)).toBe('41 ms');
    expect(formatDuration(0.5)).toBe('500 µs');
    expect(levelShort('Information')).toBe('INFO');
  });
});

describe('router', () => {
  it('matches parameterised paths', () => {
    expect(matchPath('/traces/:id', '/traces/abc')).toEqual({ id: 'abc' });
    expect(matchPath('/traces/:id', '/traces')).toBeNull();
    expect(matchPath('/dashboards/:id', '/alerts/1')).toBeNull();
  });
});

describe('trace tree', () => {
  it('orders spans parent-first with depth', () => {
    const span = (id: string, parent: string | undefined, ts: string) =>
      logEvent({ id, signal: 'span', spanId: id, parentSpanId: parent, timestamp: ts, name: id });
    const rows = spanTree([
      span('c', 'a', '2026-01-01T00:00:00.300Z'),
      span('b', 'a', '2026-01-01T00:00:00.100Z'),
      span('a', undefined, '2026-01-01T00:00:00.000Z'),
      span('d', 'b', '2026-01-01T00:00:00.200Z'),
      span('orphan', 'missing', '2026-01-01T00:00:00.050Z'),
    ]);
    expect(rows.map((r) => `${r.span.id}:${r.depth}`)).toEqual(['a:0', 'b:1', 'd:2', 'c:1', 'orphan:0']);
  });

  it('keeps nanosecond precision relative to a base second', () => {
    const base = Math.floor(Date.parse('2026-01-01T00:00:00Z') / 1000);
    expect(nanosSince('2026-01-01T00:00:00.000000123Z', base)).toBe(123);
    expect(nanosSince('2026-01-01T00:00:01.5Z', base)).toBe(1_500_000_000);
  });
});
