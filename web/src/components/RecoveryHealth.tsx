import type { QuarantinedItem, StorageStats } from '../lib/types';
import { formatBytes } from '../lib/format';

export const RECOVERY_DOCS_URL = 'https://rowan-smith.github.io/Vyrtel/docs/configuration.html#recovery-health';

const SIGNALS = ['logs', 'traces', 'metrics'] as const;

const KIND_LABEL: Record<QuarantinedItem['kind'], string> = {
  segment: 'segment',
  wal: 'WAL file',
  walTail: 'WAL tail',
};

/**
 * Warns when startup recovery quarantined data, or quarantined files from an
 * earlier start are still on disk. A clean start (WAL replay alone) shows nothing.
 */
export function RecoveryHealth({ storage }: { storage: StorageStats }) {
  const summary = storage.recovery;
  if (!summary || summary.status === 'clean') return null;
  const damaged = summary.status === 'damaged';
  const affected = SIGNALS.filter((k) => storage.signals[k].recovery.status !== 'clean');
  const items = affected.flatMap((k) => storage.signals[k].recovery.quarantined);
  return (
    <div className="warning-banner" role="alert" data-testid="recovery-health">
      <strong>{damaged ? 'Startup recovery set damaged data aside' : 'Quarantined files from an earlier start'}</strong>
      <p>
        {damaged
          ? 'Vyrtel found unreadable storage files when it started and moved them to quarantine instead of guessing their contents.'
          : 'This start found no new damage, but files an earlier recovery set aside are still in quarantine.'}{' '}
        Events in quarantined files are not queryable and Vyrtel does not repair them, so data for {affected.join(', ')} may be
        incomplete. Copy the files off for diagnosis if needed, then delete them.{' '}
        <a href={RECOVERY_DOCS_URL} target="_blank" rel="noreferrer">
          Recovery documentation
        </a>
      </p>
      <table className="table">
        <thead>
          <tr>
            <th>Signal</th>
            <th>Status</th>
            <th className="num">WAL bytes discarded</th>
            <th className="num">Quarantined this start</th>
            <th className="num">In quarantine now</th>
          </tr>
        </thead>
        <tbody>
          {affected.map((k) => {
            const r = storage.signals[k].recovery;
            const count = r.segmentsQuarantined + r.walFilesQuarantined + r.walTailsQuarantined;
            return (
              <tr key={k}>
                <td>{k}</td>
                <td>{r.status === 'damaged' ? 'damaged this start' : 'quarantine retained'}</td>
                <td className="num">{formatBytes(r.walBytesDiscarded)}</td>
                <td className="num">
                  {count} ({formatBytes(r.quarantinedBytes)})
                </td>
                <td className="num">
                  {r.quarantineDir.files} {r.quarantineDir.files === 1 ? 'file' : 'files'} ({formatBytes(r.quarantineDir.bytes)})
                </td>
              </tr>
            );
          })}
        </tbody>
      </table>
      {items.length > 0 && (
        <ul className="small">
          {items.map((q, i) => (
            <li key={i}>
              {q.signal} {KIND_LABEL[q.kind]} <span className="mono">{q.source}</span>: {q.reason} → <span className="mono">{q.file}</span> (
              {formatBytes(q.bytes)})
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}
