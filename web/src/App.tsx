import { useCallback, useEffect, useState } from 'react';

import { api, setUnauthorizedHandler } from './lib/api';
import type { AuthState } from './lib/types';
import { Link, matchPath, useRouter } from './lib/router';
import { Spinner } from './components/common';
import { WORD } from './lib/wordmark';
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

/**
 * The integrated wordmark [V]yrtel: the Trace V (two telemetry paths, mint and iris, converging
 * on a correlation node) is the first letter. Rendered inside an element with class `brand`,
 * which is compact (V only) until hover/focus unless it also has `is-expanded`.
 */
function BrandMark() {
  return (
    <>
      <svg className="brand-mark" viewBox="0 0 96 96" aria-hidden="true">
        <g>
          <path d="M18 18 L47 72" fill="none" stroke="var(--mint)" strokeWidth="10" strokeLinecap="round" />
          <path d="M78 18 L49 72" fill="none" stroke="var(--iris)" strokeWidth="10" strokeLinecap="round" />
        </g>
        <circle cx="48" cy="73" r="7" fill="var(--brand-node-ring)" />
        <circle className="brand-node" cx="48" cy="73" r="4" fill="var(--brand-node)" />
      </svg>
      <span className="brand-word" aria-hidden="true">
        <svg viewBox={`0 ${-WORD.ascender} ${WORD.width} ${WORD.ascender + WORD.descender}`} style={{ width: `${WORD.width / 100}em` }}>
          <path d={WORD.path} fill="currentColor" />
        </svg>
      </span>
      <span className="sr-only">Vyrtel</span>
    </>
  );
}

function LoginBrand() {
  return (
    <h1 className="login-brand">
      <span className="brand is-expanded">
        <BrandMark />
      </span>
      <span className="brand-tagline">View. Trace. Understand.</span>
    </h1>
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
        <LoginBrand />
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
          <LoginBrand />
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
        <Link to="/logs" className="brand" title="Vyrtel">
          <BrandMark />
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
