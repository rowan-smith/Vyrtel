import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { Link } from 'react-router-dom';
import { createDashboard, deleteDashboard, listDashboards } from '../api';

export default function DashboardsPage() {
  const qc = useQueryClient();
  const dashboards = useQuery({ queryKey: ['dashboards'], queryFn: listDashboards });

  const createMut = useMutation({
    mutationFn: createDashboard,
    onSuccess: () => qc.invalidateQueries({ queryKey: ['dashboards'] }),
  });

  const deleteMut = useMutation({
    mutationFn: deleteDashboard,
    onSuccess: () => qc.invalidateQueries({ queryKey: ['dashboards'] }),
  });

  return (
    <div className="page">
      <div className="page-header">
        <h1 className="page-title">Dashboards</h1>
        <button
          className="btn btn-primary"
          onClick={() => {
            const name = window.prompt('Dashboard name');
            if (name) createMut.mutate(name);
          }}
        >
          New Dashboard
        </button>
      </div>
      <div className="dash-list">
        <table className="table">
          <thead>
            <tr>
              <th>Name</th>
              <th></th>
            </tr>
          </thead>
          <tbody>
            {(dashboards.data ?? []).map((d) => (
              <tr key={d.id}>
                <td>
                  <Link to={`/dashboards/${d.id}`}>{d.name}</Link>
                </td>
                <td style={{ textAlign: 'right' }}>
                  <button
                    className="btn btn-ghost"
                    onClick={() => {
                      if (window.confirm(`Delete “${d.name}”?`)) deleteMut.mutate(d.id);
                    }}
                  >
                    Delete
                  </button>
                </td>
              </tr>
            ))}
          </tbody>
        </table>
        {(dashboards.data?.length ?? 0) === 0 && !dashboards.isLoading && (
          <div className="empty">No dashboards yet.</div>
        )}
      </div>
    </div>
  );
}
