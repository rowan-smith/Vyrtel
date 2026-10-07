import { useEffect, useState } from 'react';
import type { ReactNode } from 'react';

import { RANGE_PRESETS } from '../lib/query';

export function QueryBar({
  query,
  onRun,
  placeholder = 'level = "Error" and service = "payments"',
  running,
  children,
}: {
  query: string;
  onRun: (query: string) => void;
  placeholder?: string;
  running?: boolean;
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

export function RangeSelect({ value, onChange }: { value: string; onChange: (v: string) => void }) {
  return (
    <select className="select" aria-label="Time range" value={value} onChange={(e) => onChange(e.target.value)}>
      {RANGE_PRESETS.map((r) => (
        <option key={r.key} value={r.key}>
          {r.label}
        </option>
      ))}
    </select>
  );
}
