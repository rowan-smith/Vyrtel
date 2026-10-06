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
      <button className="btn btn-primary" type="submit" disabled={running}>
        Run
      </button>
      {children}
    </form>
  );
}

export function LiveToggle({ live, status, onChange }: { live: boolean; status?: string; onChange: (live: boolean) => void }) {
  const label = live ? (status === 'connecting' ? 'Connecting…' : 'Live') : 'Paused';
  return (
    <button
      type="button"
      className={`live-toggle ${live ? 'is-live' : 'is-paused'}`}
      aria-pressed={live}
      title={live ? 'Live: new matching events appear automatically. Click to pause.' : 'Paused: results stay put. Click to go live.'}
      onClick={() => onChange(!live)}
    >
      <span className="live-dot" aria-hidden="true" />
      {label}
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
