use std::collections::HashMap;
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use metadata::Metadata;
use query::Engine;
use storage::{Batch, Storage};
use telemetry::{Signal, Timestamp};
use tokio::sync::{Semaphore, broadcast, watch};

use crate::config::{Config, MemoryBudgets};
use crate::error::{ApiError, ApiResult};

/// A committed batch, fanned out to live-tail subscribers.
#[derive(Clone)]
pub struct LiveBatch {
    pub signal: Signal,
    pub batch: Arc<Batch>,
}

pub struct AppState {
    pub config: Config,
    pub budgets: MemoryBudgets,
    pub storage: Arc<Storage>,
    pub engine: Arc<Engine>,
    pub metadata: Arc<Metadata>,
    pub live: broadcast::Sender<LiveBatch>,
    pub live_permits: Arc<Semaphore>,
    pub query_permits: Arc<Semaphore>,
    /// Ingest admission: one permit per request from before its body is
    /// read until its write is acknowledged (see `routes::ingest`).
    pub ingest_permits: Arc<Semaphore>,
    /// Total permits in `ingest_permits`.
    pub ingest_max_concurrent: usize,
    /// Ingest requests turned away because every permit was taken.
    pub ingest_rejected: AtomicU64,
    /// Bounds rejected ingest bodies being discarded at once.
    pub discard_permits: Arc<Semaphore>,
    /// Bounds rejected ingest bodies held at all (discarding or waiting).
    pub discard_pending: Arc<Semaphore>,
    pub started_at: Instant,
    pub started_ts: Timestamp,
    pub received_bytes: AtomicU64,
    pub received_requests: AtomicU64,
    pub http: reqwest::Client,
    /// Last time each API key's `last_used_at` was persisted.
    pub key_touch: Mutex<HashMap<i64, Instant>>,
    /// Flips to `true` when shutdown starts; long-lived responses (live
    /// tail) end so graceful shutdown does not wait on them forever.
    pub shutdown: watch::Receiver<bool>,
}

pub type SharedState = Arc<AppState>;

impl AppState {
    /// Run blocking work (SQLite, filesystem) off the async runtime.
    pub async fn blocking<T, F>(&self, f: F) -> ApiResult<T>
    where
        T: Send + 'static,
        F: FnOnce() -> ApiResult<T> + Send + 'static,
    {
        tokio::task::spawn_blocking(f).await?
    }

    pub async fn db<T, F>(&self, f: F) -> ApiResult<T>
    where
        T: Send + 'static,
        F: FnOnce(&Metadata) -> metadata::Result<T> + Send + 'static,
    {
        let m = self.metadata.clone();
        tokio::task::spawn_blocking(move || f(&m).map_err(ApiError::from)).await?
    }

    /// Run a query on the blocking pool under the concurrency limit. When
    /// all query slots stay busy, fail with 503 instead of queueing forever.
    pub async fn query<T, F>(&self, f: F) -> ApiResult<T>
    where
        T: Send + 'static,
        F: FnOnce(&Engine) -> ApiResult<T> + Send + 'static,
    {
        let permit = tokio::time::timeout(Duration::from_secs(10), self.query_permits.clone().acquire_owned())
            .await
            .map_err(|_| ApiError::unavailable("too many concurrent queries; try again shortly"))?
            .map_err(|_| ApiError::unavailable("shutting down"))?;
        let engine = self.engine.clone();
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            f(&engine)
        })
        .await?
    }
}
