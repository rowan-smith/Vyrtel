import { useState } from 'react';

import type { Diagnostics as D } from '../lib/types';
import { formatBytes, formatCount, formatDuration } from '../lib/format';

const KIND_LABEL: Record<string, string> = {
  time: 'time range',
  bitmap: 'bitmap index',
  bloom: 'Bloom filter',
  zonemap: 'zone map',
  none: 'no index (scanned)',
};

/** Why a query was fast or slow. */
export function DiagnosticsPanel({ diagnostics, count }: { diagnostics: D | null; count?: number }) {
  const [open, setOpen] = useState(false);
  if (!diagnostics) return null;
  const d = diagnostics;
  return (
    <div className="diagnostics">
      <button type="button" className="diag-summary" onClick={() => setOpen((o) => !o)} aria-expanded={open}>
        <span className="chev">{open ? '▾' : '▸'}</span>
        {count != null && <span>{formatCount(count)} results · </span>}
        <span>{formatDuration(d.elapsedMs)}</span>
        <span className="muted">
          {' '}
          · {formatCount(d.eventsExamined)} examined · {d.segmentsSkipped}/{d.segmentsConsidered} segments skipped ·{' '}
          {formatBytes(d.bytesRead)} read
        </span>
      </button>
      {open && (
        <div className="diag-body" data-testid="diagnostics">
          <table className="kv">
            <tbody>
              <tr>
                <th>Execution</th>
                <td>{formatDuration(d.elapsedMs)}</td>
              </tr>
              <tr>
                <th>Events examined</th>
                <td>{formatCount(d.eventsExamined)}</td>
              </tr>
              <tr>
                <th>Events matched</th>
                <td>{formatCount(d.eventsMatched)}</td>
              </tr>
              <tr>
                <th>Segments considered</th>
                <td>{d.segmentsConsidered}</td>
              </tr>
              <tr>
                <th>Segments skipped</th>
                <td>
                  {d.segmentsSkipped}{' '}
                  <span className="muted">
                    (time {d.segmentsSkippedByTime}, index {d.segmentsSkippedByIndex}, not needed {d.segmentsNotNeeded})
                  </span>
                </td>
              </tr>
              <tr>
                <th>Blocks read</th>
                <td>
                  {d.blocksRead} <span className="muted">({d.blocksSkipped} of {d.blocksConsidered} skipped)</span>
                </td>
              </tr>
              <tr>
                <th>Bytes read</th>
                <td>{formatBytes(d.bytesRead)}</td>
              </tr>
              <tr>
                <th>Unsealed events scanned</th>
                <td>{formatCount(d.unsealedEventsScanned)}</td>
              </tr>
            </tbody>
          </table>
          <div className="indexes">
            <div className="detail-label">Indexes used</div>
            <ul>
              {d.indexes.map((i) => (
                <li key={i.field} className={i.kind === 'none' ? 'idx-none' : 'idx-used'}>
                  <span aria-hidden="true">{i.kind === 'none' ? '○' : '✓'}</span> {i.field}{' '}
                  <span className="muted">{KIND_LABEL[i.kind] ?? i.kind}</span>
                </li>
              ))}
            </ul>
          </div>
        </div>
      )}
    </div>
  );
}
