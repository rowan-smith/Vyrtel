import { render, screen, within } from '@testing-library/react';
import { describe, expect, it } from 'vitest';

import { RECOVERY_DOCS_URL, RecoveryHealth } from '../../src/components/RecoveryHealth';
import type { SignalRecovery, StorageStats } from '../../src/lib/types';

function recovery(overrides: Partial<SignalRecovery> = {}): SignalRecovery {
  return {
    status: 'clean',
    segmentsLoaded: 2,
    walFilesReplayed: 1,
    walRecordsReplayed: 40,
    walEventsReplayed: 400,
    segmentsSealed: 0,
    staleWalsRemoved: 0,
    replacedSegmentsRemoved: 0,
    walBytesDiscarded: 0,
    segmentsQuarantined: 0,
    walFilesQuarantined: 0,
    walTailsQuarantined: 0,
    quarantinedBytes: 0,
    quarantineDir: { files: 0, bytes: 0, error: null },
    quarantined: [],
    ...overrides,
  };
}

function storage(signals: Partial<Record<'logs' | 'traces' | 'metrics', SignalRecovery>> = {}): StorageStats {
  const sig = (r: SignalRecovery) => ({ recovery: r }) as StorageStats['signals']['logs'];
  const all = { logs: recovery(), traces: recovery(), metrics: recovery(), ...signals };
  const affected = (['logs', 'traces', 'metrics'] as const).filter((k) => all[k].status !== 'clean');
  const worst = (['damaged', 'unknown', 'quarantineRetained'] as const).find((st) =>
    affected.some((k) => all[k].status === st),
  );
  const status = worst ?? 'clean';
  return {
    recovery: { status, signals: affected },
    signals: { logs: sig(all.logs), traces: sig(all.traces), metrics: sig(all.metrics) },
  } as StorageStats;
}

const tornTail = recovery({
  status: 'damaged',
  walBytesDiscarded: 61,
  walTailsQuarantined: 1,
  quarantinedBytes: 61,
  quarantineDir: { files: 1, bytes: 61, error: null },
  quarantined: [
    {
      signal: 'logs',
      kind: 'walTail',
      source: '000000000014.wal',
      file: 'quarantine/logs/000000000014.wal.tail-48213.1791622413000',
      bytes: 61,
      reason: 'incomplete record at offset 48213 (torn write)',
    },
  ],
});

describe('RecoveryHealth', () => {
  it('shows nothing after a clean start, even though the WAL was replayed', () => {
    const { container } = render(<RecoveryHealth storage={storage()} />);
    expect(container).toBeEmptyDOMElement();
    expect(screen.queryByRole('alert')).toBeNull();
  });

  it('warns about damage found this start with per-signal counts and a link to the recovery docs', () => {
    render(<RecoveryHealth storage={storage({ logs: tornTail })} />);
    const alert = screen.getByRole('alert');
    expect(alert).toHaveTextContent('Startup recovery set damaged data aside');
    expect(alert).toHaveTextContent('not queryable');
    expect(alert).toHaveTextContent('Vyrtel does not repair them');
    expect(within(alert).getByRole('link', { name: 'Recovery documentation' })).toHaveAttribute(
      'href',
      RECOVERY_DOCS_URL,
    );
    const rows = within(alert).getAllByRole('row');
    expect(rows).toHaveLength(2);
    expect(rows[1]).toHaveTextContent('logs');
    expect(rows[1]).toHaveTextContent('damaged this start');
    expect(rows[1]).toHaveTextContent('61 B');
    expect(rows[1]).toHaveTextContent('1 file');
    expect(alert).toHaveTextContent('incomplete record at offset 48213 (torn write)');
    expect(alert).toHaveTextContent('quarantine/logs/000000000014.wal.tail-48213.1791622413000');
    expect(alert).not.toHaveTextContent('traces');
  });

  it('warns about files left in quarantine by an earlier or interrupted recovery', () => {
    const retained = recovery({ status: 'quarantineRetained', quarantineDir: { files: 2, bytes: 4096, error: null } });
    render(<RecoveryHealth storage={storage({ traces: retained })} />);
    const alert = screen.getByRole('alert');
    expect(alert).toHaveTextContent('Quarantined files from an earlier start');
    expect(alert).toHaveTextContent('data for traces may be incomplete');
    const rows = within(alert).getAllByRole('row');
    expect(rows[1]).toHaveTextContent('quarantine retained');
    expect(rows[1]).toHaveTextContent('2 files');
  });

  it('never treats an unreadable quarantine directory as clean', () => {
    const unreadable = recovery({
      status: 'unknown',
      quarantineDir: {
        files: null,
        bytes: null,
        error: { reason: 'permissionDenied', message: 'Permission denied (os error 13)' },
      },
    });
    render(<RecoveryHealth storage={storage({ logs: unreadable })} />);
    const alert = screen.getByRole('alert');
    expect(alert).toHaveTextContent('The quarantine directory could not be checked');
    expect(alert).toHaveTextContent('data for logs may be incomplete');
    const rows = within(alert).getAllByRole('row');
    expect(rows[1]).toHaveTextContent('quarantine unreadable');
    expect(rows[1]).toHaveTextContent('unreadable (permissionDenied: Permission denied (os error 13))');
  });
});
