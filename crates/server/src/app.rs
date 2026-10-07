//! Process wiring: open storage and metadata, start background jobs, serve.

use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::Context;
use axum::Router;
use metadata::Metadata;
use query::{Engine, QueryLimits};
use storage::{CommitHook, Storage, StorageConfig};
use telemetry::{Signal, Timestamp};
use tokio::net::TcpListener;
use tokio::sync::{Semaphore, broadcast, watch};
use tokio::task::JoinHandle;

use crate::config::{Config, Durability, human_bytes};
use crate::state::{AppState, LiveBatch, SharedState};

/// Batches retained for live-tail subscribers that fall behind.
const LIVE_CHANNEL_CAPACITY: usize = 64;

pub struct App {
    pub state: SharedState,
    pub router: Router,
    /// Admin password generated on first start (auth enabled, none set).
    pub generated_password: Option<String>,
    shutdown: Arc<watch::Sender<bool>>,
    tasks: Vec<JoinHandle<()>>,
}

fn storage_config(config: &Config) -> StorageConfig {
    let budgets = config.budgets();
    let mut sc = StorageConfig::new(&config.storage.path);
    sc.durability = match config.storage.durability {
        Durability::Normal => storage::Durability::Normal,
        Durability::Strict => storage::Durability::Strict,
    };
    sc.fsync_interval = config.storage.fsync_interval;
    sc.index_cache_bytes = budgets.index_cache as usize;
    sc.segment.block_events = config.storage.block_events;
    sc.segment.zstd_level = config.storage.zstd_level;
    sc.compaction.enabled = config.storage.compaction;
    // Compaction holds its inputs in memory; keep that inside the
    // background budget.
    sc.compaction.max_input_bytes = (budgets.background / 3).max(1 << 20);
    for (i, signal) in Signal::ALL.into_iter().enumerate() {
        let s = sc.stream_mut(signal);
        s.segment_target_bytes = budgets.segment_target[i];
        s.segment_max_age = config.storage.segment_max_age;
        s.segment_max_events = config.storage.segment_max_events;
        s.queue_events = config.ingest.queue_capacity;
        s.retention = match signal {
            Signal::Logs => config.retention.logs,
            Signal::Traces => config.retention.traces,
            Signal::Metrics => config.retention.metrics,
        };
    }
    sc
}

impl App {
    /// Open everything and start background tasks. Must run inside a Tokio
    /// runtime.
    pub fn build(config: Config) -> anyhow::Result<App> {
        config.validate().map_err(anyhow::Error::msg)?;
        let budgets = config.budgets();
        std::fs::create_dir_all(&config.storage.path)
            .with_context(|| format!("creating data directory {}", config.storage.path.display()))?;

        let metadata = Arc::new(
            Metadata::open(&config.storage.path.join("metadata").join("metadata.db"))
                .context("opening metadata store")?,
        );

        let (live_tx, _) = broadcast::channel::<LiveBatch>(LIVE_CHANNEL_CAPACITY);
        let hook_tx = live_tx.clone();
        let hook: CommitHook = Arc::new(move |signal, batch| {
            // Only pay for fan-out when someone is watching.
            if hook_tx.receiver_count() > 0 {
                let _ = hook_tx.send(LiveBatch { signal, batch: batch.clone() });
            }
        });
        let storage = Arc::new(Storage::open(storage_config(&config), Some(hook)).context("opening storage")?);
        let engine = Arc::new(Engine::new(
            storage.clone(),
            QueryLimits {
                timeout: config.query.timeout,
                max_scan_bytes: config.query.max_scan_bytes.0,
                max_results: config.query.max_results,
            },
        ));

        let ingest_slots = (budgets.ingest / config.ingest.max_request_size.0.max(1)).clamp(2, 1024) as usize;
        let http = reqwest::Client::builder()
            .user_agent(concat!("vyrtel/", env!("CARGO_PKG_VERSION")))
            .build()
            .context("building HTTP client")?;

        let (shutdown, rx) = watch::channel(false);
        let state: SharedState = Arc::new(AppState {
            budgets,
            storage,
            engine,
            metadata,
            live: live_tx,
            live_permits: Arc::new(Semaphore::new(config.query.max_live_streams.max(1))),
            query_permits: Arc::new(Semaphore::new(config.query.max_concurrent)),
            ingest_permits: Arc::new(Semaphore::new(ingest_slots)),
            started_at: Instant::now(),
            started_ts: Timestamp::now(),
            received_bytes: AtomicU64::new(0),
            received_requests: AtomicU64::new(0),
            http,
            key_touch: Mutex::new(Default::default()),
            shutdown: rx.clone(),
            config,
        });

        let generated_password = crate::auth::bootstrap_admin(&state).context("initialising admin user")?;
        let router = crate::routes::router(state.clone());

        let mut tasks = vec![tokio::spawn(flush_query_stats(state.clone(), rx.clone()))];
        tasks.push(tokio::spawn(purge_sessions(state.clone(), rx.clone())));
        if state.config.alerts.enabled {
            tasks.push(tokio::spawn(crate::alerts::run(state.clone(), rx)));
        }
        Ok(App { state, router, generated_password, shutdown: Arc::new(shutdown), tasks })
    }

