import type { ReactNode } from 'react';

import { AVAILABLE_COLUMNS, DEFAULT_COLUMNS, columnLabel } from '../lib/columns';
import { formatCount, valueText } from '../lib/format';
import { isFilterActive } from '../lib/query';
import type { Facet, Json } from '../lib/types';

/** The filter groups shown (for now), in order, with their headings. */
const GROUPS: [field: string, title: string][] = [
  ['level', 'Levels'],
  ['environment', 'Environments'],
];

function Section({ title, action, children }: { title: string; action?: ReactNode; children: ReactNode }) {
  return (
    <section className="side-section">
      <div className="side-head">
        <h2 className="side-title">{title}</h2>
        {action}
      </div>
      {children}
    </section>
  );
}

function Option({ on, label, count, title, onClick }: { on: boolean; label: string; count?: number; title: string; onClick: () => void }) {
  return (
    <li>
      <button type="button" className={`side-option ${on ? 'is-on' : ''}`} aria-pressed={on} title={title} onClick={onClick}>
        <span className="side-dot" aria-hidden="true" />
        <span className="side-label">{label}</span>
        {count !== undefined && <span className="side-count">{formatCount(count)}</span>}
      </button>
    </li>
  );
}

/**
 * The Logs page side panel. "Pinned" picks which fields show as columns in the log list; "Filters"
 * lists the most common levels and environments. Filters act like radio buttons per field: pick a
 * value to filter on it, pick it again to clear, pick another to swap.
 */
export function LogsSidebar({
  facets,
  sampled,
  loading,
  query,
  columns,
  onColumnsChange,
  onFilter,
}: {
  facets: Facet[];
  sampled: number;
  loading?: boolean;
  query: string;
  columns: string[];
  onColumnsChange: (cols: string[]) => void;
  onFilter: (field: string, value: Json | null) => void;
}) {
  const candidates = AVAILABLE_COLUMNS;
  const isDefault = columns.length === DEFAULT_COLUMNS.length && DEFAULT_COLUMNS.every((c) => columns.includes(c));

  const groups = GROUPS.map(([field, title]) => ({ title, facet: facets.find((f) => f.field === field && f.values.length > 0) })).filter(
    (g): g is { title: string; facet: Facet } => !!g.facet,
  );

  const togglePin = (key: string) =>
    onColumnsChange(columns.includes(key) ? columns.filter((c) => c !== key) : [...columns, key]);

  return (
    <aside className="logs-side" aria-label="Columns and filters">
      <Section
        title="Pinned"
        action={
          !isDefault && (
            <button type="button" className="side-action" onClick={() => onColumnsChange(DEFAULT_COLUMNS)} title="Show the default columns">
              Reset
            </button>
          )
        }
      >
        <ul className="side-list" aria-label="Columns">
          {candidates.map((key) => {
            const on = columns.includes(key);
            return (
              <Option
                key={key}
                on={on}
                label={columnLabel(key)}
                title={on ? `Remove the ${columnLabel(key)} column` : `Show ${columnLabel(key)} as a column`}
                onClick={() => togglePin(key)}
              />
            );
          })}
        </ul>
      </Section>

      <Section title="Filters" action={<span className="side-note">{loading ? 'updating…' : `${formatCount(sampled)} events`}</span>}>
        {groups.length === 0 && !loading && <div className="muted small side-empty">No levels or environments yet.</div>}
        {groups.map(({ title, facet: f }) => (
          <div key={f.field} className="side-group">
            <h3 className="side-group-name" title={`${formatCount(f.count)} events have this field`}>
              {title}
            </h3>
            <ul className="side-list" aria-label={title}>
              {f.values.map((v) => {
                const on = isFilterActive(query, f.field, v.value);
                const text = valueText(v.value);
                return (
                  <Option
                    key={text}
                    on={on}
                    label={text}
                    count={v.count}
                    title={on ? `Clear filter: ${f.field} = ${text}` : `Filter: ${f.field} = ${text}`}
                    onClick={() => onFilter(f.field, on ? null : v.value)}
                  />
                );
              })}
            </ul>
          </div>
        ))}
      </Section>
    </aside>
  );
}
