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
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use argon2::password_hash::phc::PasswordHash;
use argon2::{Argon2, PasswordHasher, PasswordVerifier};
use axum::extract::{Request, State};
use axum::http::{HeaderMap, header};
use axum::middleware::Next;
use axum::response::Response;
use metadata::Metadata;
use rand::Rng;
use sha2::{Digest, Sha256};
use tokio::sync::{Mutex, Semaphore};

use crate::error::{ApiError, ApiResult};
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
    rand::rng().fill_bytes(&mut b);
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
    PasswordHasher::<PasswordHash>::hash_password(&Argon2::default(), password.as_bytes())
        .map(|h| h.to_string())
        .map_err(|e| e.to_string())
}

pub fn verify_password(password: &str, hash: &str) -> bool {
    Argon2::default().verify_password(password.as_bytes(), hash).is_ok()
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

/// Most API key changes waiting for or running their SQLite write at once.
/// Further changes are refused with 503 instead of queueing without bound.
pub const MAX_PENDING_KEY_CHANGES: usize = 16;

/// The active API keys at one moment, by SHA-256 of the key with its id
/// and scope. Ingestion is protected while it holds an ingest-scoped key;
/// admin keys do not count. Never changed once published: a change builds
/// the next set (see [`ApiKeys`]).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct KeySet {
    by_hash: HashMap<String, (i64, String)>,
    ingest: usize,
}

impl KeySet {
    /// Whether ingestion needs an API key.
    pub fn ingest_protected(&self) -> bool {
        self.ingest > 0
    }

    /// The `(id, scope)` of an active key.
    fn find(&self, key: &str) -> Result<(i64, String), ApiError> {
        self.by_hash.get(&sha256_hex(key)).cloned().ok_or_else(|| ApiError::unauthorized("invalid API key"))
    }

