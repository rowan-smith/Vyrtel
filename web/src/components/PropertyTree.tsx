import type { Fields, Json } from '../lib/types';
import { valueText } from '../lib/format';
import { isPlainField } from '../lib/query';

export type FilterAction = (field: string, value: Json, negate: boolean) => void;

/**
 * Structured properties rendered vertically: one row per key, nested
 * objects indented underneath. Never a wide horizontal table.
 */
export function PropertyTree({ fields, prefix = '', onFilter }: { fields: Fields; prefix?: string; onFilter?: FilterAction }) {
  const entries = Object.entries(fields);
  if (!entries.length) return <div className="muted small">No properties</div>;
  return (
    <dl className="props">
      {entries.map(([k, v]) => (
        <PropertyRow key={k} name={k} path={prefix ? `${prefix}.${k}` : k} value={v} onFilter={onFilter} />
      ))}
    </dl>
  );
}

function PropertyRow({ name, path, value, onFilter }: { name: string; path: string; value: Json; onFilter?: FilterAction }) {
  const nested = value !== null && typeof value === 'object' && !Array.isArray(value);
  const scalarArray = Array.isArray(value) && value.every((x) => x === null || typeof x !== 'object');
  return (
    <div className="prop" data-testid={`prop-${path}`}>
      <dt className="prop-key">{name}</dt>
      <dd className="prop-value">
        {nested ? (
          <PropertyTree fields={value as Fields} prefix={path} onFilter={onFilter} />
        ) : Array.isArray(value) && !scalarArray ? (
          <pre className="json">{JSON.stringify(value, null, 2)}</pre>
        ) : (
          <>
            <span className={`val val-${value === null ? 'null' : typeof value}`}>{valueText(value)}</span>
            {onFilter && isPlainField(path) && !Array.isArray(value) && (
              <span className="prop-actions">
                <button type="button" className="mini-btn" title={`Filter: ${path} = value`} aria-label={`Include ${path}`} onClick={() => onFilter(path, value, false)}>
                  =
                </button>
                <button type="button" className="mini-btn" title={`Exclude: ${path} != value`} aria-label={`Exclude ${path}`} onClick={() => onFilter(path, value, true)}>
                  ≠
                </button>
              </span>
            )}
          </>
        )}
      </dd>
    </div>
  );
}
