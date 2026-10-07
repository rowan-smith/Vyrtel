import { useCallback, useEffect, useState } from 'react';

import { api, setUnauthorizedHandler } from './lib/api';
import type { AuthState } from './lib/types';
import { Link, matchPath, useRouter } from './lib/router';
import { Spinner } from './components/common';
import { AlertsPage } from './pages/AlertsPage';
import { DashboardPage, DashboardsPage } from './pages/DashboardsPage';
import { LogsPage } from './pages/LogsPage';
import { MetricsPage } from './pages/MetricsPage';
import { SettingsPage } from './pages/SettingsPage';
import { TraceDetailPage } from './pages/TraceDetailPage';
import { TracesPage } from './pages/TracesPage';

const NAV = [
  ['/logs', 'Logs'],
  ['/traces', 'Traces'],
  ['/metrics', 'Metrics'],
  ['/dashboards', 'Dashboards'],
  ['/alerts', 'Alerts'],
] as const;

function Logo() {
  return (
    <svg width="20" height="20" viewBox="0 0 32 32" aria-hidden="true">
      <circle cx="16" cy="16" r="10" fill="none" stroke="currentColor" strokeWidth="3" />
      <circle cx="16" cy="16" r="4" fill="var(--ok)" />
    </svg>
  );
}

function LoginPage({ onLogin }: { onLogin: (a: AuthState) => void }) {
  const [username, setUsername] = useState('admin');
  const [password, setPassword] = useState('');
  const [error, setError] = useState<string | null>(null);
  return (
    <div className="login">
      <form
        className="card login-card"
        onSubmit={async (e) => {
          e.preventDefault();
          setError(null);
          try {
            onLogin(await api.login(username, password));
          } catch (err) {
            setError((err as Error).message);
          }
        }}
      >
        <h1>
          <Logo /> Vyrtel
        </h1>
        <label>
          <span className="label">Username</span>
          <input className="input" value={username} onChange={(e) => setUsername(e.target.value)} autoComplete="username" />
        </label>
        <label>
          <span className="label">Password</span>
          <input className="input" type="password" value={password} onChange={(e) => setPassword(e.target.value)} autoComplete="current-password" autoFocus />
        </label>
        {error && (
          <div className="error-banner" role="alert">
            {error}
          </div>
        )}
        <button className="btn btn-primary" type="submit">
          Sign in
        </button>
      </form>
    </div>
  );
}

function Routes() {
  const { location, navigate } = useRouter();
  const path = location.path;
  useEffect(() => {
    if (path === '/' || path === '') navigate('/logs', { replace: true });
  }, [path, navigate]);

  let m;
  if (path.startsWith('/logs')) return <LogsPage />;
  if ((m = matchPath('/traces/:id', path))) return <TraceDetailPage traceId={m.id} />;
  if (path.startsWith('/traces')) return <TracesPage />;
  if (path.startsWith('/metrics')) return <MetricsPage />;
  if ((m = matchPath('/dashboards/:id', path))) return <DashboardPage id={Number(m.id)} />;
  if (path.startsWith('/dashboards')) return <DashboardsPage />;
  if (path.startsWith('/alerts')) return <AlertsPage />;
  if (path.startsWith('/settings')) return <SettingsPage />;
  return (
    <div className="page">
      <h1>Not found</h1>
      <Link to="/logs">Go to Logs</Link>
    </div>
  );
}

export function App() {
  const { location } = useRouter();
  const [auth, setAuth] = useState<AuthState | null>(null);
  const [authError, setAuthError] = useState<string | null>(null);

  const refresh = useCallback(() => {
    api
      .me()
      .then((a) => {
        setAuth(a);
        setAuthError(null);
      })
      .catch((e) => setAuthError((e as Error).message));
  }, []);

  useEffect(() => {
    refresh();
    setUnauthorizedHandler(() => setAuth((a) => (a ? { ...a, user: null } : a)));
    return () => setUnauthorizedHandler(null);
  }, [refresh]);

  if (authError) {
    return (
      <div className="login">
        <div className="card login-card">
          <h1>
            <Logo /> Vyrtel
          </h1>
          <div className="error-banner">{authError}</div>
          <button className="btn" onClick={refresh}>
            Retry
          </button>
        </div>
      </div>
    );
  }
  if (!auth) return <div className="center"><Spinner /></div>;
  if (auth.authEnabled && !auth.user) return <LoginPage onLogin={setAuth} />;

  const active = (p: string) => location.path === p || location.path.startsWith(`${p}/`);
  return (
    <div className="app">
      <header className="topbar">
        <Link to="/logs" className="brand">
          <Logo /> Vyrtel
        </Link>
        <nav className="nav" aria-label="Main">
          {NAV.map(([p, label]) => (
            <Link key={p} to={p} className={`nav-link ${active(p) ? 'is-active' : ''}`}>
              {label}
            </Link>
          ))}
        </nav>
        <div className="nav-right">
          <Link to="/settings" className={`nav-link ${active('/settings') ? 'is-active' : ''}`}>
            Settings
          </Link>
          {auth.authEnabled ? (
            <button
              className="nav-link profile"
              title="Sign out"
              onClick={async () => {
                await api.logout();
                setAuth({ ...auth, user: null });
              }}
            >
              {auth.user} · Sign out
            </button>
          ) : (
            <span className="nav-link profile muted" title="Authentication is disabled">
              Local
            </span>
          )}
        </div>
      </header>
      <main className="main">
        <Routes />
      </main>
    </div>
  );
}