    /// Whether an ingest request with `headers` is admitted: `None` while
    /// ingestion is open, else the key's `(id, scope)`. Policy and key come
    /// from this one set, so a key change between them cannot mix two.
    fn admit_ingest(&self, headers: &HeaderMap) -> Result<Option<(i64, String)>, ApiError> {
        if !self.ingest_protected() {
            return Ok(None);
        }
        let Some(key) = api_key_from(headers) else {
            return Err(ApiError::unauthorized(
                "missing API key; send 'Authorization: Bearer <key>' or 'X-Api-Key: <key>'",
            ));
        };
        self.find(&key).map(Some)
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

/// The API key registry. Loaded at startup and changed only through
/// [`ApiKeys::change`], so authenticating a request by API key never reads
/// SQLite. (Sessions are cached, but a miss reads SQLite: see [`Sessions`].)
///
/// Requests read the current [`KeySet`] by cloning an `Arc` under a read
/// lock that is only ever held for a pointer copy or swap, so they never
/// wait for SQLite. Changes take turns on an async mutex (at most
/// [`MAX_PENDING_KEY_CHANGES`] waiting or running), write SQLite on the
/// blocking pool, and only after the write commits publish the next set.
/// Lock order: `changes`, then `current`; `current` is never held across
/// anything else.
pub struct ApiKeys {
    current: RwLock<Arc<KeySet>>,
    changes: Arc<Mutex<()>>,
    pending: Arc<Semaphore>,
}

impl ApiKeys {
    pub fn new(keys: KeySet) -> Self {
        Self {
            current: RwLock::new(Arc::new(keys)),
            changes: Arc::new(Mutex::new(())),
            pending: Arc::new(Semaphore::new(MAX_PENDING_KEY_CHANGES)),
        }
    }

    pub fn load(m: &Metadata) -> metadata::Result<Self> {
        let mut keys = KeySet::default();
        for (id, hash, scope) in m.active_api_keys()? {
            keys.add(hash, id, scope);
        }
        Ok(Self::new(keys))
    }

    /// The current keys. Hold the result only as long as one decision
    /// needs it, so superseded sets are freed.
    pub fn snapshot(&self) -> Arc<KeySet> {
        self.current.read().unwrap().clone()
    }

    /// Whether ingestion needs an API key.
    pub fn ingest_protected(&self) -> bool {
        self.snapshot().ingest_protected()
    }

    /// Run `write` (the SQLite change) on the blocking pool, then publish
    /// the current set changed by `apply`. Changes run one at a time, in
    /// the order they get their turn.
    ///
    /// A change is published only after its write commits and before this
    /// returns, so a request that starts after it returns sees it. A failed
    /// (or panicking) write publishes nothing. Dropping this future while
    /// it waits for its turn cancels the change; once the write has started
    /// it runs to the end and is published, and keeps its turn until then,
    /// so a later change can never be overtaken by it. Requests admitted
    /// before a change are not affected by it.
    pub async fn change<T, W, A>(self: &Arc<Self>, write: W, apply: A) -> ApiResult<T>
    where
        T: Send + 'static,
        W: FnOnce() -> metadata::Result<T> + Send + 'static,
        A: FnOnce(&mut KeySet, &T) + Send + 'static,
    {
        let Ok(queued) = self.pending.clone().try_acquire_owned() else {
            return Err(ApiError::unavailable("too many API key changes in progress; try again shortly"));
        };
        let turn = self.changes.clone().lock_owned().await;
        let keys = self.clone();
        tokio::task::spawn_blocking(move || {
            let _held = (queued, turn);
            let out = write()?;
            let mut next = KeySet::clone(&keys.snapshot());
            apply(&mut next, &out);
            *keys.current.write().unwrap() = Arc::new(next);
            Ok(out)
        })
        .await?
    }

    /// See [`create_api_key`].
    pub async fn create(
        self: &Arc<Self>,
        m: &Arc<Metadata>,
        name: &str,
        scope: &str,
    ) -> ApiResult<(metadata::ApiKey, String)> {
        let (key, prefix, hash) = new_api_key();
        let (m, name, scope, stored) = (m.clone(), name.to_string(), scope.to_string(), hash.clone());
        let write = move || m.create_api_key(&name, &prefix, &stored, &scope);
        let rec = self.change(write, |keys, rec| keys.add(hash, rec.id, rec.scope.clone())).await?;
        Ok((rec, key))
    }

    /// See [`revoke_api_key`].
    pub async fn revoke(self: &Arc<Self>, m: &Arc<Metadata>, id: i64) -> ApiResult<()> {
        let m = m.clone();
        self.change(move || m.revoke_api_key(id), move |keys, ()| keys.remove(id)).await
    }
}

/// Store a new key and accept it; an ingest key also protects ingestion
/// from here on. Returns the record and the plaintext key.
pub async fn create_api_key(state: &AppState, name: &str, scope: &str) -> ApiResult<(metadata::ApiKey, String)> {
    state.api_keys.create(&state.metadata, name, scope).await
}

/// Revoke a key; revoking the last ingest key opens ingestion.
pub async fn revoke_api_key(state: &AppState, id: i64) -> ApiResult<()> {
    state.api_keys.revoke(&state.metadata, id).await
}

/// Note that key `id` was used: persist last-used at most once a minute per
/// key, so ingestion does not turn into a stream of SQLite writes.
fn touch_api_key(state: &AppState, id: i64) {
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
}

fn check_api_key(state: &AppState, key: &str) -> Result<Principal, ApiError> {
    let (id, scope) = state.api_keys.snapshot().find(key)?;
    touch_api_key(state, id);
    Ok(Principal::ApiKey { id, scope })
}

/// Most sessions [`Sessions`] keeps. An entry is the SHA-256 (hex) of the
/// token, the user name and the expiry, about 200 bytes with the map's
/// overhead, so the cache stays under about 1 MB.
pub const SESSION_CACHE_ENTRIES: usize = 4096;

/// Most session lookups reading SQLite at once. Further lookups wait for a
/// turn without holding a thread; how many can wait is bounded by the
/// connection limit (`server.max_connections`).
pub const MAX_SESSION_FILLS: usize = 16;

/// Where request authentication by session cookie spends its time, counted
/// since the process started and reported by `/api/v1/system/info`
/// (AUTH-PERF-02). Each SQLite lookup is split into its wait for a turn and
/// a blocking thread (`queue`), its wait for the metadata connection
/// (`lock`, held by any other metadata read or write) and the lookup
/// itself (`query`). Only atomics.
#[derive(Debug, Default)]
struct SessionLookups {
    hits: AtomicU64,
    lookups: AtomicU64,
    found: AtomicU64,
    failed: AtomicU64,
    in_flight: AtomicU64,
    peak_in_flight: AtomicU64,
    queue_us: AtomicU64,
    lock_us: AtomicU64,
    query_us: AtomicU64,
    max_queue_us: AtomicU64,
    max_lock_us: AtomicU64,
    max_query_us: AtomicU64,
}

/// One lookup in flight; leaving (even by cancellation) ends it.
struct InFlight<'a>(&'a SessionLookups);

impl Drop for InFlight<'_> {
    fn drop(&mut self) {
        self.0.in_flight.fetch_sub(1, Ordering::Relaxed);
    }
}

impl SessionLookups {
    fn begin(&self) -> InFlight<'_> {
        let now = self.in_flight.fetch_add(1, Ordering::Relaxed) + 1;
        self.peak_in_flight.fetch_max(now, Ordering::Relaxed);
        InFlight(self)
    }

    fn record(&self, found: Option<bool>, queue: Duration, lock: Duration, query: Duration) {
        let add = |total: &AtomicU64, max: &AtomicU64, d: Duration| {
            let us = d.as_micros().min(u64::MAX as u128) as u64;
            total.fetch_add(us, Ordering::Relaxed);
            max.fetch_max(us, Ordering::Relaxed);
        };
        self.lookups.fetch_add(1, Ordering::Relaxed);
        match found {
            Some(true) => self.found.fetch_add(1, Ordering::Relaxed),
            Some(false) => 0,
            None => self.failed.fetch_add(1, Ordering::Relaxed),
        };
        add(&self.queue_us, &self.max_queue_us, queue);
        add(&self.lock_us, &self.max_lock_us, lock);
        add(&self.query_us, &self.max_query_us, query);
    }
}

/// A cached session: never used at or after `expires_at` (ms since the
/// epoch), the same boundary as the SQLite lookup.
struct Cached {
    username: String,
    expires_at: i64,
}

