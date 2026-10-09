import { useEffect, useRef, useState } from 'react';
import type { ReactNode } from 'react';

import { RANGE_PRESETS } from '../lib/query';

export function QueryBar({
  query,
  onRun,
  placeholder = 'level = "Error" and service = "payments"',
  running,
  leading,
  children,
}: {
  query: string;
  onRun: (query: string) => void;
  placeholder?: string;
  running?: boolean;
  /** Controls placed before the query input (e.g. the Logs side-panel toggle). */
  leading?: ReactNode;
  children?: ReactNode;
}) {
  const [text, setText] = useState(query);
  // Follow external changes (filter panel clicks, URL navigation).
  useEffect(() => setText(query), [query]);

  return (
    <form
      className="query-bar"
      role="search"
      onSubmit={(e) => {
        e.preventDefault();
        onRun(text.trim());
      }}
    >
      {leading}
      <input
        className="query-input"
        aria-label="Query"
        value={text}
        placeholder={placeholder}
        spellCheck={false}
        autoComplete="off"
        onChange={(e) => setText(e.target.value)}
      />
      <button className="btn btn-run" type="submit" disabled={running} aria-label="Run" title="Run query (Enter)">
        <svg viewBox="0 0 24 24" width="16" height="16" aria-hidden="true">
          <path d="M7 4.5v15a1 1 0 0 0 1.5.86l12.5-7.5a1 1 0 0 0 0-1.72L8.5 3.64A1 1 0 0 0 7 4.5z" fill="currentColor" />
        </svg>
      </button>
      {children}
    </form>
  );
}

export function LiveToggle({ live, status, onChange }: { live: boolean; status?: string; onChange: (live: boolean) => void }) {
  // Always labelled "Live"; the dot shows the state (red off, amber connecting, green on) and
  // aria-pressed carries it for assistive tech.
  const state = live ? (status === 'connecting' ? 'is-connecting' : 'is-live') : 'is-paused';
  return (
    <button
      type="button"
      className={`live-toggle ${state}`}
      aria-pressed={live}
      title={
        live
          ? status === 'connecting'
            ? 'Live tail: connecting… Click to stop.'
            : 'Live tail on: new matching events appear automatically. Click to stop.'
          : 'Live tail off: results stay put. Click to go live.'
      }
      onClick={() => onChange(!live)}
    >
      <span className="live-dot" aria-hidden="true" />
      Live
    </button>
  );
}

/** The time range as a clock button with a menu. Off the default hour it also shows a short label. */
export function RangeSelect({ value, onChange }: { value: string; onChange: (v: string) => void }) {
  const [open, setOpen] = useState(false);
  const ref = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (!open) return;
    const onDown = (e: MouseEvent) => {
      if (!ref.current?.contains(e.target as Node)) setOpen(false);
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') {
        setOpen(false);
        ref.current?.querySelector<HTMLButtonElement>('button')?.focus();
      }
    };
    document.addEventListener('mousedown', onDown);
    document.addEventListener('keydown', onKey);
    return () => {
      document.removeEventListener('mousedown', onDown);
      document.removeEventListener('keydown', onKey);
    };
  }, [open]);
  const current = RANGE_PRESETS.find((r) => r.key === value) ?? RANGE_PRESETS[2];
  const short = current.key === 'all' ? 'All' : current.key;
  return (
    <div className="range-menu" ref={ref}>
      <button
        type="button"
        className="btn btn-icon"
        aria-haspopup="menu"
        aria-expanded={open}
        aria-label={`Time range: ${current.label}`}
        title={`Time range: ${current.label}`}
        onClick={() => setOpen((o) => !o)}
      >
        <svg viewBox="0 0 24 24" width="17" height="17" aria-hidden="true" fill="none" stroke="currentColor" strokeWidth="1.9" strokeLinecap="round" strokeLinejoin="round">
          <circle cx="12" cy="12" r="9" />
          <path d="M12 7v5l3 2" />
        </svg>
        {current.key !== '1h' && <span className="range-short">{short}</span>}
      </button>
      {open && (
        <div className="menu" role="menu" aria-label="Time range">
          {RANGE_PRESETS.map((r) => (
            <button
              key={r.key}
              type="button"
              role="menuitemradio"
              aria-checked={r.key === current.key}
              className="menu-item"
              onClick={() => {
                onChange(r.key);
                setOpen(false);
              }}
            >
              {r.label}
            </button>
          ))}
        </div>
      )}
    </div>
  );
}
