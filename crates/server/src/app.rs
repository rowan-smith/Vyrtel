//! Process wiring: open storage and metadata, start background jobs, serve,
//! and drain accepted work on shutdown.
//!
//! Shutdown (REL-05): the first stop request flips `AppState::shutdown`,
//! which closes the listener, refuses new ingest, queries and live streams
//! with 503, and ends live-tail streams. Requests already admitted run to
//! their acknowledgement, then every storage stream writes out its queue and
//! flushes its WAL. All of it shares one deadline,
//! `server.shutdown_timeout`; a second stop request ends the wait at once.
//! Giving up returns [`IncompleteShutdown`], which says what was unfinished.

use std::future::Future;
use std::pin::{Pin, pin};
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
use tokio::sync::{Semaphore, broadcast, oneshot, watch};
use tokio::task::JoinHandle;

use crate::config::{Config, Durability, format_duration, human_bytes};
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

        let admission = config.ingest_admission();
        if let Some(warning) = admission.warning() {
            tracing::warn!(
                max_concurrent = admission.limit,
                max_request_bytes = admission.max_request_bytes,
                ingest_budget_bytes = admission.budget,
                estimated_peak_bytes = admission.estimated_peak,
                "{warning}"
            );
        }
        let ingest_slots = admission.limit;
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
            ingest_max_concurrent: ingest_slots,
            ingest_admission: admission,
            ingest_rejected: AtomicU64::new(0),
            discard_permits: Arc::new(Semaphore::new(crate::routes::ingest::MAX_DISCARDING)),
            discard_pending: Arc::new(Semaphore::new(crate::routes::ingest::MAX_DISCARD_PENDING)),
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

    /// Serve until `shutdown_signal` resolves, then drain accepted work
    /// within `server.shutdown_timeout` (see [`App::serve_until`]).
    pub async fn serve(
        self,
        listener: TcpListener,
        shutdown_signal: impl Future<Output = ()> + Send + 'static,
    ) -> anyhow::Result<()> {
        self.serve_until(listener, shutdown_signal, std::future::pending()).await
    }

    /// Serve until `stop` resolves, then stop accepting connections, refuse
    /// new work and drain what was accepted: in-flight requests, queued
    /// writes and the final WAL flush, all within `server.shutdown_timeout`.
    /// If the deadline passes, or `force` resolves first (a second stop
    /// request), stop waiting and return [`IncompleteShutdown`]. The caller
    /// should then exit without running destructors, which could wait on
    /// the work that was left running.
    pub async fn serve_until(
        self,
        listener: TcpListener,
        stop: impl Future<Output = ()> + Send + 'static,
        force: impl Future<Output = ()>,
    ) -> anyhow::Result<()> {
        let timeout = self.state.config.server.shutdown_timeout;
        let (began_tx, mut began_rx) = oneshot::channel();
        let notify = self.shutdown.clone();
        let draining = self.state.clone();
        let serving = axum::serve(listener, self.router.clone()).with_graceful_shutdown(async move {
            stop.await;
            let _ = began_tx.send(tokio::time::Instant::now());
            // Refuse new work and end live-tail streams so draining
            // connections can finish.
            let _ = notify.send(true);
            let work = Unfinished::now(&draining);
            tracing::info!(
                deadline = %format_duration(timeout),
                ingest_requests = work.ingest_requests,
                queued_events = work.queued_events(),
                live_streams = work.live_streams,
                "shutdown started; draining accepted work"
            );
        });
        let mut serving = pin!(serving.into_future());
        let mut force = pin!(force);

        // Until the stop request. Serving ends first only on an error, or
        // when the stop request and the drain both complete in one poll.
        let began = tokio::select! {
            r = &mut serving => {
                r.context("HTTP server error")?;
                None
            }
            Ok(at) = &mut began_rx => Some(at),
        };
        let deadline = began.unwrap_or_else(tokio::time::Instant::now) + timeout;
        if began.is_some() {
            // In-flight requests finish and live-tail streams end.
            let cut_short = tokio::select! {
                r = &mut serving => {
                    r.context("HTTP server error")?;
                    None
                }
                _ = tokio::time::sleep_until(deadline) => Some(StopCause::Deadline(timeout)),
                _ = &mut force => Some(StopCause::Forced),
            };
            if let Some(cause) = cut_short {
                return Err(IncompleteShutdown::report(&self.state, cause, ShutdownPhase::Requests).into());
            }
        }
        let started = deadline - timeout;
        self.finish(deadline, force).await?;
        tracing::info!(elapsed_ms = started.elapsed().as_millis() as u64, "shutdown complete");
        Ok(())
    }

    /// Stop background jobs and flush storage, waiting at most
    /// `server.shutdown_timeout`.
    pub async fn shutdown(self) -> Result<(), IncompleteShutdown> {
        let deadline = tokio::time::Instant::now() + self.state.config.server.shutdown_timeout;
        self.finish(deadline, pin!(std::future::pending())).await
    }

    async fn finish(
        self,
        deadline: tokio::time::Instant,
        mut force: Pin<&mut impl Future<Output = ()>>,
    ) -> Result<(), IncompleteShutdown> {
        let _ = self.shutdown.send(true);
        // Background jobs are not accepted work: past the deadline they are
        // abandoned, not reported.
        let mut tasks = self.tasks;
        let joined = tokio::time::timeout_at(deadline, async {
            for t in &mut tasks {
                let _ = t.await;
            }
        });
        if joined.await.is_err() {
            tasks.iter().for_each(JoinHandle::abort);
            tracing::warn!("background jobs did not stop before the shutdown deadline; abandoned");
        }
        let state = self.state;
        if tokio::time::timeout_at(deadline, persist_query_stats(&state)).await.is_err() {
            tracing::warn!("query statistics were not saved before the shutdown deadline");
        }
        // Every stream writes out its queue and flushes its WAL. Unsealed
        // data is replayed from the WAL on the next start.
        let storage = state.storage.clone();
        let flush = tokio::task::spawn_blocking(move || storage.stop_writers());
        let cause = tokio::select! {
            _ = flush => None,
            _ = tokio::time::sleep_until(deadline) => Some(StopCause::Deadline(state.config.server.shutdown_timeout)),
            _ = &mut force => Some(StopCause::Forced),
        };
        if let Some(cause) = cause {
            return Err(IncompleteShutdown::report(&state, cause, ShutdownPhase::Storage));
        }
        // A seal or compaction in progress is not accepted work: its input
        // is still in the WAL, so stopping it part-way loses nothing.
        let storage = state.storage.clone();
        let stopped = tokio::task::spawn_blocking(move || storage.shutdown());
        let finished = tokio::select! {
            _ = stopped => true,
            _ = tokio::time::sleep_until(deadline) => false,
            _ = &mut force => false,
        };
        if !finished {
            tracing::warn!(
                "sealing or compaction was still running when shutdown stopped waiting; \
                 nothing acknowledged is affected and the next start redoes it from the WAL"
            );
        }
        Ok(())
    }
}

