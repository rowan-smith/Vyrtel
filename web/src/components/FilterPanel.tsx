import type { Facet, Json } from '../lib/types';
import { formatCount, valueText } from '../lib/format';
import { isPlainField } from '../lib/query';

const LABELS: Record<string, string> = {
  level: 'Level',
  service: 'Service',
  environment: 'Environment',
  'exception.type': 'Exception',
};

/** Detected fields and their most common values; clicking adds a filter. */
export function FilterPanel({
  facets,
  sampled,
  loading,
  onSelect,
}: {
  facets: Facet[];
  sampled: number;
  loading?: boolean;
  onSelect: (field: string, value: Json) => void;
}) {
  const usable = facets.filter((f) => isPlainField(f.field) && f.values.length > 0);
  return (
    <aside className="filter-panel" aria-label="Filters">
      <div className="filter-head">
        Filters{' '}
        <span className="muted small">{loading ? 'updating…' : `from ${formatCount(sampled)} recent events`}</span>
      </div>
      {usable.length === 0 && !loading && <div className="muted small pad">No fields detected.</div>}
      {usable.map((f) => (
        <section key={f.field} className="facet">
          <h3 className="facet-name" title={`${f.count} events have this field`}>
            {LABELS[f.field] ?? f.field}
          </h3>
          <ul>
            {f.values.map((v) => (
              <li key={valueText(v.value)}>
                <button
                  type="button"
                  className="facet-value"
                  onClick={() => onSelect(f.field, v.value)}
                  title={`Add filter: ${f.field} = ${valueText(v.value)}`}
                >
                  <span className="facet-text">{valueText(v.value)}</span>
                  <span className="facet-count">{formatCount(v.count)}</span>
                </button>
              </li>
            ))}
          </ul>
        </section>
      ))}
    </aside>
  );
}
