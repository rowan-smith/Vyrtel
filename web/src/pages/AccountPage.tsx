import type { Account } from '../types';

export default function AccountPage({
  account,
  onLogout,
}: {
  account: Account;
  onLogout: () => void;
}) {
  return (
    <div className="page">
      <div className="page-header">
        <h1 className="page-title">Account</h1>
      </div>

      <section className="settings-section">
        <h2>Profile</h2>
        <div className="form-row">
          <span className="muted">Username</span>
          <span>{account.username}</span>
        </div>
        <div className="form-row">
          <span className="muted">Display name</span>
          <span>{account.displayName}</span>
        </div>
        <div className="form-row">
          <span className="muted">Created</span>
          <span>{new Date(account.createdAt).toLocaleString()}</span>
        </div>
        <div style={{ marginTop: '1rem' }}>
          <button type="button" className="btn" onClick={onLogout}>
            Sign out
          </button>
        </div>
      </section>
    </div>
  );
}