/// Why shutdown stopped waiting for accepted work.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopCause {
    /// `server.shutdown_timeout` passed.
    Deadline(Duration),
    /// A second stop request arrived.
    Forced,
}

/// What shutdown was waiting for when it gave up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShutdownPhase {
    /// HTTP requests: ingest awaiting acknowledgement, queries, live tail.
    Requests,
    /// Storage writing out queued batches and flushing its WAL.
    Storage,
}

/// Accepted work still in progress.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unfinished {
    /// Ingest requests admitted and not yet answered.
    pub ingest_requests: usize,
    /// Events submitted to storage and not yet acknowledged, per signal.
    pub queued: Vec<(Signal, usize)>,
    /// Live-tail streams still open.
    pub live_streams: usize,
}

impl Unfinished {
    /// Reads lock-free counters only, so it works with a stuck writer.
    pub fn now(state: &AppState) -> Unfinished {
        Unfinished {
            ingest_requests: state.ingest_max_concurrent - state.ingest_permits.available_permits(),
            queued: Signal::ALL
                .into_iter()
                .map(|s| (s, state.storage.stream(s).queued_events()))
                .filter(|(_, n)| *n > 0)
                .collect(),
            live_streams: state.config.query.max_live_streams.max(1) - state.live_permits.available_permits(),
        }
    }

    pub fn queued_events(&self) -> usize {
        self.queued.iter().map(|(_, n)| n).sum()
    }
}

