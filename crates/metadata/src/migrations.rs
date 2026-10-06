//! Schema migrations. Append new migrations; never edit applied ones.

pub const MIGRATIONS: &[(i64, &str)] = &[(
    1,
    r#"
CREATE TABLE settings (
    key        TEXT PRIMARY KEY,
    value      TEXT NOT NULL,
    updated_at INTEGER NOT NULL
);

CREATE TABLE api_keys (
    id           INTEGER PRIMARY KEY,
    name         TEXT NOT NULL,
    prefix       TEXT NOT NULL,
    key_hash     TEXT NOT NULL UNIQUE,
    scope        TEXT NOT NULL CHECK (scope IN ('ingest', 'admin')),
    created_at   INTEGER NOT NULL,
    last_used_at INTEGER,
    revoked_at   INTEGER
);

CREATE TABLE users (
    id            INTEGER PRIMARY KEY,
    username      TEXT NOT NULL UNIQUE,
    password_hash TEXT NOT NULL,
    created_at    INTEGER NOT NULL
);

CREATE TABLE sessions (
    token_hash TEXT PRIMARY KEY,
    user_id    INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    created_at INTEGER NOT NULL,
    expires_at INTEGER NOT NULL
);

CREATE TABLE dashboards (
    id          INTEGER PRIMARY KEY,
    name        TEXT NOT NULL,
    description TEXT,
    created_at  INTEGER NOT NULL,
    updated_at  INTEGER NOT NULL
);

CREATE TABLE dashboard_panels (
    id           INTEGER PRIMARY KEY,
    dashboard_id INTEGER NOT NULL REFERENCES dashboards(id) ON DELETE CASCADE,
    title        TEXT NOT NULL,
    kind         TEXT NOT NULL,
    config       TEXT NOT NULL,
    position     INTEGER NOT NULL,
    width        INTEGER NOT NULL DEFAULT 6,
    height       INTEGER NOT NULL DEFAULT 4,
    created_at   INTEGER NOT NULL,
    updated_at   INTEGER NOT NULL
);
CREATE INDEX dashboard_panels_dashboard ON dashboard_panels(dashboard_id, position);

CREATE TABLE saved_queries (
    id         INTEGER PRIMARY KEY,
    name       TEXT NOT NULL,
    signal     TEXT NOT NULL,
    query      TEXT NOT NULL,
    created_at INTEGER NOT NULL
);

CREATE TABLE alerts (
    id            INTEGER PRIMARY KEY,
    name          TEXT NOT NULL,
    query         TEXT NOT NULL,
    condition_op  TEXT NOT NULL CHECK (condition_op IN ('>', '>=', '<', '<=', '=', '!=')),
    threshold     REAL NOT NULL,
    window_secs   INTEGER NOT NULL CHECK (window_secs > 0),
    interval_secs INTEGER NOT NULL CHECK (interval_secs > 0),
    webhook_url   TEXT,
    enabled       INTEGER NOT NULL DEFAULT 1,
    created_at    INTEGER NOT NULL,
    updated_at    INTEGER NOT NULL
);

CREATE TABLE alert_state (
    alert_id                INTEGER PRIMARY KEY REFERENCES alerts(id) ON DELETE CASCADE,
    state                   TEXT NOT NULL CHECK (state IN ('OK', 'FIRING', 'ERROR')),
    last_value              REAL,
    last_evaluated_at       INTEGER,
    last_transition_at      INTEGER,
    last_error              TEXT,
    last_notification_at    INTEGER,
    last_notification_error TEXT
);

CREATE TABLE alert_history (
    id         INTEGER PRIMARY KEY,
    alert_id   INTEGER NOT NULL REFERENCES alerts(id) ON DELETE CASCADE,
    from_state TEXT NOT NULL,
    to_state   TEXT NOT NULL,
    value      REAL,
    message    TEXT,
    at         INTEGER NOT NULL
);
CREATE INDEX alert_history_alert ON alert_history(alert_id, at);

CREATE TABLE query_stats (
    signal           TEXT NOT NULL,
    field            TEXT NOT NULL,
    index_kind       TEXT NOT NULL,
    queries          INTEGER NOT NULL,
    bytes_read       INTEGER NOT NULL,
    segments_scanned INTEGER NOT NULL,
    segments_skipped INTEGER NOT NULL,
    total_ms         REAL NOT NULL,
    last_seen        INTEGER NOT NULL,
    PRIMARY KEY (signal, field)
);
"#,
)];
