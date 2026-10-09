import { useState } from 'react';
import type { ReactNode } from 'react';

import type { TelemetryEvent } from '../lib/types';
import { DEFAULT_COLUMNS, columnLabel, columnValue } from '../lib/columns';
import { formatDateTime, formatTime, isToday, levelClass, levelShort, valueText } from '../lib/format';
import { Link } from '../lib/router';
import { PropertyTree } from './PropertyTree';
import type { FilterAction } from './PropertyTree';
import { StackTrace } from './StackTrace';

export function LogRow({
  event,
  expanded,
  onToggle,
  onFilter,
  isNew,
  columns = DEFAULT_COLUMNS,
}: {
  event: TelemetryEvent;
  expanded: boolean;
  onToggle: () => void;
  onFilter?: FilterAction;
  isNew?: boolean;
  /** Column keys in display order (see lib/columns); the grid template comes from the list. */
  columns?: string[];
}) {
  const time = isToday(event.timestamp) ? formatTime(event.timestamp) : formatDateTime(event.timestamp);
  const cell = (key: string) => {
    switch (key) {
      case 'timestamp':
        return (
          <span key={key} className="log-time" title={event.timestamp}>
            {time}
          </span>
        );
      case 'level':
        return (
          <span key={key} className={`log-level ${levelClass(event.level)}`}>
            {levelShort(event.level)}
          </span>
        );
      case 'service':
        return (
          <span key={key} className="log-service">
            {event.service ?? ''}
          </span>
        );
      case 'message':
        return (
          <span key={key} className="log-message">
            {event.message}
            {event.exception && !expanded && <span className="log-exc"> {event.exception.type ?? 'exception'}</span>}
          </span>
        );
      default: {
        const v = columnValue(event, key);
        const text = v === undefined ? '' : valueText(v);
        return (
          <span key={key} className="log-field" title={text ? `${columnLabel(key)}: ${text}` : undefined}>
            {text}
          </span>
        );
      }
    }
  };
  return (
    <div className={`log ${expanded ? 'is-expanded' : ''} ${isNew ? 'is-new' : ''}`} data-testid="log-row">
      <button type="button" className="log-line" aria-expanded={expanded} onClick={onToggle}>
        <svg className="log-chevron" viewBox="0 0 16 16" width="12" height="12" aria-hidden="true">
          <path d="M6 4l4 4-4 4" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round" />
        </svg>
        {columns.map(cell)}
      </button>
      {expanded && <LogDetails event={event} onFilter={onFilter} />}
    </div>
  );
}

function Detail({ label, children }: { label: string; children: ReactNode }) {
  return (
    <div className="detail">
      <div className="detail-label">{label}</div>
      <div className="detail-value">{children}</div>
    </div>
  );
}

export function LogDetails({ event, onFilter }: { event: TelemetryEvent; onFilter?: FilterAction }) {
  const [raw, setRaw] = useState(false);
  const filter = (field: string, value: string | undefined) =>
    onFilter && value ? (
      <button type="button" className="mini-btn" aria-label={`Include ${field}`} onClick={() => onFilter(field, value, false)}>
        =
      </button>
    ) : null;
  return (
    <div className="log-details" data-testid="log-details">
      <Detail label="Message">
        <div className="detail-message">{event.message}</div>
      </Detail>
      {event.messageTemplate && <Detail label="Template">{event.messageTemplate}</Detail>}
      <Detail label="Timestamp">
        {formatDateTime(event.timestamp)} <span className="muted">({event.timestamp})</span>
      </Detail>
      <Detail label="Level">
        <span className={`log-level ${levelClass(event.level)}`}>{event.level}</span> {filter('level', event.level)}
      </Detail>
      {event.service && (
        <Detail label="Service">
          {event.service} {filter('service', event.service)}
        </Detail>
      )}
      {event.environment && (
        <Detail label="Environment">
          {event.environment} {filter('environment', event.environment)}
        </Detail>
      )}
      {event.traceId && (
        <Detail label="Trace ID">
          <code>{event.traceId}</code> {filter('traceId', event.traceId)}{' '}
          <Link className="btn btn-small" to={`/traces/${event.traceId}`}>
            View trace
          </Link>
        </Detail>
      )}
      {event.spanId && (
        <Detail label="Span ID">
          <code>{event.spanId}</code>
        </Detail>
      )}
      <Detail label="Properties">
        <PropertyTree fields={event.properties} onFilter={onFilter} />
      </Detail>
      {event.exception && (
        <Detail label="Exception">
          <div className="exception">
            {(event.exception.type || event.exception.message) && (
              <div className="exception-head">
                {event.exception.type && <strong>{event.exception.type}</strong>}
                {event.exception.message && <span>: {event.exception.message}</span>}
              </div>
            )}
            {event.exception.stackTrace && <StackTrace text={event.exception.stackTrace} />}
          </div>
        </Detail>
      )}
      {Object.keys(event.resource).length > 0 && (
        <Detail label="Resource">
          <PropertyTree fields={event.resource} prefix="resource" onFilter={onFilter} />
        </Detail>
      )}
      <Detail label="Raw event">
        <button type="button" className="link-btn" onClick={() => setRaw((r) => !r)} aria-expanded={raw}>
          {raw ? 'Hide JSON' : 'Show JSON'}
        </button>
        {raw && <pre className="json">{JSON.stringify(event, null, 2)}</pre>}
      </Detail>
    </div>
  );
}