/// Shutdown gave up before all accepted work was done. Acknowledged events
/// are not affected: they reach the WAL before they are acknowledged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IncompleteShutdown {
    pub cause: StopCause,
    pub phase: ShutdownPhase,
    pub unfinished: Unfinished,
}

impl IncompleteShutdown {
    /// Capture the unfinished work and log it.
    fn report(state: &AppState, cause: StopCause, phase: ShutdownPhase) -> IncompleteShutdown {
        let e = IncompleteShutdown { cause, phase, unfinished: Unfinished::now(state) };
        let u = &e.unfinished;
        tracing::error!(
            ingest_requests = u.ingest_requests,
            queued_events = u.queued_events(),
            live_streams = u.live_streams,
            "{e}"
        );
        e
    }
}

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

impl std::fmt::Display for IncompleteShutdown {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("shutdown incomplete: ")?;
        match self.cause {
            StopCause::Deadline(d) => write!(f, "server.shutdown_timeout ({}) passed", format_duration(d))?,
            StopCause::Forced => f.write_str("a second stop signal arrived")?,
        }
        f.write_str(match self.phase {
            ShutdownPhase::Requests => " while waiting for in-flight requests",
            ShutdownPhase::Storage => " while storage was writing queued batches and flushing the WAL",
        })?;
        let u = &self.unfinished;
        let mut parts = Vec::new();
        if u.ingest_requests > 0 {
            parts.push(format!(
                "{} not acknowledged (the client gets no response and should retry)",
                plural(u.ingest_requests, "ingest request", "ingest requests")
            ));
        }
        if !u.queued.is_empty() {
            let per: Vec<String> = u.queued.iter().map(|(s, n)| format!("{} {n}", s.as_str())).collect();
            parts.push(format!(
                "{} not written ({})",
                plural(u.queued_events(), "queued event", "queued events"),
                per.join(", ")
            ));
        }
        if u.live_streams > 0 {
            parts.push(format!(
                "{} still open (client not reading)",
                plural(u.live_streams, "live-tail stream", "live-tail streams")
            ));
        }
        if parts.is_empty() {
            f.write_str("; no requests or queued events were left")?;
        } else {
            write!(f, "; unfinished: {}", parts.join("; "))?;
        }
        f.write_str(". Acknowledged events are already in the WAL and are replayed on the next start; ")?;
        f.write_str(match self.cause {
            StopCause::Deadline(_) => "raise server.shutdown_timeout if draining regularly takes longer",
            StopCause::Forced => "send a single stop signal to let accepted work finish",
        })
    }
}

