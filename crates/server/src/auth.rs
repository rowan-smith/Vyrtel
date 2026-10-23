//! Authentication: API keys for ingestion, admin sessions for the UI.
//!
//! * API keys are random 32-byte tokens shown once; only their SHA-256 is
//!   stored (keys are high-entropy, so a fast hash is appropriate).
//! * The admin password is hashed with Argon2id.
//! * Sessions are random tokens in an HttpOnly, SameSite=Lax cookie; only
//!   their SHA-256 is stored.
//!
//! The UI and API always need a session or an admin key. Ingestion needs an
//! API key while at least one ingest-scoped key exists, and is open while
//! none does (see [`ApiKeys`]). The boundaries are kept small so OIDC etc.
//! can be added later as another way of producing a [`Principal`].

use std::collections::HashMap;
use std::time::{Duration, Instant};

use argon2::Argon2;
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use axum::extract::{Request, State};
use axum::http::{HeaderMap, header};
use axum::middleware::Next;
use axum::response::Response;
use metadata::Metadata;
use rand::RngCore;
use sha2::{Digest, Sha256};

use crate::error::ApiError;
use crate::state::{AppState, SharedState};

pub const SESSION_COOKIE: &str = "vyrtel_session";

#[derive(Debug, Clone)]
pub enum Principal {
    User(String),
    ApiKey { id: i64, scope: String },
}

pub fn sha256_hex(s: &str) -> String {
    hex::encode(Sha256::digest(s.as_bytes()))
}

pub fn random_token(bytes: usize) -> String {
    let mut b = vec![0u8; bytes];
    rand::thread_rng().fill_bytes(&mut b);
    hex::encode(b)
}

/// Generate an API key: returns (plaintext, display prefix, hash).
pub fn new_api_key() -> (String, String, String) {
    let key = format!("vyr_{}", random_token(32));
    let prefix = key[..12].to_string();
    let hash = sha256_hex(&key);
    (key, prefix, hash)
}

pub fn hash_password(password: &str) -> Result<String, String> {
    let salt = SaltString::generate(&mut argon2::password_hash::rand_core::OsRng);
    Argon2::default().hash_password(password.as_bytes(), &salt).map(|h| h.to_string()).map_err(|e| e.to_string())
}

pub fn verify_password(password: &str, hash: &str) -> bool {
    PasswordHash::new(hash).map(|h| Argon2::default().verify_password(password.as_bytes(), &h).is_ok()).unwrap_or(false)
}

/// `Authorization: Bearer <key>` or `X-Api-Key: <key>`.
pub fn api_key_from(headers: &HeaderMap) -> Option<String> {
    if let Some(v) = headers.get(header::AUTHORIZATION).and_then(|v| v.to_str().ok()) {
        let v = v.trim();
        if let Some(k) = v.strip_prefix("Bearer ").or_else(|| v.strip_prefix("bearer ")) {
            return Some(k.trim().to_string());
        }
    }
    headers.get("x-api-key").and_then(|v| v.to_str().ok()).map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

pub fn session_from(headers: &HeaderMap) -> Option<String> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|c| c.trim().split_once('='))
        .find(|(k, _)| *k == SESSION_COOKIE)
        .map(|(_, v)| v.to_string())
        .filter(|v| !v.is_empty())
}

/// Every active API key, by SHA-256 of the key with its id and scope.
/// Loaded at startup and changed only by [`create_api_key`] and
/// [`revoke_api_key`], so authenticating a request never reads SQLite.
/// Ingestion is protected while it holds an ingest-scoped key; admin keys
/// do not count.
#[derive(Default)]
pub struct ApiKeys {
    by_hash: HashMap<String, (i64, String)>,
    ingest: usize,
}

impl ApiKeys {
    pub fn load(m: &Metadata) -> metadata::Result<Self> {
        let mut keys = Self::default();
        for (id, hash, scope) in m.active_api_keys()? {
            keys.add(hash, id, scope);
        }
        Ok(keys)
    }

    /// Whether ingestion needs an API key.
    pub fn ingest_protected(&self) -> bool {
        self.ingest > 0
    }

    fn add(&mut self, hash: String, id: i64, scope: String) {
        self.ingest += usize::from(scope == "ingest");
        self.by_hash.insert(hash, (id, scope));
    }

    fn remove(&mut self, id: i64) {
        let Self { by_hash, ingest } = self;
        by_hash.retain(|_, (key_id, scope)| {
            let revoked = *key_id == id;
            if revoked && scope == "ingest" {
                *ingest -= 1;
            }
            !revoked
        });
    }
}

/// Store a new key and accept it; an ingest key also protects ingestion
/// from here on. Returns the record and the plaintext key. Blocking.
///
/// The registry stays locked across the SQLite write, so it never holds a
/// key that is revoked in SQLite, even if the revocation races the
/// creation. Requests wait for that write only while a key is created or
/// revoked.
pub fn create_api_key(state: &AppState, name: &str, scope: &str) -> metadata::Result<(metadata::ApiKey, String)> {
    let (key, prefix, hash) = new_api_key();
    let mut keys = state.api_keys.write().unwrap();
    let rec = state.metadata.create_api_key(name, &prefix, &hash, scope)?;
    keys.add(hash, rec.id, rec.scope.clone());
    Ok((rec, key))
}

/// Revoke a key; revoking the last ingest key opens ingestion. Blocking.
pub fn revoke_api_key(state: &AppState, id: i64) -> metadata::Result<()> {
    let mut keys = state.api_keys.write().unwrap();
    state.metadata.revoke_api_key(id)?;
    keys.remove(id);
    Ok(())
}

