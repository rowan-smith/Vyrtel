//! Live tail over Server-Sent Events.
//!
//! The storage writer publishes every committed batch on a bounded
//! broadcast channel. Each SSE connection filters batches with its own
//! compiled query. Backpressure: the channel keeps a fixed number of recent
//! batches; a client that falls behind receives a `lagged` event with the
//! number of skipped batches instead of making the server buffer for it.
//! Per batch, at most [`MAX_EVENTS_PER_BATCH`] matches are sent.
//!
//! A stream sends only while the credential that opened it is still
//! valid. It keeps the [`Grant`] it was admitted with and rechecks it
//! before sending anything further, whenever an API key or session changes
//! and at least every [`REAUTHORIZE_EVERY`]. When the recheck refuses it,
//! the events matched but not yet sent are dropped, the stream sends
//! `unauthorized` (data: the error object a request would get) and ends.
//! When the recheck cannot be decided (the session store is unreadable) it
//! fails closed the same way with `unavailable` and a `retry`.
//!
//! When shutdown starts, each stream sends what it has already matched, then
//! a `shutdown` event carrying an SSE `retry` of [`RECONNECT_AFTER`], and
//! ends. The response finishes normally (no reset), so `EventSource`
//! reconnects by itself and reaches the next instance once it is up.

use std::collections::VecDeque;
use std::convert::Infallible;
use std::time::Duration;

use axum::Extension;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::sse::{Event, KeepAlive, Sse};
use futures_util::Stream;
use query::eval::{CExpr, eval};
use serde::Deserialize;
use telemetry::Signal;
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::{OwnedSemaphorePermit, broadcast, watch};
use tokio::time::Instant;

use crate::auth::{Grant, REAUTHORIZE_EVERY};
use crate::error::{ApiError, ApiQuery, ApiResult};
use crate::state::{LiveBatch, SharedState};

pub const MAX_EVENTS_PER_BATCH: usize = 200;

/// Reconnection delay suggested to clients when the server shuts down.
pub const RECONNECT_AFTER: Duration = Duration::from_secs(2);

#[derive(Deserialize)]
pub struct LiveParams {
    #[serde(default)]
    pub query: String,
    pub signal: Option<String>,
}

struct Tail {
    state: SharedState,
    rx: broadcast::Receiver<LiveBatch>,
    filter: Option<CExpr>,
    signal: Signal,
    pending: VecDeque<Event>,
    shutdown: watch::Receiver<bool>,
    /// The `shutdown`, `unauthorized` or `unavailable` event is queued;
    /// end once `pending` is sent.
    closing: bool,
    /// What admitted the stream; `None` once a recheck refused it.
    grant: Option<Grant>,
    /// When the grant must be rechecked even if nothing changed.
    recheck_at: Instant,
    _permit: OwnedSemaphorePermit,
}

impl Tail {
    fn close(&mut self) {
        self.closing = true;
        self.pending.push_back(Event::default().event("shutdown").retry(RECONNECT_AFTER).data("{}"));
    }

    /// Whether the grant must be rechecked before anything more is sent.
    fn recheck_due(&self) -> bool {
        self.grant.as_ref().is_some_and(|g| g.changed() || Instant::now() >= self.recheck_at)
    }

    /// Recheck the grant; on refusal, drop what is not yet sent and end.
    async fn reauthorize(&mut self) {
        let Some(grant) = self.grant.as_mut() else { return };
        match grant.reauthorize(&self.state).await {
            Ok(()) => self.recheck_at = Instant::now() + REAUTHORIZE_EVERY,
            Err(e) => self.refuse(e),
        }
    }

    fn refuse(&mut self, e: ApiError) {
        self.grant = None;
        self.pending.clear();
        self.closing = true;
        let data = serde_json::json!({ "error": { "code": e.code, "message": e.message } }).to_string();
        let ev = if matches!(e.status, StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN) {
            Event::default().event("unauthorized").data(data)
        } else {
            Event::default().event("unavailable").retry(RECONNECT_AFTER).data(data)
        };
        self.pending.push_back(ev);
    }
}

/// Resolves when the grant must be rechecked: a key or session changed, or
/// `at` passed. Never, once there is no grant.
async fn recheck(grant: &mut Option<Grant>, at: Instant) {
    match grant {
        Some(g) => tokio::select! {
            _ = g.changes() => {}
            _ = tokio::time::sleep_until(at) => {}
        },
        None => std::future::pending().await,
    }
}

pub async fn live(
    State(state): State<SharedState>,
    grant: Option<Extension<Grant>>,
    ApiQuery(p): ApiQuery<LiveParams>,
) -> ApiResult<Sse<impl Stream<Item = Result<Event, Infallible>>>> {
    // `auth::authorize` adds it to every query request; never stream without.
    let Some(Extension(grant)) = grant else {
        tracing::error!("live tail admitted without a grant; refused");
        return Err(ApiError::unauthorized("authentication required"));
    };
    let signal = match p.signal.as_deref().unwrap_or("logs") {
        "logs" => Signal::Logs,
        "traces" => Signal::Traces,
        "metrics" => Signal::Metrics,
        _ => return Err(ApiError::bad_request("signal must be logs, traces or metrics")),
    };
    let filter = query::compile_query(&p.query, signal)?;
    if state.draining() {
        return Err(ApiError::shutting_down());
    }
    let permit = state
        .live_permits
        .clone()
        .try_acquire_owned()
        .map_err(|_| ApiError::unavailable("too many live streams open"))?;
    let mut pending = VecDeque::new();
    pending.push_back(Event::default().event("ready").data("{}"));
    let tail = Tail {
        rx: state.live.subscribe(),
        shutdown: state.shutdown.clone(),
        state,
        filter,
        signal,
        pending,
        closing: false,
        grant: Some(grant),
        recheck_at: Instant::now() + REAUTHORIZE_EVERY,
        _permit: permit,
    };
    let stream = futures_util::stream::unfold(tail, |mut t| async move {
        loop {
            // Before every event: nothing is sent on a credential known to
            // have changed or due for its periodic recheck.
            if t.recheck_due() {
                t.reauthorize().await;
            }
            if let Some(ev) = t.pending.pop_front() {
                return Some((Ok(ev), t));
            }
            if t.closing {
                return None;
            }
            if *t.shutdown.borrow() {
                t.close();
                continue;
            }
            let next = tokio::select! {
                r = t.rx.recv() => r,
                _ = t.shutdown.changed() => {
                    t.close();
                    continue;
                }
                _ = recheck(&mut t.grant, t.recheck_at) => continue,
            };
            match next {
                Ok(lb) if lb.signal == t.signal => {
                    let mut matched = 0usize;
                    let mut dropped = 0usize;
                    for e in &lb.batch.events {
                        if t.filter.as_ref().is_none_or(|f| eval(f, e)) {
                            if matched < MAX_EVENTS_PER_BATCH {
                                if let Ok(ev) = Event::default().event("event").json_data(e) {
                                    t.pending.push_back(ev);
                                }
                                matched += 1;
                            } else {
                                dropped += 1;
                            }
                        }
                    }
                    if dropped > 0 {
                        t.pending.push_back(Event::default().event("dropped").data(dropped.to_string()));
                    }
                }
                Ok(_) => {}
                Err(RecvError::Lagged(n)) => {
                    t.pending.push_back(Event::default().event("lagged").data(n.to_string()));
                }
                Err(RecvError::Closed) => return None,
            }
        }
    });
    Ok(Sse::new(stream).keep_alive(KeepAlive::default()))
}