#[derive(Default)]
struct Cache {
    by_hash: HashMap<String, Cached>,
    /// Advanced by every invalidation. A lookup caches its result only if
    /// no invalidation happened since before it read SQLite, so a lookup
    /// racing a logout can never put the ended session back.
    epoch: u64,
}

/// Sessions for request authentication (AUTH-PERF-02): a bounded cache of
/// valid sessions in front of the SQLite lookup, so repeat requests from
/// one session do not queue for the metadata connection.
///
/// * Keyed by the token's SHA-256, as stored; the token itself is never
///   kept.
/// * Only sessions found valid are cached (no negative entries), so unknown
///   tokens cannot grow it; at most [`SESSION_CACHE_ENTRIES`], evicting
///   expired entries first, then an arbitrary one.
/// * An entry is used only before its session's expiry.
/// * Logout deletes the session in SQLite, then [`Sessions::invalidate`]s
///   it before responding: from then on no request is admitted with it,
///   and no lookup that started earlier can cache it again.
/// * A miss reads SQLite as before, at most [`MAX_SESSION_FILLS`] at once;
///   a database error fails the request (`500`) and caches nothing.
/// * In memory only: a restart starts empty and reads SQLite again.
pub struct Sessions {
    cache: std::sync::Mutex<Cache>,
    fills: Arc<Semaphore>,
    stats: SessionLookups,
}

impl Default for Sessions {
    fn default() -> Self {
        Self {
            cache: Default::default(),
            fills: Arc::new(Semaphore::new(MAX_SESSION_FILLS)),
            stats: Default::default(),
        }
    }
}

impl Sessions {
    /// The user of an unexpired cached session.
    fn cached(&self, hash: &str, now: i64) -> Option<String> {
        let mut c = self.cache.lock().unwrap();
        match c.by_hash.get(hash) {
            Some(e) if e.expires_at > now => Some(e.username.clone()),
            Some(_) => {
                c.by_hash.remove(hash);
                None
            }
            None => None,
        }
    }

    fn epoch(&self) -> u64 {
        self.cache.lock().unwrap().epoch
    }

    /// Cache a session found valid by a lookup that began at `epoch`.
    fn fill(&self, hash: String, username: String, expires_at: i64, epoch: u64) {
        let mut c = self.cache.lock().unwrap();
        if c.epoch != epoch {
            return;
        }
        if c.by_hash.len() >= SESSION_CACHE_ENTRIES && !c.by_hash.contains_key(&hash) {
            let now = metadata::now_ms();
            c.by_hash.retain(|_, e| e.expires_at > now);
            if c.by_hash.len() >= SESSION_CACHE_ENTRIES
                && let Some(evict) = c.by_hash.keys().next().cloned()
            {
                c.by_hash.remove(&evict);
            }
        }
        c.by_hash.insert(hash, Cached { username, expires_at });
    }

    /// Forget session `hash` (by its token's SHA-256), and stop every lookup
    /// already under way from caching what it read.
    pub fn invalidate(&self, hash: &str) {
        let mut c = self.cache.lock().unwrap();
        c.epoch += 1;
        c.by_hash.remove(hash);
    }

    /// The user of session `hash`, from the cache or else from `lookup` (the
    /// SQLite read: the user and expiry if valid, and how long it waited for
    /// the connection), run on the blocking pool.
    pub async fn check<F>(&self, hash: String, lookup: F) -> ApiResult<Option<String>>
    where
        F: FnOnce(&str) -> metadata::Result<(Option<(String, i64)>, Duration)> + Send + 'static,
    {
        if let Some(user) = self.cached(&hash, metadata::now_ms()) {
            self.stats.hits.fetch_add(1, Ordering::Relaxed);
            return Ok(Some(user));
        }
        let _in_flight = self.stats.begin();
        let submitted = Instant::now();
        let Ok(turn) = self.fills.clone().acquire_owned().await else {
            return Err(ApiError::unavailable("shutting down"));
        };
        // Before the read: an invalidation from here on discards its result.
        let epoch = self.epoch();
        let key = hash.clone();
        let (r, queue, lock, query) = tokio::task::spawn_blocking(move || {
            let _turn = turn;
            let queue = submitted.elapsed();
            let started = Instant::now();
            let r = lookup(&key);
            let ran = started.elapsed();
            let lock = r.as_ref().map_or(Duration::ZERO, |(_, lock)| *lock);
            (r, queue, lock, ran.saturating_sub(lock))
        })
        .await?;
        self.stats.record(r.as_ref().ok().map(|(found, _)| found.is_some()), queue, lock, query);
        let (found, _) = r?;
        Ok(found.map(|(username, expires_at)| {
            self.fill(hash, username.clone(), expires_at, epoch);
            username
        }))
    }

