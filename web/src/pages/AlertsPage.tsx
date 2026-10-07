import { useCallback, useEffect, useState } from 'react';

import { api } from '../lib/api';
import type { Alert, AlertInput } from '../lib/types';
import { formatDateTime } from '../lib/format';
import { logsLink, Link } from '../lib/router';
import { Empty, ErrorBanner, Modal, Spinner } from '../components/common';

const BLANK: AlertInput = {
  name: '',
  query: 'level = "Error"',
  op: '>',
  threshold: 100,
  windowSecs: 300,
  intervalSecs: 60,
  webhookUrl: '',
  enabled: true,
};

function AlertForm({ initial, onSave, onClose }: { initial: AlertInput; onSave: (a: AlertInput) => Promise<void>; onClose: () => void }) {
  const [a, setA] = useState<AlertInput>(initial);
  const [error, setError] = useState<unknown>(null);
  return (
    <Modal title={initial.name ? 'Edit alert' : 'New alert'} onClose={onClose}>
      <form
        className="form"
        onSubmit={async (e) => {
          e.preventDefault();
          try {
            await onSave({ ...a, webhookUrl: a.webhookUrl?.trim() || null });
          } catch (err) {
            setError(err);
          }
        }}
      >
        <label>
          <span className="label">Name</span>
          <input className="input" required value={a.name} onChange={(e) => setA({ ...a, name: e.target.value })} placeholder="Payment error spike" />
        </label>
        <label>
          <span className="label">Log query</span>
          <input className="input mono" value={a.query} onChange={(e) => setA({ ...a, query: e.target.value })} />
        </label>
        <div className="form-row">
          <label>
            <span className="label">Condition</span>
            <div className="inline-form">
              <span className="muted">count</span>
              <select className="select" value={a.op} aria-label="Operator" onChange={(e) => setA({ ...a, op: e.target.value })}>
                {['>', '>=', '<', '<=', '=', '!='].map((o) => (
                  <option key={o}>{o}</option>
                ))}
              </select>
              <input className="input num-input" type="number" aria-label="Threshold" value={a.threshold} onChange={(e) => setA({ ...a, threshold: Number(e.target.value) })} />
            </div>
          </label>
          <label>
            <span className="label">Window (minutes)</span>
            <input className="input num-input" type="number" min={1} value={a.windowSecs / 60} onChange={(e) => setA({ ...a, windowSecs: Math.round(Number(e.target.value) * 60) })} />
          </label>
          <label>
            <span className="label">Evaluate every (minutes)</span>
            <input className="input num-input" type="number" min={1} value={a.intervalSecs / 60} onChange={(e) => setA({ ...a, intervalSecs: Math.max(10, Math.round(Number(e.target.value) * 60)) })} />
          </label>
        </div>
        <label>
          <span className="label">Webhook URL (optional)</span>
          <input className="input" value={a.webhookUrl ?? ''} onChange={(e) => setA({ ...a, webhookUrl: e.target.value })} placeholder="https://example.com/hooks/vyrtel" />
        </label>
        <label className="checkbox">
          <input type="checkbox" checked={a.enabled} onChange={(e) => setA({ ...a, enabled: e.target.checked })} /> Enabled
        </label>
        <ErrorBanner error={error} query={a.query} />
        <div className="form-actions">
          <button type="button" className="btn" onClick={onClose}>
            Cancel
          </button>
          <button className="btn btn-primary" type="submit">
            Save
          </button>
        </div>
      </form>
    </Modal>
  );
}

export function StatusBadge({ status }: { status: string }) {
  return <span className={`badge badge-${status.toLowerCase()}`}>{status}</span>;
}