fn check_api_key(state: &AppState, key: &str) -> Result<Principal, ApiError> {
    let found = state.api_keys.read().unwrap().by_hash.get(&sha256_hex(key)).cloned();
    let Some((id, scope)) = found else {
        return Err(ApiError::unauthorized("invalid API key"));
    };
    // Persist last-used at most once a minute per key, so ingestion does
    // not turn into a stream of SQLite writes.
    let touch = {
        let mut t = state.key_touch.lock().unwrap();
        let due = t.get(&id).is_none_or(|last| last.elapsed() > Duration::from_secs(60));
        if due {
            t.insert(id, Instant::now());
        }
        due
    };
    if touch {
        let m = state.metadata.clone();
        tokio::task::spawn_blocking(move || {
            let _ = m.touch_api_key(id);
        });
    }
    Ok(Principal::ApiKey { id, scope })
}

async fn check_session(state: &SharedState, token: String) -> Result<Option<Principal>, ApiError> {
    let hash = sha256_hex(&token);
    let user = state.db(move |m| m.find_session(&hash)).await?;
    Ok(user.map(|u| Principal::User(u.username)))
}

/// Ingestion endpoints: open while no ingest-scoped key exists, otherwise
/// any valid API key.
pub async fn require_ingest(
    State(state): State<SharedState>,
    mut req: Request,
    next: Next,
) -> Result<Response, ApiError> {
    if !state.api_keys.read().unwrap().ingest_protected() {
        return Ok(next.run(req).await);
    }
    let Some(key) = api_key_from(req.headers()) else {
        return Err(ApiError::unauthorized(
            "missing API key; send 'Authorization: Bearer <key>' or 'X-Api-Key: <key>'",
        ));
    };
    let p = check_api_key(&state, &key)?;
    req.extensions_mut().insert(p);
    Ok(next.run(req).await)
}

/// Everything else under /api/v1: an admin session or an admin-scoped key.
pub async fn require_user(
    State(state): State<SharedState>,
    mut req: Request,
    next: Next,
) -> Result<Response, ApiError> {
    if let Some(token) = session_from(req.headers())
        && let Some(p) = check_session(&state, token).await?
    {
        req.extensions_mut().insert(p);
        return Ok(next.run(req).await);
    }
    if let Some(key) = api_key_from(req.headers()) {
        let p = check_api_key(&state, &key)?;
        if let Principal::ApiKey { scope, .. } = &p
            && scope != "admin"
        {
            return Err(ApiError::forbidden("this API key may only ingest data"));
        }
        req.extensions_mut().insert(p);
        return Ok(next.run(req).await);
    }
    Err(ApiError::unauthorized("authentication required"))
}

/// Create/refresh the admin user at startup. Returns a generated password
/// when none was configured and no user exists yet.
pub fn bootstrap_admin(state: &SharedState) -> anyhow::Result<Option<String>> {
    let auth = &state.config.auth;
    let m = &state.metadata;
    if let Some(pw) = &auth.admin_password {
        let existing = m.find_user(&auth.admin_username)?;
        // Only re-hash when the configured password changed.
        if !existing.is_some_and(|u| verify_password(pw, &u.password_hash)) {
            m.upsert_user(&auth.admin_username, &hash_password(pw).map_err(anyhow::Error::msg)?)?;
        }
        return Ok(None);
    }
    if m.count_users()? == 0 {
        let pw = random_token(12);
        m.upsert_user(&auth.admin_username, &hash_password(&pw).map_err(anyhow::Error::msg)?)?;
        return Ok(Some(pw));
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    #[test]
    fn passwords() {
        let h = hash_password("hunter2").unwrap();
        assert!(h.starts_with("$argon2id$"));
        assert!(verify_password("hunter2", &h));
        assert!(!verify_password("hunter3", &h));
        assert!(!verify_password("x", "not a hash"));
    }

    #[test]
    fn api_keys() {
        let (key, prefix, hash) = new_api_key();
        assert!(key.starts_with("vyr_") && key.len() == 68);
        assert!(key.starts_with(&prefix));
        assert_eq!(hash, sha256_hex(&key));
        assert_ne!(new_api_key().0, key);
    }

    #[test]
    fn header_parsing() {
        let mut h = HeaderMap::new();
        h.insert(header::AUTHORIZATION, HeaderValue::from_static("Bearer abc"));
        assert_eq!(api_key_from(&h).as_deref(), Some("abc"));
        let mut h = HeaderMap::new();
        h.insert("x-api-key", HeaderValue::from_static(" k1 "));
        assert_eq!(api_key_from(&h).as_deref(), Some("k1"));
        let mut h = HeaderMap::new();
        h.insert(header::AUTHORIZATION, HeaderValue::from_static("Basic Zm9v"));
        assert_eq!(api_key_from(&h), None);
        let mut h = HeaderMap::new();
        h.insert(header::COOKIE, HeaderValue::from_static("a=1; vyrtel_session=tok; b=2"));
        assert_eq!(session_from(&h).as_deref(), Some("tok"));
    }

    #[test]
    fn only_ingest_keys_protect_ingestion() {
        let mut k = ApiKeys::default();
        k.add("a".into(), 1, "admin".into());
        assert!(!k.ingest_protected());
        k.add("i1".into(), 2, "ingest".into());
        k.add("i2".into(), 3, "ingest".into());
        assert!(k.ingest_protected());
        k.remove(2);
        assert!(k.ingest_protected());
        k.remove(1);
        k.remove(99);
        assert!(k.ingest_protected());
        k.remove(3);
        assert!(!k.ingest_protected());
        assert!(k.by_hash.is_empty());
    }
}