impl std::error::Error for IncompleteShutdown {}

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

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::sync::{Arc, Mutex};

    use super::*;

    #[derive(Clone, Default)]
    struct Captured(Arc<Mutex<Vec<u8>>>);

    impl Write for Captured {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// Start an app with `max_concurrent` and return what it logged.
    async fn startup_log(max_concurrent: usize) -> String {
        let dir = tempfile::tempdir().unwrap();
        let mut config = Config::default();
        config.storage.path = dir.path().to_path_buf();
        config.alerts.enabled = false;
        config.ingest.max_concurrent = max_concurrent;
        let out = Captured::default();
        let writer = out.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(move || writer.clone())
            .with_max_level(tracing::Level::WARN)
            .with_ansi(false)
            .finish();
        let app = tracing::subscriber::with_default(subscriber, || App::build(config)).unwrap();
        app.shutdown().await.unwrap();
        String::from_utf8(out.0.lock().unwrap().clone()).unwrap()
    }

    // REL-14 AC4: one warning at startup for an over-budget explicit limit,
    // none within budget or when derived.
    #[tokio::test]
    async fn over_budget_explicit_ingest_limit_warns_once_at_startup() {
        let log = startup_log(8).await;
        assert_eq!(log.matches("the limit is honoured").count(), 1, "{log}");
        for part in
            ["max_concurrent=8", "max_request_bytes=1048576", "ingest_budget_bytes=80530635", "estimated_peak_bytes="]
        {
            assert!(log.contains(part), "{part} in {log}");
        }
        assert!(!startup_log(2).await.contains("honoured"));
        assert!(!startup_log(0).await.contains("honoured"));
    }

    // REL-05 AC1: once shutdown starts, new work gets 503 and the
    // connection closes; readiness takes the instance out of rotation.
    #[tokio::test]
    async fn draining_refuses_new_work() {
        use axum::body::Body;
        use axum::http::{Request, StatusCode, header};
        use tower::ServiceExt;

        let dir = tempfile::tempdir().unwrap();
        let mut config = Config::default();
        config.storage.path = dir.path().to_path_buf();
        config.alerts.enabled = false;
        let app = App::build(config).unwrap();
        let request = |method: &str, uri: &str, body: &'static str| {
            Request::builder()
                .method(method)
                .uri(uri)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body))
                .unwrap()
        };
        let r = app.router.clone().oneshot(request("GET", "/ready", "")).await.unwrap();
        assert_eq!(r.status(), StatusCode::OK);

        app.shutdown.send(true).unwrap();
        for (method, uri, body) in [
            ("POST", "/api/v1/events", r#"{"message":"late"}"#),
            ("POST", "/v1/logs", "{}"),
            ("POST", "/api/v1/query/logs", r#"{"query":""}"#),
            ("GET", "/api/v1/live/logs", ""),
        ] {
            let r = app.router.clone().oneshot(request(method, uri, body)).await.unwrap();
            assert_eq!(r.status(), StatusCode::SERVICE_UNAVAILABLE, "{uri}");
            assert_eq!(r.headers()[header::RETRY_AFTER], "1", "{uri}");
            assert_eq!(r.headers()[header::CONNECTION], "close", "{uri}");
            let body = axum::body::to_bytes(r.into_body(), 1 << 16).await.unwrap();
            let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(v["error"]["code"], "shutting_down", "{uri}");
        }
        let r = app.router.clone().oneshot(request("GET", "/ready", "")).await.unwrap();
        assert_eq!(r.status(), StatusCode::SERVICE_UNAVAILABLE);
        let body = axum::body::to_bytes(r.into_body(), 1 << 16).await.unwrap();
        assert_eq!(serde_json::from_slice::<serde_json::Value>(&body).unwrap()["status"], "draining");
        // Liveness is unaffected: the process is still running.
        let r = app.router.clone().oneshot(request("GET", "/health", "")).await.unwrap();
        assert_eq!(r.status(), StatusCode::OK);
        assert_eq!(app.state.storage.stream(Signal::Logs).queued_events(), 0, "nothing reached storage");
        app.shutdown().await.unwrap();
    }

    // REL-05 AC3: the diagnostic names the cause, the phase and every kind
    // of unfinished work, and what happens to acknowledged data.
    #[test]
    fn incomplete_shutdown_describes_unfinished_work() {
        let e = IncompleteShutdown {
            cause: StopCause::Deadline(Duration::from_secs(8)),
            phase: ShutdownPhase::Requests,
            unfinished: Unfinished {
                ingest_requests: 2,
                queued: vec![(Signal::Logs, 150), (Signal::Traces, 1)],
                live_streams: 1,
            },
        };
        assert_eq!(
            e.to_string(),
            "shutdown incomplete: server.shutdown_timeout (8s) passed while waiting for in-flight requests; \
             unfinished: 2 ingest requests not acknowledged (the client gets no response and should retry); \
             151 queued events not written (logs 150, traces 1); 1 live-tail stream still open (client not reading). \
             Acknowledged events are already in the WAL and are replayed on the next start; \
             raise server.shutdown_timeout if draining regularly takes longer"
        );
        let e = IncompleteShutdown {
            cause: StopCause::Forced,
            phase: ShutdownPhase::Storage,
            unfinished: Unfinished { ingest_requests: 0, queued: vec![(Signal::Metrics, 1)], live_streams: 0 },
        };
        let text = e.to_string();
        assert!(
            text.starts_with(
                "shutdown incomplete: a second stop signal arrived while storage was writing queued batches \
                 and flushing the WAL; unfinished: 1 queued event not written (metrics 1)."
            ),
            "{text}"
        );
        assert!(text.ends_with("next start; send a single stop signal to let accepted work finish"), "{text}");
        let idle = IncompleteShutdown {
            cause: StopCause::Forced,
            phase: ShutdownPhase::Storage,
            unfinished: Unfinished { ingest_requests: 0, queued: vec![], live_streams: 0 },
        };
        assert!(idle.to_string().contains("; no requests or queued events were left."), "{idle}");
    }
}