export function AlertsPage() {
  const [alerts, setAlerts] = useState<Alert[] | null>(null);
  const [error, setError] = useState<unknown>(null);
  const [editing, setEditing] = useState<Alert | 'new' | null>(null);
  const [open, setOpen] = useState<Alert | null>(null);

  const load = useCallback(() => {
    api
      .alerts()
      .then((r) => setAlerts(r.alerts))
      .catch(setError);
  }, []);
  useEffect(() => {
    load();
    const t = window.setInterval(load, 15_000);
    return () => window.clearInterval(t);
  }, [load]);

  const toInput = (a: Alert): AlertInput => ({
    name: a.name,
    query: a.query,
    op: a.op,
    threshold: a.threshold,
    windowSecs: a.windowSecs,
    intervalSecs: a.intervalSecs,
    webhookUrl: a.webhookUrl ?? '',
    enabled: a.enabled,
  });

  return (
    <div className="page">
      <div className="page-head">
        <h1>Alerts</h1>
        <button className="btn btn-primary" onClick={() => setEditing('new')}>
          New alert
        </button>
      </div>
      <p className="muted">Log-count threshold alerts. Each transition (OK ⇄ FIRING, ERROR) is recorded and sent to the alert's webhook.</p>
      <ErrorBanner error={error} />
      {alerts === null && <Spinner />}
      {alerts?.length === 0 && <Empty title="No alerts yet" />}
      {alerts && alerts.length > 0 && (
        <table className="table">
          <thead>
            <tr>
              <th>Status</th>
              <th>Name</th>
              <th>Condition</th>
              <th className="num">Last value</th>
              <th>Last evaluated</th>
              <th />
            </tr>
          </thead>
          <tbody>
            {alerts.map((a) => (
              <tr key={a.id} className={a.enabled ? '' : 'disabled'}>
                <td>
                  <StatusBadge status={a.enabled ? a.state.status : 'DISABLED'} />
                </td>
                <td>
                  <button className="link-btn" onClick={() => api.alert(a.id).then(setOpen)}>
                    {a.name}
                  </button>
                  <div className="muted small mono">{a.query}</div>
                </td>
                <td className="nowrap">
                  count {a.op} {a.threshold} in {a.windowSecs / 60} min
                </td>
                <td className="num">{a.state.lastValue ?? '–'}</td>
                <td className="nowrap">{a.state.lastEvaluatedAt ? formatDateTime(a.state.lastEvaluatedAt) : 'never'}</td>
                <td className="nowrap actions">
                  <button className="btn btn-small" onClick={() => api.evaluateAlert(a.id).then(load).catch(setError)}>
                    Evaluate
                  </button>
                  <button className="btn btn-small" onClick={() => setEditing(a)}>
                    Edit
                  </button>
                  <button
                    className="btn btn-small btn-danger"
                    onClick={async () => {
                      if (window.confirm(`Delete alert "${a.name}"?`)) {
                        await api.deleteAlert(a.id);
                        load();
                      }
                    }}
                  >
                    Delete
                  </button>
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
      {editing && (
        <AlertForm
          initial={editing === 'new' ? BLANK : toInput(editing)}
          onClose={() => setEditing(null)}
          onSave={async (a) => {
            if (editing === 'new') await api.createAlert(a);
            else await api.updateAlert(editing.id, a);
            setEditing(null);
            load();
          }}
        />
      )}
      {open && (
        <Modal title={open.name} onClose={() => setOpen(null)}>
          <p>
            <StatusBadge status={open.state.status} /> <span className="mono">{open.query}</span>{' '}
            <Link to={logsLink(open.query)}>Open in Logs</Link>
          </p>
          {open.state.lastError && <div className="error-banner">{open.state.lastError}</div>}
          {open.webhookUrl && (
            <p className="small">
              Webhook: <span className="mono">{open.webhookUrl}</span>
              {open.state.lastNotificationAt && <> — last sent {formatDateTime(open.state.lastNotificationAt)}</>}
              {open.state.lastNotificationError && <span className="text-error"> ({open.state.lastNotificationError})</span>}
            </p>
          )}
          <h3>History</h3>
          {open.history?.length ? (
            <ul className="history">
              {open.history.map((h, i) => (
                <li key={i}>
                  <span className="muted">{formatDateTime(h.at)}</span> <StatusBadge status={h.from} /> → <StatusBadge status={h.to} /> {h.value != null && `(value ${h.value})`} {h.message}
                </li>
              ))}
            </ul>
          ) : (
            <p className="muted">No transitions yet.</p>
          )}
        </Modal>
      )}
    </div>
  );
}
