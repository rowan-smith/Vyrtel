use chrono::{DateTime, Duration as ChronoDuration, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::{sqlite::SqlitePoolOptions, FromRow, SqlitePool};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
#[serde(rename_all = "camelCase")]
pub struct SavedFilter {
    pub id: String,
    pub name: String,
    pub query: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
#[serde(rename_all = "camelCase")]
pub struct Dashboard {
    pub id: String,
    pub name: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Visualization {
    Number,
    Line,
    Bar,
    Table,
}

impl Visualization {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Number => "number",
            Self::Line => "line",
            Self::Bar => "bar",
            Self::Table => "table",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "number" => Some(Self::Number),
            "line" => Some(Self::Line),
            "bar" => Some(Self::Bar),
            "table" => Some(Self::Table),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WidgetPosition {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DashboardWidget {
    pub id: String,
    pub dashboard_id: String,
    pub title: String,
    pub query: String,
    pub visualization: Visualization,
    pub position: WidgetPosition,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppSettings {
    pub retention_days: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AlertOperator {
    Gt,
    Gte,
    Lt,
    Lte,
    Eq,
}

impl AlertOperator {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Gt => "gt",
            Self::Gte => "gte",
            Self::Lt => "lt",
            Self::Lte => "lte",
            Self::Eq => "eq",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "gt" | ">" => Some(Self::Gt),
            "gte" | ">=" => Some(Self::Gte),
            "lt" | "<" => Some(Self::Lt),
            "lte" | "<=" => Some(Self::Lte),
            "eq" | "=" => Some(Self::Eq),
            _ => None,
        }
    }

    pub fn compare(self, value: f64, threshold: f64) -> bool {
        match self {
            Self::Gt => value > threshold,
            Self::Gte => value >= threshold,
            Self::Lt => value < threshold,
            Self::Lte => value <= threshold,
            Self::Eq => (value - threshold).abs() < f64::EPSILON,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AlertStatus {
    Ok,
    Firing,
}

impl AlertStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Firing => "firing",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "ok" => Some(Self::Ok),
            "firing" => Some(Self::Firing),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AlertRule {
    pub id: String,
    pub name: String,
    pub query: String,
    pub operator: AlertOperator,
    pub threshold: f64,
    pub enabled: bool,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AlertState {
    pub rule_id: String,
    pub status: AlertStatus,
    pub value: Option<f64>,
    pub message: Option<String>,
    pub last_evaluated_at: Option<DateTime<Utc>>,
    pub last_changed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AlertView {
    #[serde(flatten)]
    pub rule: AlertRule,
    pub status: AlertStatus,
    pub value: Option<f64>,
    pub message: Option<String>,
    pub last_evaluated_at: Option<DateTime<Utc>>,
    pub last_changed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Account {
    pub id: String,
    pub username: String,
    pub display_name: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct AccountRecord {
    pub id: String,
    pub username: String,
    pub password_hash: String,
    pub display_name: String,
    pub created_at: DateTime<Utc>,
}

impl AccountRecord {
    pub fn into_account(self) -> Account {
        Account {
            id: self.id,
            username: self.username,
            display_name: self.display_name,
            created_at: self.created_at,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiKeyInfo {
    pub id: String,
    pub account_id: String,
    pub name: String,
    pub key_prefix: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreatedApiKey {
    #[serde(flatten)]
    pub info: ApiKeyInfo,
    /// Full key — only returned once at creation time.
    pub key: String,
}

#[derive(Debug, Clone)]
pub struct SessionInfo {
    pub token: String,
    pub account_id: String,
    pub expires_at: DateTime<Utc>,
}

pub struct MetadataStore {
    pool: SqlitePool,
}

impl MetadataStore {
    pub async fn open(path: &std::path::Path) -> anyhow::Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let url = format!("sqlite://{}?mode=rwc", path.display());
        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect(&url)
            .await?;
        let store = Self { pool };
        store.migrate().await?;
        Ok(store)
    }

    async fn migrate(&self) -> anyhow::Result<()> {
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS saved_filters (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                query TEXT NOT NULL,
                created_at TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS dashboards (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                created_at TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS dashboard_widgets (
                id TEXT PRIMARY KEY,
                dashboard_id TEXT NOT NULL,
                title TEXT NOT NULL,
                query TEXT NOT NULL,
                visualization TEXT NOT NULL,
                pos_x INTEGER NOT NULL DEFAULT 0,
                pos_y INTEGER NOT NULL DEFAULT 0,
                pos_w INTEGER NOT NULL DEFAULT 1,
                pos_h INTEGER NOT NULL DEFAULT 1,
                FOREIGN KEY(dashboard_id) REFERENCES dashboards(id) ON DELETE CASCADE
            );

            CREATE TABLE IF NOT EXISTS settings (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS alert_rules (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                query TEXT NOT NULL,
                operator TEXT NOT NULL,
                threshold REAL NOT NULL,
                enabled INTEGER NOT NULL DEFAULT 1,
                created_at TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS alert_states (
                rule_id TEXT PRIMARY KEY,
                status TEXT NOT NULL,
                value REAL,
                message TEXT,
                last_evaluated_at TEXT,
                last_changed_at TEXT,
                FOREIGN KEY(rule_id) REFERENCES alert_rules(id) ON DELETE CASCADE
            );

            CREATE TABLE IF NOT EXISTS accounts (
                id TEXT PRIMARY KEY,
                username TEXT NOT NULL UNIQUE,
                password_hash TEXT NOT NULL,
                display_name TEXT NOT NULL,
                created_at TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS sessions (
                token TEXT PRIMARY KEY,
                account_id TEXT NOT NULL,
                created_at TEXT NOT NULL,
                expires_at TEXT NOT NULL,
                FOREIGN KEY(account_id) REFERENCES accounts(id) ON DELETE CASCADE
            );
            "#,
        )
        .execute(&self.pool)
        .await?;

        self.migrate_api_keys().await?;
        self.ensure_default_admin().await?;
        Ok(())
    }

    async fn migrate_api_keys(&self) -> anyhow::Result<()> {
        let cols: Vec<(String,)> =
            sqlx::query_as("SELECT name FROM pragma_table_info('api_keys')")
                .fetch_all(&self.pool)
                .await
                .unwrap_or_default();
        let names: Vec<&str> = cols.iter().map(|(n,)| n.as_str()).collect();
        let needs_recreate = names.is_empty() || !names.contains(&"account_id");
        if !names.is_empty() && needs_recreate {
            sqlx::query("DROP TABLE IF EXISTS api_keys")
                .execute(&self.pool)
                .await?;
        }
        if needs_recreate {
            sqlx::query(
                r#"
                CREATE TABLE IF NOT EXISTS api_keys (
                    id TEXT PRIMARY KEY,
                    account_id TEXT NOT NULL,
                    name TEXT NOT NULL,
                    key_prefix TEXT NOT NULL,
                    key_hash TEXT NOT NULL UNIQUE,
                    created_at TEXT NOT NULL,
                    FOREIGN KEY(account_id) REFERENCES accounts(id) ON DELETE CASCADE
                );
                "#,
            )
            .execute(&self.pool)
            .await?;
        }
        Ok(())
    }

    async fn ensure_default_admin(&self) -> anyhow::Result<()> {
        let (count,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM accounts")
            .fetch_one(&self.pool)
            .await?;
        if count == 0 {
            self.create_account("admin", "admin", "Administrator")
                .await?;
        }
        Ok(())
    }

    // --- Accounts / sessions / API keys ---

    pub async fn create_account(
        &self,
        username: &str,
        password: &str,
        display_name: &str,
    ) -> anyhow::Result<Account> {
        let id = Uuid::new_v4().to_string();
        let created_at = Utc::now();
        let password_hash = hash_password(password);
        sqlx::query(
            r#"INSERT INTO accounts (id, username, password_hash, display_name, created_at)
               VALUES (?, ?, ?, ?, ?)"#,
        )
        .bind(&id)
        .bind(username)
        .bind(&password_hash)
        .bind(display_name)
        .bind(created_at.to_rfc3339())
        .execute(&self.pool)
        .await?;
        Ok(Account {
            id,
            username: username.to_string(),
            display_name: display_name.to_string(),
            created_at,
        })
    }

    pub async fn list_accounts(&self) -> anyhow::Result<Vec<Account>> {
        let rows = sqlx::query_as::<_, AccountRow>(
            r#"SELECT id, username, password_hash, display_name, created_at
               FROM accounts ORDER BY username"#,
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(|r| r.into_record().into_account()).collect())
    }

    pub async fn find_account_by_username(
        &self,
        username: &str,
    ) -> anyhow::Result<Option<AccountRecord>> {
        let row = sqlx::query_as::<_, AccountRow>(
            r#"SELECT id, username, password_hash, display_name, created_at
               FROM accounts WHERE username = ?"#,
        )
        .bind(username)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|r| r.into_record()))
    }

    pub async fn get_account(&self, id: &str) -> anyhow::Result<Option<Account>> {
        let row = sqlx::query_as::<_, AccountRow>(
            r#"SELECT id, username, password_hash, display_name, created_at
               FROM accounts WHERE id = ?"#,
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|r| r.into_record().into_account()))
    }

    pub async fn verify_login(
        &self,
        username: &str,
        password: &str,
    ) -> anyhow::Result<Option<Account>> {
        let Some(record) = self.find_account_by_username(username).await? else {
            return Ok(None);
        };
        if verify_password(password, &record.password_hash) {
            Ok(Some(record.into_account()))
        } else {
            Ok(None)
        }
    }

    pub async fn create_session(
        &self,
        account_id: &str,
        ttl_hours: i64,
    ) -> anyhow::Result<SessionInfo> {
        let token = Uuid::new_v4().to_string();
        let created_at = Utc::now();
        let expires_at = created_at + ChronoDuration::hours(ttl_hours);
        sqlx::query(
            r#"INSERT INTO sessions (token, account_id, created_at, expires_at)
               VALUES (?, ?, ?, ?)"#,
        )
        .bind(&token)
        .bind(account_id)
        .bind(created_at.to_rfc3339())
        .bind(expires_at.to_rfc3339())
        .execute(&self.pool)
        .await?;
        Ok(SessionInfo {
            token,
            account_id: account_id.to_string(),
            expires_at,
        })
    }

    pub async fn get_session_account(&self, token: &str) -> anyhow::Result<Option<Account>> {
        let row = sqlx::query_as::<_, SessionJoinRow>(
            r#"
            SELECT a.id, a.username, a.password_hash, a.display_name, a.created_at, s.expires_at
            FROM sessions s
            JOIN accounts a ON a.id = s.account_id
            WHERE s.token = ?
            "#,
        )
        .bind(token)
        .fetch_optional(&self.pool)
        .await?;
        let Some(row) = row else {
            return Ok(None);
        };
        let expires_at = parse_dt(&row.expires_at).unwrap_or_else(Utc::now);
        if expires_at < Utc::now() {
            let _ = self.delete_session(token).await;
            return Ok(None);
        }
        Ok(Some(Account {
            id: row.id,
            username: row.username,
            display_name: row.display_name,
            created_at: parse_dt(&row.created_at).unwrap_or_else(Utc::now),
        }))
    }

    pub async fn delete_session(&self, token: &str) -> anyhow::Result<()> {
        sqlx::query("DELETE FROM sessions WHERE token = ?")
            .bind(token)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn create_api_key(
        &self,
        account_id: &str,
        name: &str,
    ) -> anyhow::Result<CreatedApiKey> {
        let id = Uuid::new_v4().to_string();
        let created_at = Utc::now();
        let raw_key = format!("obs_{}", Uuid::new_v4().simple());
        let key_prefix = raw_key.chars().take(12).collect::<String>();
        let key_hash = hash_api_key(&raw_key);
        sqlx::query(
            r#"INSERT INTO api_keys (id, account_id, name, key_prefix, key_hash, created_at)
               VALUES (?, ?, ?, ?, ?, ?)"#,
        )
        .bind(&id)
        .bind(account_id)
        .bind(name)
        .bind(&key_prefix)
        .bind(&key_hash)
        .bind(created_at.to_rfc3339())
        .execute(&self.pool)
        .await?;
        Ok(CreatedApiKey {
            info: ApiKeyInfo {
                id,
                account_id: account_id.to_string(),
                name: name.to_string(),
                key_prefix,
                created_at,
            },
            key: raw_key,
        })
    }

    pub async fn list_api_keys(&self, account_id: &str) -> anyhow::Result<Vec<ApiKeyInfo>> {
        let rows = sqlx::query_as::<_, ApiKeyRow>(
            r#"SELECT id, account_id, name, key_prefix, created_at
               FROM api_keys WHERE account_id = ? ORDER BY created_at DESC"#,
        )
        .bind(account_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(Into::into).collect())
    }

    pub async fn list_all_api_keys(&self) -> anyhow::Result<Vec<ApiKeyInfo>> {
        let rows = sqlx::query_as::<_, ApiKeyRow>(
            r#"SELECT id, account_id, name, key_prefix, created_at
               FROM api_keys ORDER BY created_at DESC"#,
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(Into::into).collect())
    }

    pub async fn delete_api_key(&self, id: &str) -> anyhow::Result<()> {
        sqlx::query("DELETE FROM api_keys WHERE id = ?")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn find_account_by_api_key(
        &self,
        raw_key: &str,
    ) -> anyhow::Result<Option<Account>> {
        let key_hash = hash_api_key(raw_key);
        let row = sqlx::query_as::<_, AccountRow>(
            r#"
            SELECT a.id, a.username, a.password_hash, a.display_name, a.created_at
            FROM api_keys k
            JOIN accounts a ON a.id = k.account_id
            WHERE k.key_hash = ?
            "#,
        )
        .bind(&key_hash)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|r| r.into_record().into_account()))
    }

    // --- Settings ---

    pub async fn get_retention_days(&self) -> anyhow::Result<Option<i64>> {
        let row: Option<(String,)> =
            sqlx::query_as("SELECT value FROM settings WHERE key = 'retention_days'")
                .fetch_optional(&self.pool)
                .await?;
        Ok(row.and_then(|(v,)| v.parse().ok()))
    }

    /// Whether retention has ever been saved. Distinguishes "never set" from "forever",
    /// which `get_retention_days` both report as `None`.
    pub async fn has_retention_setting(&self) -> anyhow::Result<bool> {
        let row: Option<(String,)> =
            sqlx::query_as("SELECT value FROM settings WHERE key = 'retention_days'")
                .fetch_optional(&self.pool)
                .await?;
        Ok(row.is_some())
    }

    pub async fn set_retention_days(&self, days: Option<i64>) -> anyhow::Result<()> {
        let value = match days {
            Some(d) => d.to_string(),
            None => "forever".to_string(),
        };
        sqlx::query(
            "INSERT INTO settings (key, value) VALUES ('retention_days', ?)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        )
        .bind(value)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn get_settings(&self) -> anyhow::Result<AppSettings> {
        Ok(AppSettings {
            retention_days: self.get_retention_days().await?,
        })
    }

    // --- Saved filters ---

    pub async fn list_saved_filters(&self) -> anyhow::Result<Vec<SavedFilter>> {
        let rows = sqlx::query_as::<_, SavedFilterRow>(
            "SELECT id, name, query, created_at FROM saved_filters ORDER BY name",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(Into::into).collect())
    }

    pub async fn create_saved_filter(&self, name: &str, query: &str) -> anyhow::Result<SavedFilter> {
        let id = Uuid::new_v4().to_string();
        let created_at = Utc::now();
        sqlx::query(
            "INSERT INTO saved_filters (id, name, query, created_at) VALUES (?, ?, ?, ?)",
        )
        .bind(&id)
        .bind(name)
        .bind(query)
        .bind(created_at.to_rfc3339())
        .execute(&self.pool)
        .await?;
        Ok(SavedFilter {
            id,
            name: name.to_string(),
            query: query.to_string(),
            created_at,
        })
    }

    pub async fn delete_saved_filter(&self, id: &str) -> anyhow::Result<()> {
        sqlx::query("DELETE FROM saved_filters WHERE id = ?")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    // --- Dashboards ---

    pub async fn list_dashboards(&self) -> anyhow::Result<Vec<Dashboard>> {
        let rows = sqlx::query_as::<_, DashboardRow>(
            "SELECT id, name, created_at FROM dashboards ORDER BY name",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(Into::into).collect())
    }

    pub async fn create_dashboard(&self, name: &str) -> anyhow::Result<Dashboard> {
        let id = Uuid::new_v4().to_string();
        let created_at = Utc::now();
        sqlx::query("INSERT INTO dashboards (id, name, created_at) VALUES (?, ?, ?)")
            .bind(&id)
            .bind(name)
            .bind(created_at.to_rfc3339())
            .execute(&self.pool)
            .await?;
        Ok(Dashboard {
            id,
            name: name.to_string(),
            created_at,
        })
    }

    pub async fn count_dashboards(&self) -> anyhow::Result<i64> {
        let (count,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM dashboards")
            .fetch_one(&self.pool)
            .await?;
        Ok(count)
    }

    pub async fn get_dashboard(&self, id: &str) -> anyhow::Result<Option<Dashboard>> {
        let row = sqlx::query_as::<_, DashboardRow>(
            "SELECT id, name, created_at FROM dashboards WHERE id = ?",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(Into::into))
    }

    pub async fn delete_dashboard(&self, id: &str) -> anyhow::Result<()> {
        sqlx::query("DELETE FROM dashboard_widgets WHERE dashboard_id = ?")
            .bind(id)
            .execute(&self.pool)
            .await?;
        sqlx::query("DELETE FROM dashboards WHERE id = ?")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn rename_dashboard(&self, id: &str, name: &str) -> anyhow::Result<()> {
        sqlx::query("UPDATE dashboards SET name = ? WHERE id = ?")
            .bind(name)
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn list_widgets(&self, dashboard_id: &str) -> anyhow::Result<Vec<DashboardWidget>> {
        let rows = sqlx::query_as::<_, WidgetRow>(
            r#"SELECT id, dashboard_id, title, query, visualization, pos_x, pos_y, pos_w, pos_h
               FROM dashboard_widgets WHERE dashboard_id = ? ORDER BY pos_y, pos_x"#,
        )
        .bind(dashboard_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().filter_map(|r| r.into_widget()).collect())
    }

    pub async fn create_widget(
        &self,
        dashboard_id: &str,
        title: &str,
        query: &str,
        visualization: Visualization,
        position: WidgetPosition,
    ) -> anyhow::Result<DashboardWidget> {
        let id = Uuid::new_v4().to_string();
        sqlx::query(
            r#"INSERT INTO dashboard_widgets
               (id, dashboard_id, title, query, visualization, pos_x, pos_y, pos_w, pos_h)
               VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)"#,
        )
        .bind(&id)
        .bind(dashboard_id)
        .bind(title)
        .bind(query)
        .bind(visualization.as_str())
        .bind(position.x)
        .bind(position.y)
        .bind(position.w)
        .bind(position.h)
        .execute(&self.pool)
        .await?;
        Ok(DashboardWidget {
            id,
            dashboard_id: dashboard_id.to_string(),
            title: title.to_string(),
            query: query.to_string(),
            visualization,
            position,
        })
    }

    pub async fn update_widget(
        &self,
        id: &str,
        title: &str,
        query: &str,
        visualization: Visualization,
        position: WidgetPosition,
    ) -> anyhow::Result<()> {
        sqlx::query(
            r#"UPDATE dashboard_widgets
               SET title = ?, query = ?, visualization = ?, pos_x = ?, pos_y = ?, pos_w = ?, pos_h = ?
               WHERE id = ?"#,
        )
        .bind(title)
        .bind(query)
        .bind(visualization.as_str())
        .bind(position.x)
        .bind(position.y)
        .bind(position.w)
        .bind(position.h)
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn delete_widget(&self, id: &str) -> anyhow::Result<()> {
        sqlx::query("DELETE FROM dashboard_widgets WHERE id = ?")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    // --- Alerts ---

    pub async fn list_alert_views(&self) -> anyhow::Result<Vec<AlertView>> {
        let rows = sqlx::query_as::<_, AlertJoinRow>(
            r#"
            SELECT r.id, r.name, r.query, r.operator, r.threshold, r.enabled, r.created_at,
                   s.status, s.value, s.message, s.last_evaluated_at, s.last_changed_at
            FROM alert_rules r
            LEFT JOIN alert_states s ON s.rule_id = r.id
            ORDER BY r.name
            "#,
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().filter_map(|r| r.into_view()).collect())
    }

    pub async fn list_enabled_alert_rules(&self) -> anyhow::Result<Vec<AlertRule>> {
        let rows = sqlx::query_as::<_, AlertRuleRow>(
            r#"SELECT id, name, query, operator, threshold, enabled, created_at
               FROM alert_rules WHERE enabled = 1 ORDER BY name"#,
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().filter_map(|r| r.into_rule()).collect())
    }

    pub async fn create_alert_rule(
        &self,
        name: &str,
        query: &str,
        operator: AlertOperator,
        threshold: f64,
    ) -> anyhow::Result<AlertRule> {
        let id = Uuid::new_v4().to_string();
        let created_at = Utc::now();
        sqlx::query(
            r#"INSERT INTO alert_rules (id, name, query, operator, threshold, enabled, created_at)
               VALUES (?, ?, ?, ?, ?, 1, ?)"#,
        )
        .bind(&id)
        .bind(name)
        .bind(query)
        .bind(operator.as_str())
        .bind(threshold)
        .bind(created_at.to_rfc3339())
        .execute(&self.pool)
        .await?;
        sqlx::query(
            r#"INSERT INTO alert_states (rule_id, status, value, message, last_evaluated_at, last_changed_at)
               VALUES (?, 'ok', NULL, NULL, NULL, ?)"#,
        )
        .bind(&id)
        .bind(created_at.to_rfc3339())
        .execute(&self.pool)
        .await?;
        Ok(AlertRule {
            id,
            name: name.to_string(),
            query: query.to_string(),
            operator,
            threshold,
            enabled: true,
            created_at,
        })
    }

    pub async fn delete_alert_rule(&self, id: &str) -> anyhow::Result<()> {
        sqlx::query("DELETE FROM alert_states WHERE rule_id = ?")
            .bind(id)
            .execute(&self.pool)
            .await?;
        sqlx::query("DELETE FROM alert_rules WHERE id = ?")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn set_alert_enabled(&self, id: &str, enabled: bool) -> anyhow::Result<()> {
        sqlx::query("UPDATE alert_rules SET enabled = ? WHERE id = ?")
            .bind(if enabled { 1 } else { 0 })
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn upsert_alert_state(
        &self,
        rule_id: &str,
        status: AlertStatus,
        value: Option<f64>,
        message: Option<&str>,
        changed: bool,
    ) -> anyhow::Result<()> {
        let now = Utc::now().to_rfc3339();
        if changed {
            sqlx::query(
                r#"INSERT INTO alert_states (rule_id, status, value, message, last_evaluated_at, last_changed_at)
                   VALUES (?, ?, ?, ?, ?, ?)
                   ON CONFLICT(rule_id) DO UPDATE SET
                     status = excluded.status,
                     value = excluded.value,
                     message = excluded.message,
                     last_evaluated_at = excluded.last_evaluated_at,
                     last_changed_at = excluded.last_changed_at"#,
            )
            .bind(rule_id)
            .bind(status.as_str())
            .bind(value)
            .bind(message)
            .bind(&now)
            .bind(&now)
            .execute(&self.pool)
            .await?;
        } else {
            sqlx::query(
                r#"INSERT INTO alert_states (rule_id, status, value, message, last_evaluated_at, last_changed_at)
                   VALUES (?, ?, ?, ?, ?, ?)
                   ON CONFLICT(rule_id) DO UPDATE SET
                     status = excluded.status,
                     value = excluded.value,
                     message = excluded.message,
                     last_evaluated_at = excluded.last_evaluated_at"#,
            )
            .bind(rule_id)
            .bind(status.as_str())
            .bind(value)
            .bind(message)
            .bind(&now)
            .bind(&now)
            .execute(&self.pool)
            .await?;
        }
        Ok(())
    }

    pub async fn get_alert_status(&self, rule_id: &str) -> anyhow::Result<AlertStatus> {
        let row: Option<(String,)> =
            sqlx::query_as("SELECT status FROM alert_states WHERE rule_id = ?")
                .bind(rule_id)
                .fetch_optional(&self.pool)
                .await?;
        Ok(row
            .and_then(|(s,)| AlertStatus::parse(&s))
            .unwrap_or(AlertStatus::Ok))
    }

    pub async fn count_alert_rules(&self) -> anyhow::Result<i64> {
        let (count,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM alert_rules")
            .fetch_one(&self.pool)
            .await?;
        Ok(count)
    }
}

fn hash_password(password: &str) -> String {
    let salt = Uuid::new_v4().simple().to_string();
    let digest = Sha256::digest(format!("{salt}:{password}").as_bytes());
    format!("{salt}:{}", hex_encode(&digest))
}

fn verify_password(password: &str, stored: &str) -> bool {
    let Some((salt, expected)) = stored.split_once(':') else {
        return false;
    };
    let digest = Sha256::digest(format!("{salt}:{password}").as_bytes());
    hex_encode(&digest) == expected
}

fn hash_api_key(raw_key: &str) -> String {
    hex_encode(&Sha256::digest(raw_key.as_bytes()))
}

fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn parse_dt(s: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|d| d.with_timezone(&Utc))
}

#[derive(FromRow)]
struct AccountRow {
    id: String,
    username: String,
    password_hash: String,
    display_name: String,
    created_at: String,
}

impl AccountRow {
    fn into_record(self) -> AccountRecord {
        AccountRecord {
            id: self.id,
            username: self.username,
            password_hash: self.password_hash,
            display_name: self.display_name,
            created_at: parse_dt(&self.created_at).unwrap_or_else(Utc::now),
        }
    }
}

#[derive(FromRow)]
struct SessionJoinRow {
    id: String,
    username: String,
    #[allow(dead_code)]
    password_hash: String,
    display_name: String,
    created_at: String,
    expires_at: String,
}

#[derive(FromRow)]
struct ApiKeyRow {
    id: String,
    account_id: String,
    name: String,
    key_prefix: String,
    created_at: String,
}

impl From<ApiKeyRow> for ApiKeyInfo {
    fn from(r: ApiKeyRow) -> Self {
        Self {
            id: r.id,
            account_id: r.account_id,
            name: r.name,
            key_prefix: r.key_prefix,
            created_at: parse_dt(&r.created_at).unwrap_or_else(Utc::now),
        }
    }
}

#[derive(FromRow)]
struct SavedFilterRow {
    id: String,
    name: String,
    query: String,
    created_at: String,
}

impl From<SavedFilterRow> for SavedFilter {
    fn from(r: SavedFilterRow) -> Self {
        Self {
            id: r.id,
            name: r.name,
            query: r.query,
            created_at: DateTime::parse_from_rfc3339(&r.created_at)
                .map(|d| d.with_timezone(&Utc))
                .unwrap_or_else(|_| Utc::now()),
        }
    }
}

#[derive(FromRow)]
struct DashboardRow {
    id: String,
    name: String,
    created_at: String,
}

impl From<DashboardRow> for Dashboard {
    fn from(r: DashboardRow) -> Self {
        Self {
            id: r.id,
            name: r.name,
            created_at: DateTime::parse_from_rfc3339(&r.created_at)
                .map(|d| d.with_timezone(&Utc))
                .unwrap_or_else(|_| Utc::now()),
        }
    }
}

#[derive(FromRow)]
struct WidgetRow {
    id: String,
    dashboard_id: String,
    title: String,
    query: String,
    visualization: String,
    pos_x: i32,
    pos_y: i32,
    pos_w: i32,
    pos_h: i32,
}

impl WidgetRow {
    fn into_widget(self) -> Option<DashboardWidget> {
        Some(DashboardWidget {
            id: self.id,
            dashboard_id: self.dashboard_id,
            title: self.title,
            query: self.query,
            visualization: Visualization::parse(&self.visualization)?,
            position: WidgetPosition {
                x: self.pos_x,
                y: self.pos_y,
                w: self.pos_w,
                h: self.pos_h,
            },
        })
    }
}

#[derive(FromRow)]
struct AlertRuleRow {
    id: String,
    name: String,
    query: String,
    operator: String,
    threshold: f64,
    enabled: i64,
    created_at: String,
}

impl AlertRuleRow {
    fn into_rule(self) -> Option<AlertRule> {
        Some(AlertRule {
            id: self.id,
            name: self.name,
            query: self.query,
            operator: AlertOperator::parse(&self.operator)?,
            threshold: self.threshold,
            enabled: self.enabled != 0,
            created_at: DateTime::parse_from_rfc3339(&self.created_at)
                .map(|d| d.with_timezone(&Utc))
                .unwrap_or_else(|_| Utc::now()),
        })
    }
}

#[derive(FromRow)]
struct AlertJoinRow {
    id: String,
    name: String,
    query: String,
    operator: String,
    threshold: f64,
    enabled: i64,
    created_at: String,
    status: Option<String>,
    value: Option<f64>,
    message: Option<String>,
    last_evaluated_at: Option<String>,
    last_changed_at: Option<String>,
}

impl AlertJoinRow {
    fn into_view(self) -> Option<AlertView> {
        let parse_dt = |s: Option<String>| {
            s.and_then(|v| {
                DateTime::parse_from_rfc3339(&v)
                    .ok()
                    .map(|d| d.with_timezone(&Utc))
            })
        };
        Some(AlertView {
            rule: AlertRule {
                id: self.id.clone(),
                name: self.name,
                query: self.query,
                operator: AlertOperator::parse(&self.operator)?,
                threshold: self.threshold,
                enabled: self.enabled != 0,
                created_at: DateTime::parse_from_rfc3339(&self.created_at)
                    .map(|d| d.with_timezone(&Utc))
                    .unwrap_or_else(|_| Utc::now()),
            },
            status: self
                .status
                .as_deref()
                .and_then(AlertStatus::parse)
                .unwrap_or(AlertStatus::Ok),
            value: self.value,
            message: self.message,
            last_evaluated_at: parse_dt(self.last_evaluated_at),
            last_changed_at: parse_dt(self.last_changed_at),
        })
    }
}
