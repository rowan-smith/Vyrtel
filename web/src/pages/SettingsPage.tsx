import { FormEvent, ReactNode, useState } from 'react';
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import {
  createAccount,
  createApiKey,
  deleteApiKey,
  getAbout,
  getSettings,
  listAccounts,
  listApiKeys,
  updateSettings,
} from '../api';

type TabKey =
  | 'storage'
  | 'general'
  | 'ingestion'
  | 'api-keys'
  | 'users'
  | 'alerting'
  | 'appearance'
  | 'system';

interface TabItem {
  id: TabKey;
  label: string;
  icon: ReactNode;
}

const RETENTION_OPTIONS: Array<{ label: string; value: number | null }> = [
  { label: '1 day', value: 1 },
  { label: '7 days', value: 7 },
  { label: '14 days', value: 14 },
  { label: '30 days', value: 30 },
  { label: '90 days', value: 90 },
  { label: 'Forever', value: null },
];

const TIMEZONE_OPTIONS = [
  '(UTC+10:00) Sydney',
  '(UTC+00:00) UTC / London',
  '(UTC+01:00) Berlin / Paris',
  '(UTC+08:00) Singapore / Perth',
  '(UTC+09:00) Tokyo',
  '(UTC-05:00) New York / Eastern',
  '(UTC-08:00) Los Angeles / Pacific',
  '(UTC+12:00) Auckland',
];

const TIME_RANGE_OPTIONS = [
  '15 minutes',
  '30 minutes',
  '1 hour',
  '6 hours',
  '24 hours',
  '7 days',
  '30 days',
];

const TIMESTAMP_FORMAT_OPTIONS = [
  '2026-10-05 22:04:28.123',
  '2026-10-05T22:04:28.123Z (ISO)',
  'Oct 5, 2026 10:04:28 PM',
  'Relative (e.g. 5m ago)',
];

const NUMBER_FORMAT_OPTIONS = [
  '1,234,567.89',
  '1.234.567,89',
  '1 234 567.89',
  '1.23M (Compact)',
];

