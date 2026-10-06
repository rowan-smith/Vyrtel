import { Navigate, NavLink, Route, Routes, useNavigate } from 'react-router-dom';
import { useQuery } from '@tanstack/react-query';
import { fetchMe, getToken, logout, setToken } from './api';
import LogsPage from './pages/LogsPage';
import TracesPage from './pages/TracesPage';
import TraceDetailPage from './pages/TraceDetailPage';
import MetricsPage from './pages/MetricsPage';
import AlertsPage from './pages/AlertsPage';
import DashboardsPage from './pages/DashboardsPage';
import DashboardDetailPage from './pages/DashboardDetailPage';
import SettingsPage from './pages/SettingsPage';
import AccountPage from './pages/AccountPage';
import LoginPage from './pages/LoginPage';

function Shell() {
  const navigate = useNavigate();
  const me = useQuery({
    queryKey: ['me'],
    queryFn: fetchMe,
    retry: false,
  });

  async function onLogout() {
    try {
      await logout();
    } catch {
      /* ignore */
    }
    setToken(null);
    navigate('/login', { replace: true });
  }

  if (me.isLoading) {
    return <div className="login-page muted">Loading…</div>;
  }

  if (me.isError || !me.data) {
    return <Navigate to="/login" replace />;
  }

  return (
    <div className="app-shell">
      <nav className="topnav">
        <NavLink to="/logs" className="brand">
          <span className="brand-logo">
            <svg width="20" height="20" viewBox="0 0 24 24" fill="none" xmlns="http://www.w3.org/2000/svg">
              <circle cx="12" cy="12" r="10" fill="url(#brand-grad)" />
              <path
                d="M12 7C9.24 7 7 9.24 7 12C7 14.76 9.24 17 12 17C14.76 17 17 14.76 17 12C17 9.24 14.76 7 12 7ZM12 15C10.34 15 9 13.66 9 12C9 10.34 10.34 9 12 9C13.66 9 15 10.34 15 12C15 13.66 13.66 15 12 15Z"
                fill="#ffffff"
              />
              <circle cx="12" cy="12" r="2" fill="#3b82f6" />
              <defs>
                <linearGradient id="brand-grad" x1="2" y1="2" x2="22" y2="22" gradientUnits="userSpaceOnUse">
                  <stop stopColor="#3b82f6" />
                  <stop offset="1" stopColor="#1d4ed8" />
                </linearGradient>
              </defs>
            </svg>
          </span>
          Observatory
        </NavLink>
        <div className="nav-links">
          <NavLink to="/logs" className={({ isActive }) => `nav-link${isActive ? ' active' : ''}`}>
            Logs
          </NavLink>
          <NavLink to="/traces" className={({ isActive }) => `nav-link${isActive ? ' active' : ''}`}>
            Trace
          </NavLink>
          <NavLink to="/metrics" className={({ isActive }) => `nav-link${isActive ? ' active' : ''}`}>
            Metrics
          </NavLink>
          <NavLink
            to="/dashboards"
            className={({ isActive }) => `nav-link${isActive ? ' active' : ''}`}
          >
            Dashboards
          </NavLink>
          <NavLink to="/alerts" className={({ isActive }) => `nav-link${isActive ? ' active' : ''}`}>
            Alerts
          </NavLink>
        </div>
        <div className="nav-right">
          <NavLink to="/settings" className={({ isActive }) => `nav-link nav-link-icon${isActive ? ' active' : ''}`}>
            <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
              <circle cx="12" cy="12" r="3" />
              <path d="M19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 0 1 0 2.83 2 2 0 0 1-2.83 0l-.06-.06a1.65 1.65 0 0 0-1.82-.33 1.65 1.65 0 0 0-1 1.51V21a2 2 0 0 1-2 2 2 2 0 0 1-2-2v-.09A1.65 1.65 0 0 0 9 19.4a1.65 1.65 0 0 0-1.82.33l-.06.06a2 2 0 0 1-2.83 0 2 2 0 0 1 0-2.83l.06-.06a1.65 1.65 0 0 0 .33-1.82 1.65 1.65 0 0 0-1.51-1H3a2 2 0 0 1-2-2 2 2 0 0 1 2-2h.09A1.65 1.65 0 0 0 4.6 9a1.65 1.65 0 0 0-.33-1.82l-.06-.06a2 2 0 0 1 0-2.83 2 2 0 0 1 2.83 0l.06.06a1.65 1.65 0 0 0 1.82.33H9a1.65 1.65 0 0 0 1-1.51V3a2 2 0 0 1 2-2 2 2 0 0 1 2 2v.09a1.65 1.65 0 0 0 1 1.51 1.65 1.65 0 0 0 1.82-.33l.06-.06a2 2 0 0 1 2.83 0 2 2 0 0 1 0 2.83l-.06.06a1.65 1.65 0 0 0-.33 1.82V9a1.65 1.65 0 0 0 1.51 1H21a2 2 0 0 1 2 2 2 2 0 0 1-2 2h-.09a1.65 1.65 0 0 0-1.51 1z" />
            </svg>
            Settings
          </NavLink>
          <NavLink to="/account" className={({ isActive }) => `nav-link nav-link-icon${isActive ? ' active' : ''}`}>
            <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
              <path d="M20 21v-2a4 4 0 0 0-4-4H8a4 4 0 0 0-4 4v2" />
              <circle cx="12" cy="7" r="4" />
            </svg>
            Account
            <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" style={{ marginLeft: 2, opacity: 0.7 }}>
              <polyline points="6 9 12 15 18 9" />
            </svg>
          </NavLink>
        </div>
      </nav>
      <Routes>
        <Route path="/" element={<Navigate to="/logs" replace />} />
        <Route path="/logs" element={<LogsPage />} />
        <Route path="/events" element={<Navigate to="/logs" replace />} />
        <Route path="/traces" element={<TracesPage />} />
        <Route path="/traces/:traceId" element={<TraceDetailPage />} />
        <Route path="/metrics" element={<MetricsPage />} />
        <Route path="/alerts" element={<AlertsPage />} />
        <Route path="/dashboards" element={<DashboardsPage />} />
        <Route path="/dashboards/:id" element={<DashboardDetailPage />} />
        <Route path="/settings" element={<SettingsPage />} />
        <Route path="/account" element={<AccountPage account={me.data} onLogout={onLogout} />} />
      </Routes>
    </div>
  );
}

function RequireAuth({ children }: { children: React.ReactNode }) {
  if (!getToken()) {
    return <Navigate to="/login" replace />;
  }
  return <>{children}</>;
}

export default function App() {
  return (
    <Routes>
      <Route path="/login" element={<LoginPage />} />
      <Route
        path="/*"
        element={
          <RequireAuth>
            <Shell />
          </RequireAuth>
        }
      />
    </Routes>
  );
}
