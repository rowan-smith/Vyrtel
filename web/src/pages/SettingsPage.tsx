import { useCallback, useEffect, useState } from 'react';

import { api } from '../lib/api';
import type { ApiKey, QueryFieldStats, StorageStats, SystemInfo } from '../lib/types';
import { formatBytes, formatCount, formatDateTime, formatDuration, formatUptime } from '../lib/format';
import { useRouter } from '../lib/router';
import { ErrorBanner, Spinner, Stat } from '../components/common';

const TABS = [
  ['storage', 'Storage'],
  ['system', 'System'],
  ['keys', 'API keys'],
  ['queries', 'Query statistics'],
] as const;

function StorageTab() {
  const [s, setS] = useState<StorageStats | null>(null);
  const [error, setError] = useState<unknown>(null);
  useEffect(() => {
    api.storage().then(setS).catch(setError);
  }, []);
  if (error) return <ErrorBanner error={error} />;
  if (!s) return <Spinner />;
  return (
    <div>
      <div className="stats" data-testid="storage-stats">
        <Stat label="Raw ingested" value={formatBytes(s.rawBytes)} hint="Uncompressed size in Vyrtel's binary event encoding" />
        <Stat label="Stored" value={formatBytes(s.storedBytes)} hint="Segments + indexes + WAL + metadata on disk" />
        <Stat label="Compression" value={s.compressionRatio ? `${s.compressionRatio.toFixed(1)}×` : '–'} hint="Sealed segments: raw / (data + index)" />
        <Stat label="Segments" value={formatCount(s.segmentCount)} />
        <Stat label="Events" value={formatCount(s.eventCount)} />
      </div>
      <table className="table">
        <thead>
          <tr>
            <th>Signal</th>
            <th className="num">Events</th>
            <th className="num">Segments</th>
            <th className="num">Raw</th>
            <th className="num">Data</th>
            <th className="num">Indexes</th>
            <th className="num">WAL</th>
            <th className="num">Ratio</th>
            <th>Oldest</th>
          </tr>
        </thead>
        <tbody>
          {(['logs', 'traces', 'metrics'] as const).map((k) => {
            const x = s.signals[k];
            return (
              <tr key={k}>
                <td>{k}</td>
                <td className="num">{formatCount(x.events)}</td>
                <td className="num">{x.segments}</td>
                <td className="num">{formatBytes(x.rawBytes)}</td>
                <td className="num">{formatBytes(x.segmentBytes)}</td>
                <td className="num">{formatBytes(x.indexBytes)}</td>
                <td className="num">{formatBytes(x.walBytes)}</td>
                <td className="num">{x.compressionRatio ? `${x.compressionRatio.toFixed(1)}×` : '–'}</td>
                <td className="nowrap">{x.oldest ? formatDateTime(x.oldest) : '–'}</td>
              </tr>
            );
          })}
        </tbody>
      </table>
      <p className="muted small">
        Index overhead {s.indexOverhead != null ? `${(s.indexOverhead * 100).toFixed(1)}%` : '–'} of compressed data · metadata{' '}
        {formatBytes(s.metadataBytes)} · index cache {formatBytes(s.indexCache.usedBytes)} / {formatBytes(s.indexCache.budgetBytes)} ({s.indexCache.hits} hits,{' '}
        {s.indexCache.misses} misses) · received since start {formatBytes(s.receivedBytes)}
      </p>
    </div>
  );
}

function SystemTab() {
  const [i, setI] = useState<SystemInfo | null>(null);
  const [error, setError] = useState<unknown>(null);
  useEffect(() => {
    api.info().then(setI).catch(setError);
  }, []);
  if (error) return <ErrorBanner error={error} />;
  if (!i) return <Spinner />;
  const b = i.memory.budgets;
  return (
    <table className="table kv-table">
      <tbody>
        <tr>
          <th>Version</th>
          <td>{i.version}</td>
        </tr>
        <tr>
          <th>Uptime</th>
          <td>{formatUptime(i.uptimeSeconds)}</td>
        </tr>
        <tr>
          <th>Data directory</th>
          <td className="mono">{i.dataDir}</td>
        </tr>
        <tr>
          <th>Durability</th>
          <td>{i.durability}</td>
        </tr>
        <tr>
          <th>Authentication</th>
          <td>{i.authEnabled ? 'enabled' : 'disabled'}</td>
        </tr>
        <tr>
          <th>Events</th>
          <td>
            logs {formatCount(i.eventCounts.logs)} · traces {formatCount(i.eventCounts.traces)} · metrics {formatCount(i.eventCounts.metrics)}
          </td>
        </tr>
        <tr>
          <th>Memory limit</th>
          <td>
            {formatBytes(i.memory.limitBytes)} {i.memory.rssBytes != null && <span className="muted">(resident {formatBytes(i.memory.rssBytes)})</span>}
          </td>
        </tr>
        <tr>
          <th>Memory budgets</th>
          <td>
            active segments {formatBytes(b.activeSegments as number)} · index cache {formatBytes(b.indexCache as number)} · ingest {formatBytes(b.ingest as number)} · queries{' '}
            {formatBytes(b.queries as number)} · background {formatBytes(b.background as number)}
          </td>
        </tr>
        <tr>
          <th>Ingest queues</th>
          <td>
            {Object.entries(i.queue).map(([k, q]) => (
              <span key={k} className="tag">
                {k} {q.depth}/{q.capacity}
              </span>
            ))}
          </td>
        </tr>
        <tr>
          <th>Live subscribers</th>
          <td>{i.liveSubscribers}</td>
        </tr>
      </tbody>
    </table>
  );
}