    pub fn snapshot(&self) -> serde_json::Value {
        let s = &self.stats;
        let n = |c: &AtomicU64| c.load(Ordering::Relaxed);
        let cached = self.cache.lock().unwrap().by_hash.len();
        serde_json::json!({
            "cacheHits": n(&s.hits), "cached": cached, "cacheCapacity": SESSION_CACHE_ENTRIES,
            "lookups": n(&s.lookups), "found": n(&s.found), "failed": n(&s.failed),
            "inFlight": n(&s.in_flight), "peakInFlight": n(&s.peak_in_flight),
            "queueMicros": n(&s.queue_us), "lockMicros": n(&s.lock_us), "queryMicros": n(&s.query_us),
            "maxQueueMicros": n(&s.max_queue_us), "maxLockMicros": n(&s.max_lock_us),
            "maxQueryMicros": n(&s.max_query_us),
        })
    }
}

/// The session's user, from [`Sessions`] or SQLite. A lookup reads through
/// the metadata store's one connection, so it can wait behind any metadata
/// write, a key change's included.
async fn check_session(state: &SharedState, token: String) -> Result<Option<Principal>, ApiError> {
    let m = state.metadata.clone();
    let lookup = move |hash: &str| {
        let (found, lock) = m.find_session_timed(hash)?;
        Ok((found.map(|(user, expires_at)| (user.username, expires_at)), lock))
    };
    Ok(state.sessions.check(sha256_hex(&token), lookup).await?.map(Principal::User))
}