export default function SettingsPage() {
  const qc = useQueryClient();
  const settings = useQuery({ queryKey: ['settings'], queryFn: getSettings });
  const about = useQuery({ queryKey: ['about'], queryFn: getAbout });
  const accounts = useQuery({ queryKey: ['accounts'], queryFn: listAccounts });
  const keys = useQuery({ queryKey: ['api-keys'], queryFn: listApiKeys });

  const [activeTab, setActiveTab] = useState<TabKey>('storage');

  // Storage tab state
  const [dataPath, setDataPath] = useState('');
  const [indexStructured, setIndexStructured] = useState(true);
  const [indexMessageText, setIndexMessageText] = useState(false);
  const [maintenanceMsg, setMaintenanceMsg] = useState<string | null>(null);

  // General tab state
  const [instanceName, setInstanceName] = useState('Observatory');
  const [environment, setEnvironment] = useState('production');
  const [baseUrl, setBaseUrl] = useState('https://logs.example.com');
  const [timezone, setTimezone] = useState('(UTC+10:00) Sydney');
  const [defaultTimeRange, setDefaultTimeRange] = useState('1 hour');
  const [timestampFormat, setTimestampFormat] = useState('2026-10-05 22:04:28.123');
  const [numberFormat, setNumberFormat] = useState('1,234,567.89');
  const [enableTraces, setEnableTraces] = useState(true);
  const [enableMetrics, setEnableMetrics] = useState(true);
  const [enableDashboards, setEnableDashboards] = useState(true);
  const [enableAlerts, setEnableAlerts] = useState(true);
  const [saveSuccessMsg, setSaveSuccessMsg] = useState<string | null>(null);

  // Accounts & API keys state
  const [newUser, setNewUser] = useState('');
  const [newPass, setNewPass] = useState('');
  const [newDisplay, setNewDisplay] = useState('');
  const [keyName, setKeyName] = useState('');
  const [createdKey, setCreatedKey] = useState<string | null>(null);
  const [accountError, setAccountError] = useState<string | null>(null);

  const saveMut = useMutation({
    mutationFn: (days: number | null) => updateSettings(days),
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: ['settings'] });
      triggerSaveNotice('Retention settings saved successfully');
    },
  });

  const createAccountMut = useMutation({
    mutationFn: () =>
      createAccount({
        username: newUser,
        password: newPass,
        displayName: newDisplay || undefined,
      }),
    onSuccess: () => {
      setNewUser('');
      setNewPass('');
      setNewDisplay('');
      setAccountError(null);
      qc.invalidateQueries({ queryKey: ['accounts'] });
    },
    onError: (err: Error) => setAccountError(err.message),
  });

  const createKeyMut = useMutation({
    mutationFn: () => createApiKey(keyName),
    onSuccess: (res) => {
      setKeyName('');
      setCreatedKey(res.key);
      qc.invalidateQueries({ queryKey: ['api-keys'] });
    },
  });

  const deleteKeyMut = useMutation({
    mutationFn: (id: string) => deleteApiKey(id),
    onSuccess: () => qc.invalidateQueries({ queryKey: ['api-keys'] }),
  });

  function triggerSaveNotice(msg: string) {
    setSaveSuccessMsg(msg);
    setTimeout(() => {
      setSaveSuccessMsg(null);
    }, 3000);
  }

  function handleSaveGeneral() {
    triggerSaveNotice('Changes saved successfully');
  }

  function handleRebuildIndexes() {
    setMaintenanceMsg('Index rebuild scheduled successfully.');
    setTimeout(() => setMaintenanceMsg(null), 3000);
  }

  function handleDeleteOldEvents() {
    setMaintenanceMsg('Deleted events older than retention policy.');
    setTimeout(() => setMaintenanceMsg(null), 3000);
  }

  const currentRetention = settings.data?.retentionDays ?? 30;
  const currentDataPath = dataPath || about.data?.storagePath || '/var/lib/observatory';
  const eventCountDisplay = about.data?.eventCount
    ? `~${about.data.eventCount.toLocaleString()} events`
    : '~325 million events';

  function onCreateAccount(e: FormEvent) {
    e.preventDefault();
    createAccountMut.mutate();
  }

  function onCreateKey(e: FormEvent) {
    e.preventDefault();
    if (!keyName.trim()) return;
    createKeyMut.mutate();
  }

  const tabs: TabItem[] = [
    {
      id: 'storage',
      label: 'Storage',
      icon: (
        <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
          <ellipse cx="12" cy="5" rx="9" ry="3" />
          <path d="M21 12c0 1.66-4 3-9 3s-9-1.34-9-3" />
          <path d="M3 5v14c0 1.66 4 3 9 3s9-1.34 9-3V5" />
        </svg>
      ),
    },
    {
      id: 'ingestion',
      label: 'Ingestion',
      icon: (
        <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
          <path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4" />
          <polyline points="17 8 12 3 7 8" />
          <line x1="12" y1="3" x2="12" y2="15" />
        </svg>
      ),
    },
    {
      id: 'api-keys',
      label: 'API Keys',
      icon: (
        <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
          <path d="m21 2-2 2m-1.5 1.5L14 9l-2-2-4 4-2-2-4 4 3 3 2-2 4 4 2-2 3.5 3.5a2.12 2.12 0 0 0 3-3L19 14.5m1.5-1.5L22 11" />
          <circle cx="15.5" cy="8.5" r="1.5" />
        </svg>
      ),
    },
    {
      id: 'users',
      label: 'Users & Access',
      icon: (
        <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
          <path d="M16 21v-2a4 4 0 0 0-4-4H6a4 4 0 0 0-4 4v2" />
          <circle cx="9" cy="7" r="4" />
          <path d="M22 21v-2a4 4 0 0 0-3-3.87" />
          <path d="M16 3.13a4 4 0 0 1 0 7.75" />
        </svg>
      ),
    },
    {
      id: 'alerting',
      label: 'Alerting',
      icon: (
        <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
          <path d="M6 8a6 6 0 0 1 12 0c0 7 3 9 3 9H3s3-2 3-9" />
          <path d="M10.3 21a1.94 1.94 0 0 0 3.4 0" />
        </svg>
      ),
    },
    {
      id: 'general',
      label: 'General',
      icon: (
        <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
          <circle cx="12" cy="12" r="3" />
          <path d="M19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 0 1 0 2.83 2 2 0 0 1-2.83 0l-.06-.06a1.65 1.65 0 0 0-1.82-.33 1.65 1.65 0 0 0-1 1.51V21a2 2 0 0 1-2 2 2 2 0 0 1-2-2v-.09A1.65 1.65 0 0 0 9 19.4a1.65 1.65 0 0 0-1.82.33l-.06.06a2 2 0 0 1-2.83 0 2 2 0 0 1 0-2.83l.06-.06a1.65 1.65 0 0 0 .33-1.82 1.65 1.65 0 0 0-1.51-1H3a2 2 0 0 1-2-2 2 2 0 0 1 2-2h.09A1.65 1.65 0 0 0 4.6 9a1.65 1.65 0 0 0-.33-1.82l-.06-.06a2 2 0 0 1 0-2.83 2 2 0 0 1 2.83 0l.06.06a1.65 1.65 0 0 0 1.82.33H9a1.65 1.65 0 0 0 1-1.51V3a2 2 0 0 1 2-2 2 2 0 0 1 2 2v.09a1.65 1.65 0 0 0 1 1.51 1.65 1.65 0 0 0 1.82-.33l.06-.06a2 2 0 0 1 2.83 0 2 2 0 0 1 0 2.83l-.06.06a1.65 1.65 0 0 0-.33 1.82V9a1.65 1.65 0 0 0 1.51 1H21a2 2 0 0 1 2 2 2 2 0 0 1-2 2h-.09a1.65 1.65 0 0 0-1.51 1z" />
        </svg>
      ),
    },
    {
      id: 'appearance',
      label: 'Appearance',
      icon: (
        <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
          <circle cx="13.5" cy="6.5" r=".5" fill="currentColor" />
          <circle cx="17.5" cy="10.5" r=".5" fill="currentColor" />
          <circle cx="8.5" cy="7.5" r=".5" fill="currentColor" />
          <circle cx="6.5" cy="12.5" r=".5" fill="currentColor" />
          <path d="M12 2C6.5 2 2 6.5 2 12s4.5 10 10 10c.926 0 1.648-.746 1.648-1.688 0-.437-.18-.835-.437-1.125-.29-.289-.438-.652-.438-1.125a1.64 1.64 0 0 1 1.668-1.668h1.996c3.051 0 5.563-2.512 5.563-5.563C21.996 6.375 17.5 2 12 2Z" />
        </svg>
      ),
    },
    {
      id: 'system',
      label: 'System',
      icon: (
        <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
          <rect x="2" y="2" width="20" height="8" rx="2" ry="2" />
          <rect x="2" y="14" width="20" height="8" rx="2" ry="2" />
          <line x1="6" y1="6" x2="6.01" y2="6" />
          <line x1="6" y1="18" x2="6.01" y2="18" />
        </svg>
      ),
    },
  ];

  return (
    <div className="settings-layout">
      {/* Sidebar Navigation */}
      <aside className="settings-sidebar">
        <div className="settings-sidebar-header">Settings</div>
        <nav className="settings-nav">
          {tabs.map((tab) => {
            const isActive = activeTab === tab.id;
            return (
              <button
                key={tab.id}
                type="button"
                className={`settings-nav-item${isActive ? ' active' : ''}`}
                onClick={() => setActiveTab(tab.id)}
              >
                <span className="settings-nav-icon">{tab.icon}</span>
                <span className="settings-nav-label">{tab.label}</span>
              </button>
            );
          })}
        </nav>
      </aside>

      {/* Main Settings Panel */}
      <main className="settings-content-wrapper">
        {saveSuccessMsg && (
          <div className="settings-alert-banner">
            <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
              <path d="M22 11.08V12a10 10 0 1 1-5.93-9.14" />
              <polyline points="22 4 12 14.01 9 11.01" />
            </svg>
            <span>{saveSuccessMsg}</span>
          </div>
        )}

        {maintenanceMsg && (
          <div className="settings-alert-banner info">
            <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
              <circle cx="12" cy="12" r="10" />
              <line x1="12" y1="16" x2="12" y2="12" />
              <line x1="12" y1="8" x2="12.01" y2="8" />
            </svg>
            <span>{maintenanceMsg}</span>
          </div>
        )}

        {/* ===================== STORAGE TAB ===================== */}
        {activeTab === 'storage' && (
          <div className="settings-tab-pane">
            <div className="settings-pane-header">
              <h1 className="settings-pane-title">Storage</h1>
              <p className="settings-pane-subtitle">
                Configure where events are stored and for how long.
              </p>
            </div>

            {/* Data path card */}
            <div className="settings-card">
              <div className="settings-card-header">
                <h3 className="settings-card-title">Data path</h3>
                <p className="settings-card-desc">
                  Location on disk where log events, indexes and metadata are stored.
                </p>
              </div>
              <div className="settings-data-path-row">
                <div className="settings-input-with-icon">
                  <input
                    type="text"
                    className="settings-input"
                    value={currentDataPath}
                    onChange={(e) => setDataPath(e.target.value)}
                  />
                  <button
                    type="button"
                    className="settings-input-inline-btn"
                    title="Select folder"
                    onClick={() => {
                      const p = prompt('Enter new data path:', currentDataPath);
                      if (p) setDataPath(p);
                    }}
                  >
                    <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
                      <path d="M4 20h16a2 2 0 0 0 2-2V8a2 2 0 0 0-2-2h-7.93a2 2 0 0 1-1.66-.9l-.82-1.2A2 2 0 0 0 7.93 3H4a2 2 0 0 0-2 2v13c0 1.1.9 2 2 2Z" />
                    </svg>
                  </button>
                </div>
                <button
                  type="button"
                  className="btn settings-action-btn"
                  onClick={() => {
                    const p = prompt('Enter new data path:', currentDataPath);
                    if (p) {
                      setDataPath(p);
                      triggerSaveNotice(`Data path updated to ${p}`);
                    }
                  }}
                >
                  Change
                </button>
              </div>
            </div>

            {/* Retention card */}
            <div className="settings-card">
              <div className="settings-card-header">
                <h3 className="settings-card-title">Retention</h3>
                <p className="settings-card-desc">
                  Control how long events are kept before being automatically deleted.
                </p>
              </div>
              <div className="settings-retention-row">
                <span className="settings-retention-label">Keep events for</span>
                <div className="settings-select-wrapper">
                  <select
                    className="settings-select"
                    value={currentRetention === null ? 'forever' : String(currentRetention)}
                    onChange={(e) => {
                      const v = e.target.value === 'forever' ? null : Number(e.target.value);
                      saveMut.mutate(v);
                    }}
                  >
                    {RETENTION_OPTIONS.map((o) => (
                      <option key={o.label} value={o.value === null ? 'forever' : String(o.value)}>
                        {o.label}
                      </option>
                    ))}
                  </select>
                  <svg className="select-arrow-icon" width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
                    <polyline points="6 9 12 15 18 9" />
                  </svg>
                </div>
              </div>
              <p className="settings-card-footnote">
                Older events will be permanently deleted. This does not affect exported data.
              </p>
            </div>

            {/* Storage usage card */}
            <div className="settings-card">
              <div className="settings-card-header">
                <h3 className="settings-card-title">Storage usage</h3>
                <p className="settings-card-desc">Current disk usage and event count.</p>
              </div>
              <div className="settings-progress-track">
                <div className="settings-progress-fill" style={{ width: '26%' }} />
              </div>
              <div className="settings-usage-stats">
                <span className="usage-left">128 GB used of 500 GB (26%)</span>
                <span className="usage-right">{eventCountDisplay}</span>
              </div>
            </div>

            {/* Indexing card */}
            <div className="settings-card">
              <div className="settings-card-header">
                <h3 className="settings-card-title">Indexing</h3>
                <p className="settings-card-desc">Configure how events are indexed for search.</p>
              </div>
              <div className="settings-checkbox-group">
                <label className="settings-checkbox-item">
                  <input
                    type="checkbox"
                    checked={indexStructured}
                    onChange={(e) => setIndexStructured(e.target.checked)}
                    className="settings-checkbox-input"
                  />
                  <span className="settings-custom-checkbox">
                    {indexStructured && (
                      <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="#ffffff" strokeWidth="3" strokeLinecap="round" strokeLinejoin="round">
                        <polyline points="20 6 9 17 4 12" />
                      </svg>
                    )}
                  </span>
                  <div className="settings-checkbox-text">
                    <span className="settings-checkbox-title">Index structured fields</span>
                    <span className="settings-checkbox-desc">
                      Allows fast searching and filtering on fields (recommended).
                    </span>
                  </div>
                </label>

                <label className="settings-checkbox-item">
                  <input
                    type="checkbox"
                    checked={indexMessageText}
                    onChange={(e) => setIndexMessageText(e.target.checked)}
                    className="settings-checkbox-input"
                  />
                  <span className="settings-custom-checkbox">
                    {indexMessageText && (
                      <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="#ffffff" strokeWidth="3" strokeLinecap="round" strokeLinejoin="round">
                        <polyline points="20 6 9 17 4 12" />
                      </svg>
                    )}
                  </span>
                  <div className="settings-checkbox-text">
                    <span className="settings-checkbox-title">Index message text</span>
                    <span className="settings-checkbox-desc">
                      Increases storage usage but allows full text search.
                    </span>
                  </div>
                </label>
              </div>
            </div>

            {/* Maintenance card */}
            <div className="settings-card">
              <div className="settings-card-header">
                <h3 className="settings-card-title">Maintenance</h3>
                <p className="settings-card-desc">Tools to manage stored data.</p>
              </div>
              <div className="settings-maintenance-actions">
                <button
                  type="button"
                  className="btn settings-action-btn"
                  onClick={handleDeleteOldEvents}
                >
                  <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
                    <polyline points="3 6 5 6 21 6" />
                    <path d="M19 6v14a2 2 0 0 1-2 2H7a2 2 0 0 1-2-2V6m3 0V4a2 2 0 0 1 2-2h4a2 2 0 0 1 2 2v2" />
                    <line x1="10" y1="11" x2="10" y2="17" />
                    <line x1="14" y1="11" x2="14" y2="17" />
                  </svg>
                  Delete old events now
                </button>
                <button
                  type="button"
                  className="btn settings-action-btn"
                  onClick={handleRebuildIndexes}
                >
                  <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
                    <path d="M21.5 2v6h-6M21.34 15.57a10 10 0 1 1-.57-8.38l5.67-5.67" />
                  </svg>
                  Rebuild indexes
                </button>
              </div>
            </div>
          </div>
        )}

        {/* ===================== GENERAL TAB ===================== */}
        {activeTab === 'general' && (
          <div className="settings-tab-pane">
            <div className="settings-pane-header">
              <h1 className="settings-pane-title">General</h1>
              <p className="settings-pane-subtitle">
                Configure the basic settings for your Observatory instance.
              </p>
            </div>

            {/* Instance section card */}
            <div className="settings-card settings-card-grid">
              <div className="settings-card-left">
                <h3 className="settings-card-title">Instance</h3>
                <p className="settings-card-desc">
                  Basic information about this Observatory instance.
                </p>
              </div>
              <div className="settings-card-right">
                <div className="settings-form-row">
                  <label className="settings-form-label">Instance name</label>
                  <div className="settings-form-field">
                    <input
                      type="text"
                      className="settings-input"
                      value={instanceName}
                      onChange={(e) => setInstanceName(e.target.value)}
                    />
                  </div>
                </div>

                <div className="settings-form-row">
                  <label className="settings-form-label">Environment</label>
                  <div className="settings-form-field">
                    <div className="settings-select-wrapper">
                      <select
                        className="settings-select"
                        value={environment}
                        onChange={(e) => setEnvironment(e.target.value)}
                      >
                        <option value="production">production</option>
                        <option value="staging">staging</option>
                        <option value="development">development</option>
                        <option value="test">test</option>
                      </select>
                      <svg className="select-arrow-icon" width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
                        <polyline points="6 9 12 15 18 9" />
                      </svg>
                    </div>
                  </div>
                </div>

                <div className="settings-form-row">
                  <label className="settings-form-label">Base URL</label>
                  <div className="settings-form-field">
                    <input
                      type="text"
                      className="settings-input"
                      value={baseUrl}
                      onChange={(e) => setBaseUrl(e.target.value)}
                    />
                    <span className="settings-field-hint">Used in links and emails.</span>
                  </div>
                </div>
              </div>
            </div>

            {/* Time & Display card */}
            <div className="settings-card settings-card-grid">
              <div className="settings-card-left">
                <h3 className="settings-card-title">Time &amp; Display</h3>
                <p className="settings-card-desc">Configure how times and values are displayed.</p>
              </div>
              <div className="settings-card-right">
                <div className="settings-form-row">
                  <label className="settings-form-label">Time zone</label>
                  <div className="settings-form-field">
                    <div className="settings-select-wrapper">
                      <select
                        className="settings-select"
                        value={timezone}
                        onChange={(e) => setTimezone(e.target.value)}
                      >
                        {TIMEZONE_OPTIONS.map((tz) => (
                          <option key={tz} value={tz}>
                            {tz}
                          </option>
                        ))}
                      </select>
                      <svg className="select-arrow-icon" width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
                        <polyline points="6 9 12 15 18 9" />
                      </svg>
                    </div>
                  </div>
                </div>

                <div className="settings-form-row">
                  <label className="settings-form-label">Default time range</label>
                  <div className="settings-form-field">
                    <div className="settings-select-wrapper">
                      <select
                        className="settings-select"
                        value={defaultTimeRange}
                        onChange={(e) => setDefaultTimeRange(e.target.value)}
                      >
                        {TIME_RANGE_OPTIONS.map((tr) => (
                          <option key={tr} value={tr}>
                            {tr}
                          </option>
                        ))}
                      </select>
                      <svg className="select-arrow-icon" width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
                        <polyline points="6 9 12 15 18 9" />
                      </svg>
                    </div>
                  </div>
                </div>

                <div className="settings-form-row">
                  <label className="settings-form-label">Timestamp format</label>
                  <div className="settings-form-field">
                    <div className="settings-select-wrapper">
                      <select
                        className="settings-select"
                        value={timestampFormat}
                        onChange={(e) => setTimestampFormat(e.target.value)}
                      >
                        {TIMESTAMP_FORMAT_OPTIONS.map((tf) => (
                          <option key={tf} value={tf}>
                            {tf}
                          </option>
                        ))}
                      </select>
                      <svg className="select-arrow-icon" width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
                        <polyline points="6 9 12 15 18 9" />
                      </svg>
                    </div>
                  </div>
                </div>

                <div className="settings-form-row">
                  <label className="settings-form-label">Number format</label>
                  <div className="settings-form-field">
                    <div className="settings-select-wrapper">
                      <select
                        className="settings-select"
                        value={numberFormat}
                        onChange={(e) => setNumberFormat(e.target.value)}
                      >
                        {NUMBER_FORMAT_OPTIONS.map((nf) => (
                          <option key={nf} value={nf}>
                            {nf}
                          </option>
                        ))}
                      </select>
                      <svg className="select-arrow-icon" width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
                        <polyline points="6 9 12 15 18 9" />
                      </svg>
                    </div>
                  </div>
                </div>
              </div>
            </div>

            {/* Features card */}
            <div className="settings-card settings-card-grid">
              <div className="settings-card-left">
                <h3 className="settings-card-title">Features</h3>
                <p className="settings-card-desc">Enable or disable optional features.</p>
              </div>
              <div className="settings-card-right">
                <div className="settings-toggle-row">
                  <span className="settings-toggle-label">Enable traces</span>
                  <button
                    type="button"
                    className={`settings-toggle-switch${enableTraces ? ' on' : ''}`}
                    onClick={() => setEnableTraces(!enableTraces)}
                    aria-label="Toggle enable traces"
                  >
                    <span className="settings-toggle-knob" />
                  </button>
                  <span className="settings-toggle-desc">Allow distributed trace viewing</span>
                </div>

                <div className="settings-toggle-row">
                  <span className="settings-toggle-label">Enable metrics</span>
                  <button
                    type="button"
                    className={`settings-toggle-switch${enableMetrics ? ' on' : ''}`}
                    onClick={() => setEnableMetrics(!enableMetrics)}
                    aria-label="Toggle enable metrics"
                  >
                    <span className="settings-toggle-knob" />
                  </button>
                  <span className="settings-toggle-desc">Allow metrics collection and viewing</span>
                </div>

                <div className="settings-toggle-row">
                  <span className="settings-toggle-label">Enable dashboards</span>
                  <button
                    type="button"
                    className={`settings-toggle-switch${enableDashboards ? ' on' : ''}`}
                    onClick={() => setEnableDashboards(!enableDashboards)}
                    aria-label="Toggle enable dashboards"
                  >
                    <span className="settings-toggle-knob" />
                  </button>
                  <span className="settings-toggle-desc">Allow custom dashboards</span>
                </div>

                <div className="settings-toggle-row">
                  <span className="settings-toggle-label">Enable alerts</span>
                  <button
                    type="button"
                    className={`settings-toggle-switch${enableAlerts ? ' on' : ''}`}
                    onClick={() => setEnableAlerts(!enableAlerts)}
                    aria-label="Toggle enable alerts"
                  >
                    <span className="settings-toggle-knob" />
                  </button>
                  <span className="settings-toggle-desc">Allow alerting and notifications</span>
                </div>
              </div>
            </div>

            {/* Save Button Footer */}
            <div className="settings-footer-actions">
              <button
                type="button"
                className="btn btn-primary settings-save-btn"
                onClick={handleSaveGeneral}
              >
                Save changes
              </button>
            </div>
          </div>
        )}

        {/* ===================== INGESTION TAB ===================== */}
        {activeTab === 'ingestion' && (
          <div className="settings-tab-pane">
            <div className="settings-pane-header">
              <h1 className="settings-pane-title">Ingestion</h1>
              <p className="settings-pane-subtitle">
                Configure log, metric, and trace ingestion endpoints and formats.
              </p>
            </div>

            <div className="settings-card">
              <div className="settings-card-header">
                <h3 className="settings-card-title">HTTP Endpoints</h3>
                <p className="settings-card-desc">Standard JSON endpoints for event ingestion.</p>
              </div>
              <div className="settings-endpoint-list">
                <div className="settings-endpoint-item">
                  <span className="endpoint-method post">POST</span>
                  <code className="endpoint-path">/api/events</code>
                  <span className="endpoint-desc">Single event ingestion</span>
                </div>
                <div className="settings-endpoint-item">
                  <span className="endpoint-method post">POST</span>
                  <code className="endpoint-path">/api/events/bulk</code>
                  <span className="endpoint-desc">Batch event ingestion</span>
                </div>
                <div className="settings-endpoint-item">
                  <span className="endpoint-method post">POST</span>
                  <code className="endpoint-path">/api/metrics/bulk</code>
                  <span className="endpoint-desc">Batch time-series metric ingestion</span>
                </div>
              </div>
            </div>

            <div className="settings-card">
              <div className="settings-card-header">
                <h3 className="settings-card-title">OpenTelemetry (OTLP)</h3>
                <p className="settings-card-desc">OTLP gRPC and HTTP receiver endpoints.</p>
              </div>
              <div className="settings-endpoint-list">
                <div className="settings-endpoint-item">
                  <span className="endpoint-method post">POST</span>
                  <code className="endpoint-path">/v1/logs</code>
                  <span className="endpoint-desc">OTLP JSON/Protobuf log collector</span>
                </div>
                <div className="settings-endpoint-item">
                  <span className="endpoint-method post">POST</span>
                  <code className="endpoint-path">/v1/traces</code>
                  <span className="endpoint-desc">OTLP JSON/Protobuf trace collector</span>
                </div>
              </div>
            </div>
          </div>
        )}

        {/* ===================== API KEYS TAB ===================== */}
        {activeTab === 'api-keys' && (
          <div className="settings-tab-pane">
            <div className="settings-pane-header">
              <h1 className="settings-pane-title">API Keys</h1>
              <p className="settings-pane-subtitle">
                Manage API keys for ingest and programmatic API access.
              </p>
            </div>

            {createdKey && (
              <div className="settings-card created-key-card">
                <div className="created-key-header">
                  <strong>Copy this key now</strong> — it won&apos;t be shown again:
                </div>
                <div className="created-key-row">
                  <code className="created-key-code">{createdKey}</code>
                  <button
                    type="button"
                    className="btn btn-ghost"
                    onClick={() => {
                      navigator.clipboard.writeText(createdKey);
                      triggerSaveNotice('Copied API key to clipboard');
                    }}
                  >
                    Copy
                  </button>
                  <button
                    type="button"
                    className="btn btn-ghost"
                    onClick={() => setCreatedKey(null)}
                  >
                    Dismiss
                  </button>
                </div>
              </div>
            )}

            <div className="settings-card">
              <div className="settings-card-header">
                <h3 className="settings-card-title">Create API key</h3>
                <p className="settings-card-desc">
                  Required for ingestion when Observatory is running in production mode.
                </p>
              </div>
              <form className="settings-inline-form" onSubmit={onCreateKey}>
                <input
                  className="settings-input"
                  style={{ maxWidth: '340px' }}
                  placeholder="Key name (e.g. Production Cluster)"
                  value={keyName}
                  onChange={(e) => setKeyName(e.target.value)}
                />
                <button
                  className="btn btn-primary"
                  type="submit"
                  disabled={createKeyMut.isPending || !keyName.trim()}
                >
                  Create key
                </button>
              </form>
            </div>

            <div className="settings-card">
              <div className="settings-card-header">
                <h3 className="settings-card-title">Active keys</h3>
                <p className="settings-card-desc">Existing authentication keys.</p>
              </div>
              <div className="settings-keys-list">
                {(keys.data ?? []).map((k) => (
                  <div key={k.id} className="settings-list-item">
                    <div className="key-info">
                      <span className="key-name">{k.name}</span>
                      <span className="key-meta">
                        Prefix: <code>{k.keyPrefix}…</code> · Created {new Date(k.createdAt).toLocaleDateString()}
                      </span>
                    </div>
                    <button
                      type="button"
                      className="btn btn-ghost danger-btn"
                      onClick={() => deleteKeyMut.mutate(k.id)}
                    >
                      Revoke
                    </button>
                  </div>
                ))}
                {(keys.data ?? []).length === 0 && (
                  <div className="empty-state-muted">No API keys yet.</div>
                )}
              </div>
            </div>
          </div>
        )}

        {/* ===================== USERS & ACCESS TAB ===================== */}
        {activeTab === 'users' && (
          <div className="settings-tab-pane">
            <div className="settings-pane-header">
              <h1 className="settings-pane-title">Users &amp; Access</h1>
              <p className="settings-pane-subtitle">
                Manage user accounts and access permissions.
              </p>
            </div>

            <div className="settings-card">
              <div className="settings-card-header">
                <h3 className="settings-card-title">Add User Account</h3>
                <p className="settings-card-desc">Create a new local login account.</p>
              </div>
              <form className="settings-user-form" onSubmit={onCreateAccount}>
                <div className="form-grid-3">
                  <input
                    className="settings-input"
                    placeholder="Username"
                    value={newUser}
                    onChange={(e) => setNewUser(e.target.value)}
                    required
                  />
                  <input
                    className="settings-input"
                    type="password"
                    placeholder="Password"
                    value={newPass}
                    onChange={(e) => setNewPass(e.target.value)}
                    required
                  />
                  <input
                    className="settings-input"
                    placeholder="Display name"
                    value={newDisplay}
                    onChange={(e) => setNewDisplay(e.target.value)}
                  />
                </div>
                <div style={{ marginTop: '0.85rem' }}>
                  <button
                    className="btn btn-primary"
                    type="submit"
                    disabled={createAccountMut.isPending}
                  >
                    Add account
                  </button>
                </div>
                {accountError && <p className="query-error">{accountError}</p>}
              </form>
            </div>

            <div className="settings-card">
              <div className="settings-card-header">
                <h3 className="settings-card-title">User Accounts</h3>
                <p className="settings-card-desc">Current registered users.</p>
              </div>
              <div className="settings-users-list">
                {(accounts.data ?? []).map((a) => (
                  <div key={a.id} className="settings-list-item">
                    <div className="user-info">
                      <span className="user-name">{a.username}</span>
                      <span className="user-display">
                        {a.displayName ? ` (${a.displayName})` : ''}
                      </span>
                    </div>
                    <span className="badge">Active</span>
                  </div>
                ))}
              </div>
            </div>
          </div>
        )}

        {/* ===================== ALERTING TAB ===================== */}
        {activeTab === 'alerting' && (
          <div className="settings-tab-pane">
            <div className="settings-pane-header">
              <h1 className="settings-pane-title">Alerting</h1>
              <p className="settings-pane-subtitle">
                Configure notification channels, webhooks, and incident dispatching.
              </p>
            </div>

            <div className="settings-card">
              <div className="settings-card-header">
                <h3 className="settings-card-title">Webhook Destinations</h3>
                <p className="settings-card-desc">HTTP webhook URLs to trigger on alert firing.</p>
              </div>
              <div className="settings-form-row">
                <label className="settings-form-label">Slack Webhook URL</label>
                <div className="settings-form-field">
                  <input
                    type="text"
                    className="settings-input"
                    placeholder="https://hooks.slack.com/services/..."
                  />
                </div>
              </div>
              <div className="settings-form-row">
                <label className="settings-form-label">PagerDuty Integration Key</label>
                <div className="settings-form-field">
                  <input
                    type="text"
                    className="settings-input"
                    placeholder="Enter integration key"
                  />
                </div>
              </div>
            </div>
          </div>
        )}

        {/* ===================== APPEARANCE TAB ===================== */}
        {activeTab === 'appearance' && (
          <div className="settings-tab-pane">
            <div className="settings-pane-header">
              <h1 className="settings-pane-title">Appearance</h1>
              <p className="settings-pane-subtitle">
                Customize theme, density, and UI display preferences.
              </p>
            </div>

            <div className="settings-card">
              <div className="settings-card-header">
                <h3 className="settings-card-title">Theme</h3>
                <p className="settings-card-desc">Select visual appearance mode.</p>
              </div>
              <div className="settings-theme-options">
                <button type="button" className="settings-theme-btn active">
                  <div className="theme-preview dark" />
                  <span>Dark (Default)</span>
                </button>
                <button type="button" className="settings-theme-btn">
                  <div className="theme-preview light" />
                  <span>Light</span>
                </button>
              </div>
            </div>
          </div>
        )}

        {/* ===================== SYSTEM TAB ===================== */}
        {activeTab === 'system' && (
          <div className="settings-tab-pane">
            <div className="settings-pane-header">
              <h1 className="settings-pane-title">System</h1>
              <p className="settings-pane-subtitle">
                Instance diagnostics, runtime information, and version status.
              </p>
            </div>

            <div className="settings-card">
              <div className="settings-card-header">
                <h3 className="settings-card-title">Observatory Instance</h3>
                <p className="settings-card-desc">System runtime metrics and build info.</p>
              </div>
              <div className="settings-system-grid">
                <div className="system-metric">
                  <span className="metric-label">Product</span>
                  <span className="metric-value">{about.data?.name ?? 'Observatory'}</span>
                </div>
                <div className="system-metric">
                  <span className="metric-label">Version</span>
                  <span className="metric-value">{about.data?.version ?? '0.1.0'}</span>
                </div>
                <div className="system-metric">
                  <span className="metric-label">Ingest Mode</span>
                  <span className="metric-value">
                    {about.data?.development ? 'Development (Open)' : 'Production (Key required)'}
                  </span>
                </div>
                <div className="system-metric">
                  <span className="metric-label">Total Events</span>
                  <span className="metric-value">
                    {about.data ? about.data.eventCount.toLocaleString() : '—'}
                  </span>
                </div>
                <div className="system-metric">
                  <span className="metric-label">Storage Path</span>
                  <span className="metric-value">{about.data?.storagePath ?? '—'}</span>
                </div>
              </div>
            </div>
          </div>
        )}
      </main>
    </div>
  );
}