function KeysTab() {
  const [keys, setKeys] = useState<ApiKey[] | null>(null);
  const [error, setError] = useState<unknown>(null);
  const [name, setName] = useState('');
  const [scope, setScope] = useState<'ingest' | 'admin'>('ingest');
  const [created, setCreated] = useState<ApiKey | null>(null);
  const load = useCallback(() => {
    api
      .apiKeys()
      .then((r) => setKeys(r.apiKeys))
      .catch(setError);
  }, []);
  useEffect(load, [load]);
  return (
    <div>
      <p className="muted">
        Send keys as <code>Authorization: Bearer &lt;key&gt;</code> or <code>X-Api-Key</code>. Ingest keys can only send data; admin keys can use the whole API. Keys are
        enforced when authentication is enabled.
      </p>
      <form
        className="inline-form"
        onSubmit={async (e) => {
          e.preventDefault();
          try {
            setCreated(await api.createApiKey(name, scope));
            setName('');
            load();
          } catch (err) {
            setError(err);
          }
        }}
      >
        <input className="input" aria-label="Key name" placeholder="Key name, e.g. checkout-service" value={name} onChange={(e) => setName(e.target.value)} required />
        <select className="select" aria-label="Scope" value={scope} onChange={(e) => setScope(e.target.value as 'ingest' | 'admin')}>
          <option value="ingest">ingest</option>
          <option value="admin">admin</option>
        </select>
        <button className="btn btn-primary">Create key</button>
      </form>
      {created?.key && (
        <div className="notice" role="status">
          Copy this key now — it will not be shown again:
          <pre className="mono">{created.key}</pre>
        </div>
      )}
      <ErrorBanner error={error} />
      <table className="table">
        <thead>
          <tr>
            <th>Name</th>
            <th>Prefix</th>
            <th>Scope</th>
            <th>Created</th>
            <th>Last used</th>
            <th />
          </tr>
        </thead>
        <tbody>
          {keys?.map((k) => (
            <tr key={k.id}>
              <td>{k.name}</td>
              <td className="mono">{k.prefix}…</td>
              <td>{k.scope}</td>
              <td>{formatDateTime(k.createdAt)}</td>
              <td>{k.lastUsedAt ? formatDateTime(k.lastUsedAt) : 'never'}</td>
              <td>
                <button
                  className="btn btn-small btn-danger"
                  onClick={async () => {
                    if (window.confirm(`Revoke key "${k.name}"?`)) {
                      await api.revokeApiKey(k.id);
                      load();
                    }
                  }}
                >
                  Revoke
                </button>
              </td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}

function QueriesTab() {
  const [rows, setRows] = useState<QueryFieldStats[] | null>(null);
  const [error, setError] = useState<unknown>(null);
  useEffect(() => {
    api
      .queryStats()
      .then((r) => setRows(r.fields))
      .catch(setError);
  }, []);
  if (error) return <ErrorBanner error={error} />;
  if (!rows) return <Spinner />;
  return (
    <div>
      <p className="muted">Which fields are queried, how much they cost and how often existing indexes helped. This is the input for adaptive indexing in a future version.</p>
      <table className="table">
        <thead>
          <tr>
            <th>Signal</th>
            <th>Field</th>
            <th>Index</th>
            <th className="num">Queries</th>
            <th className="num">Avg time</th>
            <th className="num">Bytes read</th>
            <th className="num">Segments skipped</th>
          </tr>
        </thead>
        <tbody>
          {rows.map((r) => (
            <tr key={`${r.signal}:${r.field}`}>
              <td>{r.signal}</td>
              <td className="mono">{r.field}</td>
              <td>{r.index}</td>
              <td className="num">{formatCount(r.queries)}</td>
              <td className="num">{formatDuration(r.totalMs / Math.max(1, r.queries))}</td>
              <td className="num">{formatBytes(r.bytesRead)}</td>
              <td className="num">
                {formatCount(r.segmentsSkipped)} / {formatCount(r.segmentsSkipped + r.segmentsScanned)}
              </td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}

export function SettingsPage() {
  const { location, navigate } = useRouter();
  const tab = location.search.get('tab') ?? 'storage';
  return (
    <div className="page narrow">
      <h1>Settings</h1>
      <div className="tabs" role="tablist">
        {TABS.map(([k, label]) => (
          <button key={k} role="tab" aria-selected={tab === k} className={`tab ${tab === k ? 'is-active' : ''}`} onClick={() => navigate(`/settings?tab=${k}`)}>
            {label}
          </button>
        ))}
      </div>
      {tab === 'storage' && <StorageTab />}
      {tab === 'system' && <SystemTab />}
      {tab === 'keys' && <KeysTab />}
      {tab === 'queries' && <QueriesTab />}
    </div>
  );
}
