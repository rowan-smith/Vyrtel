import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { createAlert, deleteAlert, listAlerts, setAlertEnabled } from '../api';
import type { AlertOperator } from '../types';

const OPERATORS: { id: AlertOperator; label: string }[] = [
  { id: 'gt', label: '>' },
  { id: 'gte', label: '≥' },
  { id: 'lt', label: '<' },
  { id: 'lte', label: '≤' },
  { id: 'eq', label: '=' },
];

export default function AlertsPage() {
  const qc = useQueryClient();
  const alerts = useQuery({
    queryKey: ['alerts'],
    queryFn: listAlerts,
    refetchInterval: 10_000,
  });

  const createMut = useMutation({
    mutationFn: createAlert,
    onSuccess: () => qc.invalidateQueries({ queryKey: ['alerts'] }),
  });

  const deleteMut = useMutation({
    mutationFn: deleteAlert,
    onSuccess: () => qc.invalidateQueries({ queryKey: ['alerts'] }),
  });

  const enableMut = useMutation({
    mutationFn: ({ id, enabled }: { id: string; enabled: boolean }) => setAlertEnabled(id, enabled),
    onSuccess: () => qc.invalidateQueries({ queryKey: ['alerts'] }),
  });

  const firing = (alerts.data ?? []).filter((a) => a.status === 'firing').length;

  return (
    <div className="page">
      <div className="page-header">
        <div>
          <h1 className="page-title">Alerts</h1>
          <div className="muted" style={{ marginTop: 4 }}>
            {firing > 0 ? `${firing} firing` : 'All clear'} · evaluated every 15s over the last hour
          </div>
        </div>
        <button
          className="btn btn-primary"
          onClick={() => {
            const name = window.prompt('Alert name', 'Errors');
            if (!name) return;
            const query = window.prompt('Query (must aggregate)', 'level = error | count');
            if (!query) return;
            const operator = (window.prompt('Operator (gt|gte|lt|lte|eq)', 'gte') ??
              'gte') as AlertOperator;
            const threshold = Number(window.prompt('Threshold', '1') ?? '1');
            createMut.mutate({ name, query, operator, threshold });
          }}
        >
          New Alert
        </button>
      </div>

      <div className="trace-list">
        <table className="table">
          <thead>
            <tr>
              <th>Status</th>
              <th>Name</th>
              <th>Condition</th>
              <th>Value</th>
              <th>Enabled</th>
              <th></th>
            </tr>
          </thead>
          <tbody>
            {(alerts.data ?? []).map((a) => (
              <tr key={a.id}>
                <td>
                  <span className={`chip${a.status === 'firing' ? ' live active' : ''}`}>
                    {a.status === 'firing' ? 'Firing' : 'OK'}
                  </span>
                </td>
                <td>{a.name}</td>
                <td style={{ fontFamily: 'var(--mono)', fontSize: 12 }}>
                  {a.query} {opLabel(a.operator)} {a.threshold}
                </td>
                <td style={{ fontFamily: 'var(--mono)' }}>
                  {a.value != null ? a.value.toFixed(2) : '—'}
                </td>
                <td>
                  <button
                    className={`btn${a.enabled ? ' active' : ''}`}
                    onClick={() => enableMut.mutate({ id: a.id, enabled: !a.enabled })}
                  >
                    {a.enabled ? 'On' : 'Off'}
                  </button>
                </td>
                <td style={{ textAlign: 'right' }}>
                  <button
                    className="btn btn-ghost"
                    onClick={() => {
                      if (window.confirm(`Delete “${a.name}”?`)) deleteMut.mutate(a.id);
                    }}
                  >
                    Delete
                  </button>
                </td>
              </tr>
            ))}
          </tbody>
        </table>
        {(alerts.data?.length ?? 0) === 0 && !alerts.isLoading && (
          <div className="empty">No alert rules yet.</div>
        )}
      </div>
    </div>
  );
}

function opLabel(op: AlertOperator): string {
  return OPERATORS.find((o) => o.id === op)?.label ?? op;
}