/// Ingestion endpoints: open while no ingest-scoped key exists, otherwise
/// any valid API key. Both are decided from one [`KeySet`].
pub async fn require_ingest(
    State(state): State<SharedState>,
    mut req: Request,
    next: Next,
) -> Result<Response, ApiError> {
    let admitted = state.api_keys.snapshot().admit_ingest(req.headers())?;
    if let Some((id, scope)) = admitted {
        touch_api_key(&state, id);
        req.extensions_mut().insert(Principal::ApiKey { id, scope });
    }
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
        let mut k = KeySet::default();
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

    // ---- AUTH-PERF-01: key changes off the request path -------------------

    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc;

    use metadata::MetadataError;
    use tokio::sync::oneshot;

    fn bearer(key: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert(header::AUTHORIZATION, HeaderValue::from_str(&format!("Bearer {key}")).unwrap());
        h
    }

    /// A key write that signals when it starts and returns `out` only once
    /// released (or once the release handle is dropped).
    fn gated<T: Send + 'static>(
        out: metadata::Result<T>,
    ) -> (impl FnOnce() -> metadata::Result<T> + Send + 'static, oneshot::Receiver<()>, mpsc::Sender<()>) {
        let (started_tx, started) = oneshot::channel();
        let (release, released) = mpsc::channel::<()>();
        let write = move || {
            let _ = started_tx.send(());
            let _ = released.recv();
            out
        };
        (write, started, release)
    }

    async fn until(what: &str, cond: impl Fn() -> bool) {
        tokio::time::timeout(Duration::from_secs(10), async {
            while !cond() {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("timed out waiting for {what}"));
    }

    fn in_memory() -> (Arc<Metadata>, Arc<ApiKeys>) {
        let m = Arc::new(Metadata::open_in_memory().unwrap());
        let keys = Arc::new(ApiKeys::load(&m).unwrap());
        (m, keys)
    }

    /// The published set equals what a restart would load.
    fn assert_matches_store(keys: &ApiKeys, m: &Metadata) {
        assert_eq!(*keys.snapshot(), *ApiKeys::load(m).unwrap().snapshot());
    }

    // AC1: while a key write is stalled, the runtime keeps running, requests
    // are decided from the published set without waiting, queued changes
    // hold no blocking thread, and changes beyond the bound are refused.
    #[test]
    fn slow_key_writes_block_neither_requests_nor_the_runtime() {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .max_blocking_threads(2)
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let keys = Arc::new(ApiKeys::new(KeySet::default()));
            let (write, started, release) = gated(Ok(()));
            let first = tokio::spawn({
                let keys = keys.clone();
                async move { keys.change(write, |k, ()| k.add(sha256_hex("vyr_first"), 1, "ingest".into())).await }
            });
            started.await.unwrap();
            let queued: Vec<_> = (2..=MAX_PENDING_KEY_CHANGES as i64)
                .map(|id| {
                    let keys = keys.clone();
                    tokio::spawn(async move {
                        keys.change(|| Ok(()), move |k, ()| k.add(format!("h{id}"), id, "admin".into())).await
                    })
                })
                .collect();
            until("every change to queue", || keys.pending.available_permits() == 0).await;

            let refused = keys.change(|| Ok(()), |_, ()| {}).await.unwrap_err();
            assert_eq!((refused.status.as_u16(), refused.code), (503, "unavailable"));
            let heartbeat = Instant::now();
            for _ in 0..20 {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
            assert!(heartbeat.elapsed() < Duration::from_secs(5), "the runtime kept ticking");
            // One blocking thread runs the stalled write; the other is free.
            tokio::time::timeout(Duration::from_secs(5), tokio::task::spawn_blocking(|| ())).await.unwrap().unwrap();
            let open = keys.snapshot();
            assert_eq!(open.admit_ingest(&HeaderMap::new()).unwrap(), None, "still open: nothing committed");
            assert!(!first.is_finished());

            release.send(()).unwrap();
            first.await.unwrap().unwrap();
            for q in queued {
                q.await.unwrap().unwrap();
            }
            let now = keys.snapshot();
            assert!(now.ingest_protected());
            assert_eq!(now.by_hash.len(), MAX_PENDING_KEY_CHANGES);
            assert!(!open.ingest_protected(), "a published set never changes");
            until("every turn to be returned", || keys.pending.available_permits() == MAX_PENDING_KEY_CHANGES).await;
        });
    }

    // AC2: a failed or panicking write publishes nothing and releases its
    // turn; a failed revoke keeps the key accepted.
    #[tokio::test]
    async fn failed_key_writes_publish_nothing() {
        let (m, keys) = in_memory();
        let before = keys.snapshot();
        assert_eq!(keys.create(&m, "ci", "root").await.unwrap_err().status.as_u16(), 400);
        let disk = || Err::<(), _>(MetadataError::Invalid("disk I/O error".into()));
        keys.change(disk, |k, ()| k.add("fictitious".into(), 1, "ingest".into())).await.unwrap_err();
        let panicked = keys
            .change(
                || -> metadata::Result<()> { panic!("injected key write panic") },
                |k, ()| k.add("fictitious".into(), 1, "ingest".into()),
            )
            .await
            .unwrap_err();
        assert_eq!(panicked.status.as_u16(), 500);
        assert!(Arc::ptr_eq(&before, &keys.snapshot()), "nothing published");
        assert!(!keys.ingest_protected(), "the failed first key did not change the policy");

        let (rec, key) = keys.create(&m, "ci", "ingest").await.unwrap();
        keys.change(disk, move |k, ()| k.remove(rec.id)).await.unwrap_err();
        assert_eq!(keys.snapshot().admit_ingest(&bearer(&key)).unwrap(), Some((rec.id, "ingest".into())));
        assert!(keys.snapshot().admit_ingest(&HeaderMap::new()).is_err(), "ingestion stays protected");
        assert_eq!(keys.revoke(&m, rec.id + 100).await.unwrap_err().status.as_u16(), 404);
        assert_matches_store(&keys, &m);

        keys.revoke(&m, rec.id).await.unwrap();
        assert!(!keys.ingest_protected());
        assert_eq!(keys.revoke(&m, rec.id).await.unwrap_err().status.as_u16(), 404, "revoked once");
        assert_matches_store(&keys, &m);
    }

    // AC2: a change whose caller goes away while it waits never runs; one
    // whose write has started commits and is published, in order.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cancelled_key_changes_finish_or_never_start() {
        let (m, keys) = in_memory();
        let (key, prefix, hash) = new_api_key();
        let (gate, started, release) = gated(Ok(()));
        let created = tokio::spawn({
            let (keys, m, stored) = (keys.clone(), m.clone(), hash.clone());
            async move {
                let write = move || {
                    gate()?;
                    m.create_api_key("ci", &prefix, &stored, "ingest")
                };
                keys.change(write, move |k, rec| k.add(hash, rec.id, rec.scope.clone())).await
            }
        });
        started.await.unwrap();
        let ran = Arc::new(AtomicBool::new(false));
        let wiped = tokio::spawn({
            let (keys, ran) = (keys.clone(), ran.clone());
            async move {
                let write = move || {
                    ran.store(true, Ordering::SeqCst);
                    Ok(())
                };
                keys.change(write, |k, ()| *k = KeySet::default()).await
            }
        });
        until("the second change to queue", || keys.pending.available_permits() == MAX_PENDING_KEY_CHANGES - 2).await;
        created.abort();
        wiped.abort();
        assert!(wiped.await.unwrap_err().is_cancelled());
        assert!(created.await.unwrap_err().is_cancelled());
        assert!(!keys.ingest_protected(), "not published before it commits");

        release.send(()).unwrap();
        until("the abandoned create to publish", || keys.ingest_protected()).await;
        until("every turn to be returned", || keys.pending.available_permits() == MAX_PENDING_KEY_CHANGES).await;
        assert!(!ran.load(Ordering::SeqCst), "the cancelled change never wrote");
        assert!(matches!(keys.snapshot().admit_ingest(&bearer(&key)), Ok(Some(_))));
        assert_matches_store(&keys, &m);
    }

    // AC3/AC4: concurrent creates and revokes leave exactly the stored keys,
    // a restart loads the same set, and only ingest keys toggle protection.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_key_changes_match_the_store_across_restarts() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("metadata.db");
        let m = Arc::new(Metadata::open(&path).unwrap());
        let keys = Arc::new(ApiKeys::load(&m).unwrap());
        let create = |name: String, scope: &'static str| {
            let (keys, m) = (keys.clone(), m.clone());
            tokio::spawn(async move { keys.create(&m, &name, scope).await.unwrap() })
        };
        let revoke = |id: i64| {
            let (keys, m) = (keys.clone(), m.clone());
            tokio::spawn(async move { keys.revoke(&m, id).await.unwrap() })
        };
        let first: Vec<_> =
            (0..8).flat_map(|i| [create(format!("i{i}"), "ingest"), create(format!("a{i}"), "admin")]).collect();
        let mut made = Vec::new();
        for t in first {
            made.push(t.await.unwrap());
        }
        assert!(keys.ingest_protected());
        assert_matches_store(&keys, &m);

        // Revoke half of each scope while creating more, all at once.
        let (gone, kept): (Vec<_>, Vec<_>) = made.into_iter().enumerate().partition(|(i, _)| i % 4 < 2);
        let revokes: Vec<_> = gone.iter().map(|(_, (rec, _))| revoke(rec.id)).collect();
        let more: Vec<_> =
            (0..8).map(|i| create(format!("m{i}"), if i % 2 == 0 { "ingest" } else { "admin" })).collect();
        for t in revokes {
            t.await.unwrap();
        }
        let mut live: Vec<_> = kept.into_iter().map(|(_, k)| k).collect();
        for t in more {
            live.push(t.await.unwrap());
        }
        let now = keys.snapshot();
        for (_, (rec, key)) in &gone {
            assert!(now.find(key).is_err(), "revoked key {} is refused", rec.id);
        }
        for (rec, key) in &live {
            assert_eq!(now.find(key).unwrap(), (rec.id, rec.scope.clone()), "scope of {}", rec.name);
        }
        assert_eq!(now.by_hash.len(), live.len());
        assert_matches_store(&keys, &m);

        // A restart loads exactly the committed keys.
        let before = keys.snapshot();
        drop((keys, m));
        let m = Arc::new(Metadata::open(&path).unwrap());
        let keys = Arc::new(ApiKeys::load(&m).unwrap());
        assert_eq!(*keys.snapshot(), *before);

        // Revoking every ingest key but one keeps ingestion protected; the
        // last reopens it; the admin keys left never protect it.
        let ingest: Vec<_> = live.iter().filter(|(rec, _)| rec.scope == "ingest").map(|(rec, _)| rec.id).collect();
        let (last, rest) = ingest.split_last().unwrap();
        let revokes: Vec<_> = rest
            .iter()
            .map(|&id| {
                let (keys, m) = (keys.clone(), m.clone());
                tokio::spawn(async move { keys.revoke(&m, id).await.unwrap() })
            })
            .collect();
        for t in revokes {
            t.await.unwrap();
        }
        assert!(keys.ingest_protected(), "one ingest key is left");
        keys.revoke(&m, *last).await.unwrap();
        assert!(!keys.ingest_protected());
        assert!(!keys.snapshot().by_hash.is_empty(), "admin keys remain");
        let (_, admin) = keys.create(&m, "another admin", "admin").await.unwrap();
        assert!(!keys.ingest_protected(), "admin keys do not protect ingestion");
        assert!(keys.snapshot().find(&admin).is_ok());
        assert_matches_store(&keys, &m);
    }

    // AC2/AC3: under churn of the first/last ingest key, every admission is
    // one consistent decision, and none accepts a key whose revoke returned.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn no_admission_with_a_key_after_its_revoke_returns() {
        let (m, keys) = in_memory();
        let (admin_rec, admin) = keys.create(&m, "automation", "admin").await.unwrap();
        let revoked = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let done = Arc::new(AtomicBool::new(false));
        let checkers: Vec<_> = (0..3)
            .map(|_| {
                let (keys, revoked, done, admin) = (keys.clone(), revoked.clone(), done.clone(), admin.clone());
                tokio::spawn(async move {
                    let mut checks = 0u64;
                    while !done.load(Ordering::SeqCst) {
                        let gone = revoked.lock().unwrap().clone();
                        let set = keys.snapshot();
                        for key in &gone {
                            match set.admit_ingest(&bearer(key)) {
                                Ok(None) => assert!(!set.ingest_protected()),
                                Ok(Some(p)) => panic!("revoked key admitted as {p:?}"),
                                Err(e) => assert!(set.ingest_protected() && e.status.as_u16() == 401),
                            }
                        }
                        match set.admit_ingest(&HeaderMap::new()) {
                            Ok(None) => assert!(!set.ingest_protected()),
                            other => assert!(set.ingest_protected() && other.is_err()),
                        }
                        let any_valid = set.admit_ingest(&bearer(&admin)).unwrap();
                        assert!(any_valid.is_none() || any_valid == Some((admin_rec.id, "admin".into())));
                        checks += 1;
                        tokio::task::yield_now().await;
                    }
                    checks
                })
            })
            .collect();

        for round in 0..50 {
            let (rec, key) = keys.create(&m, &format!("k{round}"), "ingest").await.unwrap();
            assert!(keys.snapshot().admit_ingest(&HeaderMap::new()).is_err(), "protected at once");
            keys.revoke(&m, rec.id).await.unwrap();
            assert_eq!(keys.snapshot().admit_ingest(&bearer(&key)).unwrap(), None, "open again at once");
            revoked.lock().unwrap().push(key);
        }
        done.store(true, Ordering::SeqCst);
        for c in checkers {
            assert!(c.await.unwrap() > 0);
        }
        assert_matches_store(&keys, &m);
    }

    // ---- AUTH-PERF-02: the session cache -----------------------------------

    use std::sync::atomic::AtomicUsize;

    type Found = metadata::Result<(Option<(String, i64)>, Duration)>;

    /// A lookup that finds `user` valid until `expires_at`, counting calls.
    fn valid(calls: &Arc<AtomicUsize>, expires_at: i64) -> impl FnOnce(&str) -> Found + Send + 'static {
        let calls = calls.clone();
        move |_| {
            calls.fetch_add(1, Ordering::SeqCst);
            Ok((Some(("admin".into(), expires_at)), Duration::ZERO))
        }
    }

    fn later(ms: i64) -> i64 {
        metadata::now_ms() + ms
    }

    fn stat(s: &Sessions, k: &str) -> u64 {
        s.snapshot()[k].as_u64().unwrap()
    }

    // AC4/AC5: a valid session is read once, then served from memory until
    // its expiry and never after; the token itself is not kept.
    #[tokio::test]
    async fn cached_sessions_are_used_until_their_expiry_only() {
        let s = Sessions::default();
        let calls = Arc::new(AtomicUsize::new(0));
        let token = random_token(32);
        let hash = sha256_hex(&token);
        let expires = later(300);
        for _ in 0..5 {
            assert_eq!(s.check(hash.clone(), valid(&calls, expires)).await.unwrap().as_deref(), Some("admin"));
        }
        assert_eq!(calls.load(Ordering::SeqCst), 1, "one SQLite lookup");
        assert_eq!((stat(&s, "cacheHits"), stat(&s, "lookups"), stat(&s, "cached")), (4, 1, 1));
        {
            let c = s.cache.lock().unwrap();
            assert!(c.by_hash.contains_key(&hash) && !c.by_hash.contains_key(&token), "keyed by hash only");
        }

        // At the expiry the entry is not used; SQLite (which also refuses an
        // expired session) decides.
        while metadata::now_ms() < expires {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let none = |_: &str| -> Found { Ok((None, Duration::ZERO)) };
        assert_eq!(s.check(hash.clone(), none).await.unwrap(), None);
        assert_eq!(stat(&s, "cached"), 0, "the expired entry is dropped");
        assert_eq!(s.check(hash, none).await.unwrap(), None, "and nothing negative is cached");
        assert_eq!(stat(&s, "lookups"), 3);
    }

    // AC4: logout invalidates at once, and a lookup that read the session
    // before the logout's delete cannot cache it again afterwards.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn invalidation_wins_over_a_fill_in_flight() {
        let s = Arc::new(Sessions::default());
        let calls = Arc::new(AtomicUsize::new(0));
        let hash = sha256_hex("token");
        s.check(hash.clone(), valid(&calls, later(60_000))).await.unwrap();
        s.invalidate(&hash);
        assert_eq!(stat(&s, "cached"), 0);

        // A lookup reads the (not yet deleted) session; the logout then
        // deletes and invalidates; only then does the lookup finish.
        let (gate, started, release) = gated(Ok((Some(("admin".to_string(), later(60_000))), Duration::ZERO)));
        let racing = tokio::spawn({
            let (s, hash) = (s.clone(), hash.clone());
            async move { s.check(hash, move |_| gate()).await }
        });
        started.await.unwrap();
        s.invalidate(&hash);
        release.send(()).unwrap();
        // The racing request read SQLite before the logout, so it may be
        // admitted (it is ordered before the logout), but it is not cached.
        assert_eq!(racing.await.unwrap().unwrap().as_deref(), Some("admin"));
        assert_eq!(stat(&s, "cached"), 0, "the ended session was not put back");
        let none = |_: &str| -> Found { Ok((None, Duration::ZERO)) };
        assert_eq!(s.check(hash, none).await.unwrap(), None, "the next request reads SQLite and is refused");
    }

    // AC4: the entry cap holds, expired entries go first, and a flood of
    // unknown tokens caches nothing.
    #[tokio::test]
    async fn the_cache_is_bounded_and_unknown_tokens_never_grow_it() {
        let s = Sessions::default();
        let calls = Arc::new(AtomicUsize::new(0));
        let none = |_: &str| -> Found { Ok((None, Duration::ZERO)) };
        for i in 0..10_000 {
            assert_eq!(s.check(sha256_hex(&format!("unknown-{i}")), none).await.unwrap(), None);
        }
        assert_eq!(stat(&s, "cached"), 0);
        assert_eq!(stat(&s, "lookups"), 10_000, "every unknown token is checked in SQLite");

        // Fill past the cap: soon-expiring entries first, then live ones.
        for i in 0..100 {
            s.check(sha256_hex(&format!("short-{i}")), valid(&calls, later(50))).await.unwrap();
        }
        tokio::time::sleep(Duration::from_millis(60)).await;
        for i in 0..SESSION_CACHE_ENTRIES + 500 {
            s.check(sha256_hex(&format!("live-{i}")), valid(&calls, later(60_000))).await.unwrap();
            assert!(stat(&s, "cached") as usize <= SESSION_CACHE_ENTRIES);
        }
        let c = s.cache.lock().unwrap();
        assert_eq!(c.by_hash.len(), SESSION_CACHE_ENTRIES);
        assert!(c.by_hash.values().all(|e| e.expires_at > metadata::now_ms()), "expired entries were evicted first");
    }

    // AC4: at most MAX_SESSION_FILLS lookups read SQLite at once, waiting
    // ones hold no blocking thread, and cancelled ones release their turn.
    #[test]
    fn concurrent_fills_are_bounded() {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .max_blocking_threads(MAX_SESSION_FILLS + 1)
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let s = Arc::new(Sessions::default());
            let (running, peak) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
            let (release, released) = std::sync::mpsc::channel::<()>();
            let released = Arc::new(std::sync::Mutex::new(released));
            let lookups: Vec<_> = (0..64)
                .map(|i| {
                    let (s, running, peak, released) = (s.clone(), running.clone(), peak.clone(), released.clone());
                    tokio::spawn(async move {
                        let lookup = move |_: &str| -> Found {
                            let now = running.fetch_add(1, Ordering::SeqCst) + 1;
                            peak.fetch_max(now, Ordering::SeqCst);
                            let _ = released.lock().unwrap().recv();
                            running.fetch_sub(1, Ordering::SeqCst);
                            Ok((None, Duration::ZERO))
                        };
                        s.check(sha256_hex(&format!("t{i}")), lookup).await
                    })
                })
                .collect();
            until("the fills to start", || running.load(Ordering::SeqCst) == MAX_SESSION_FILLS).await;
            tokio::time::sleep(Duration::from_millis(50)).await;
            assert_eq!(peak.load(Ordering::SeqCst), MAX_SESSION_FILLS);
            assert_eq!(stat(&s, "inFlight"), 64);
            // The one spare blocking thread is free: waiters hold none.
            tokio::time::timeout(Duration::from_secs(5), tokio::task::spawn_blocking(|| ())).await.unwrap().unwrap();
            // Cancel half of the waiting ones; the rest finish.
            for t in &lookups[48..] {
                t.abort();
            }
            for _ in 0..64 {
                let _ = release.send(());
            }
            drop(release);
            for t in lookups {
                let _ = t.await;
            }
            assert_eq!(peak.load(Ordering::SeqCst), MAX_SESSION_FILLS);
            until("every turn to return", || s.fills.available_permits() == MAX_SESSION_FILLS).await;
            assert_eq!(stat(&s, "inFlight"), 0);
        });
    }

    // AC5: a database error fails the check (no fallback, no bypass) and
    // caches nothing; a panicking lookup fails too.
    #[tokio::test]
    async fn lookup_failures_fail_closed() {
        let s = Sessions::default();
        let hash = sha256_hex("token");
        let broken = |_: &str| -> Found { Err(MetadataError::Sqlite(rusqlite::Error::InvalidQuery)) };
        assert_eq!(s.check(hash.clone(), broken).await.unwrap_err().status.as_u16(), 500);
        let panics = |_: &str| -> Found { panic!("injected lookup panic") };
        assert_eq!(s.check(hash.clone(), panics).await.unwrap_err().status.as_u16(), 500);
        assert_eq!((stat(&s, "failed"), stat(&s, "cached"), stat(&s, "inFlight")), (1, 0, 0));
        assert_eq!(s.fills.available_permits(), MAX_SESSION_FILLS);
    }

    // AC5: against the real store, sessions come and go with SQLite: login,
    // logout, expiry, a missing user and a restart (an empty cache).
    #[tokio::test]
    async fn sessions_follow_the_store() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("metadata.db");
        let m = Arc::new(Metadata::open(&path).unwrap());
        let s = Sessions::default();
        async fn check(s: &Sessions, m: &Arc<Metadata>, token: &str) -> ApiResult<Option<String>> {
            let m = m.clone();
            let lookup = move |hash: &str| -> Found {
                let (found, lock) = m.find_session_timed(hash)?;
                Ok((found.map(|(u, expires)| (u.username, expires)), lock))
            };
            s.check(sha256_hex(token), lookup).await
        }
        let uid = m.upsert_user("admin", &hash_password("pw").unwrap()).unwrap();
        let (token, short) = (random_token(32), random_token(32));
        m.create_session(&sha256_hex(&token), uid, 60_000).unwrap();
        m.create_session(&sha256_hex(&short), uid, 150).unwrap();
        assert_eq!(check(&s, &m, &token).await.unwrap().as_deref(), Some("admin"));
        assert_eq!(check(&s, &m, &short).await.unwrap().as_deref(), Some("admin"));
        assert_eq!(check(&s, &m, "never-issued").await.unwrap(), None);
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert_eq!(check(&s, &m, &short).await.unwrap(), None, "expired, though cached");

        // Logout as routes::session does it: delete, then invalidate.
        m.delete_session(&sha256_hex(&token)).unwrap();
        s.invalidate(&sha256_hex(&token));
        assert_eq!(check(&s, &m, &token).await.unwrap(), None);

        // A restart keeps stored sessions and starts with an empty cache.
        let other = random_token(32);
        m.create_session(&sha256_hex(&other), uid, 60_000).unwrap();
        drop(m);
        let m = Arc::new(Metadata::open(&path).unwrap());
        let s = Sessions::default();
        assert_eq!(check(&s, &m, &other).await.unwrap().as_deref(), Some("admin"));
        assert_eq!(check(&s, &m, &token).await.unwrap(), None);
        assert_eq!(stat(&s, "lookups"), 2);
    }
}
