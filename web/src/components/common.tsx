import { useEffect } from 'react';
import type { ReactNode } from 'react';

import { ApiError } from '../lib/api';

export function ErrorBanner({ error, query }: { error: unknown; query?: string }) {
  if (!error) return null;
  const e = error instanceof ApiError ? error : null;
  const message = e?.message ?? (error instanceof Error ? error.message : String(error));
  return (
    <div className="error-banner" role="alert">
      <strong>{e?.code === 'invalid_query' ? 'Query error' : 'Error'}</strong> {message}
      {e?.position != null && query != null && (
        <pre className="error-caret" aria-label="error position">
          {query}
          {'\n'}
          {' '.repeat(Math.min(e.position, query.length))}^
        </pre>
      )}
    </div>
  );
}

export function Spinner({ label = 'Loading' }: { label?: string }) {
  return <span className="spinner" role="status" aria-label={label} />;
}

export function Empty({ title, children }: { title: string; children?: ReactNode }) {
  return (
    <div className="empty">
      <div className="empty-title">{title}</div>
      {children && <div className="empty-body">{children}</div>}
    </div>
  );
}

export function Modal({ title, onClose, children }: { title: string; onClose: () => void; children: ReactNode }) {
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => e.key === 'Escape' && onClose();
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, [onClose]);
  return (
    <div className="modal-backdrop" onMouseDown={(e) => e.target === e.currentTarget && onClose()}>
      <div className="modal" role="dialog" aria-modal="true" aria-label={title}>
        <div className="modal-head">
          <h2>{title}</h2>
          <button className="icon-btn" onClick={onClose} aria-label="Close">
            ×
          </button>
        </div>
        <div className="modal-body">{children}</div>
      </div>
    </div>
  );
}

export function Stat({ label, value, hint }: { label: string; value: ReactNode; hint?: string }) {
  return (
    <div className="stat" title={hint}>
      <div className="stat-value">{value}</div>
      <div className="stat-label">{label}</div>
    </div>
  );
}