    pub fn banner(&self, bound: std::net::SocketAddr) -> String {
        let c = &self.state.config;
        let s = self.state.storage.stats();
        let stored: u64 = [&s.logs, &s.traces, &s.metrics]
            .iter()
            .map(|x| x.segment_data_bytes + x.segment_index_bytes + x.wal_bytes)
            .sum();
        // SocketAddr's Display brackets IPv6 addresses. A wildcard bind (0.0.0.0 / [::]) isn't
        // something you can open in a browser, so the UI link points at localhost instead.
        let url = format!("http://{bound}");
        let web = if bound.ip().is_unspecified() { format!("http://localhost:{}", bound.port()) } else { url.clone() };
        let mut out = format!(
            "Vyrtel {}\n\n\
             Web UI              {web}\n\
             Data directory      {}\n\
             HTTP                {url}\n\
             OTLP HTTP           {url}/v1/*\n\
             Storage             {}\n\
             Memory limit        {}\n\
             Durability          {}\n\
             Authentication      {}\n",
            env!("CARGO_PKG_VERSION"),
            c.storage.path.display(),
            human_bytes(stored),
            c.storage.max_memory,
            match c.storage.durability {
                Durability::Normal => "normal",
                Durability::Strict => "strict",
            },
            if c.auth.enabled { "enabled" } else { "disabled (open access; see docs/configuration.md)" },
        );
        if let Some(pw) = &self.generated_password {
            out.push_str(&format!(
                "\nAdmin login         {} / {pw}\n                    (generated once; set auth.admin_password to choose your own)\n",
                c.auth.admin_username
            ));
        }
        out.push_str("\nReady.\n");
        out
    }

    /// Serve until `shutdown_signal` resolves, then flush and stop.
    pub async fn serve(
        self,
        listener: TcpListener,
        shutdown_signal: impl std::future::Future<Output = ()> + Send + 'static,
    ) -> anyhow::Result<()> {
        let router = self.router.clone();
        let notify = self.shutdown.clone();
        axum::serve(listener, router)
            .with_graceful_shutdown(async move {
                shutdown_signal.await;
                // End live-tail streams so draining connections can finish.
                let _ = notify.send(true);
            })
            .await
            .context("HTTP server error")?;
        self.shutdown().await;
        Ok(())
    }

    pub async fn shutdown(self) {
        let _ = self.shutdown.send(true);
        for t in self.tasks {
            let _ = tokio::time::timeout(Duration::from_secs(5), t).await;
        }
        let state = self.state;
        persist_query_stats(&state).await;
        // Storage flushes WALs and joins its threads on drop/shutdown.
        let storage = state.storage.clone();
        let _ = tokio::task::spawn_blocking(move || storage.shutdown()).await;
    }
}

async fn persist_query_stats(state: &SharedState) {
    let rows: Vec<metadata::StoredQueryStats> = state
        .engine
        .stats()
        .drain()
        .into_iter()
        .map(|s| metadata::StoredQueryStats {
            signal: s.signal,
            field: s.field,
            index: s.index,
            queries: s.queries,
            bytes_read: s.bytes_read,
            segments_scanned: s.segments_scanned,
            segments_skipped: s.segments_skipped,
            total_ms: s.total_ms,
            last_seen: metadata::now_ms(),
        })
        .collect();
    if rows.is_empty() {
        return;
    }
    if let Err(e) = state.db(move |m| m.add_query_stats(&rows)).await {
        tracing::warn!(error = %e.message, "could not persist query statistics");
    }
}

async fn flush_query_stats(state: SharedState, mut shutdown: watch::Receiver<bool>) {
    let mut tick = tokio::time::interval(Duration::from_secs(60));
    tick.tick().await;
    loop {
        tokio::select! {
            _ = tick.tick() => persist_query_stats(&state).await,
            _ = shutdown.changed() => return,
        }
    }
}

async fn purge_sessions(state: SharedState, mut shutdown: watch::Receiver<bool>) {
    let mut tick = tokio::time::interval(Duration::from_secs(3600));
    loop {
        tokio::select! {
            _ = tick.tick() => {
                let _ = state.db(|m| m.purge_expired_sessions()).await;
            }
            _ = shutdown.changed() => return,
        }
    }
}
