//! Live tail over Server-Sent Events.
//!
//! The storage writer publishes every committed batch on a bounded
//! broadcast channel. Each SSE connection filters batches with its own
//! compiled query. Backpressure: the channel keeps a fixed number of recent
//! batches; a client that falls behind receives a `lagged` event with the
//! number of skipped batches instead of making the server buffer for it.
//! Per batch, at most [`MAX_EVENTS_PER_BATCH`] matches are sent.

use std::collections::VecDeque;
use std::convert::Infallible;

use axum::extract::State;
use axum::response::sse::{Event, KeepAlive, Sse};
use futures_util::Stream;
use query::eval::{CExpr, eval};
use serde::Deserialize;
use telemetry::Signal;
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::{OwnedSemaphorePermit, broadcast, watch};

use crate::error::{ApiError, ApiQuery, ApiResult};
use crate::state::{LiveBatch, SharedState};

pub const MAX_EVENTS_PER_BATCH: usize = 200;

#[derive(Deserialize)]
pub struct LiveParams {
    #[serde(default)]
    pub query: String,
    pub signal: Option<String>,
}

struct Tail {
    rx: broadcast::Receiver<LiveBatch>,
    filter: Option<CExpr>,
    signal: Signal,
    pending: VecDeque<Event>,
    shutdown: watch::Receiver<bool>,
    _permit: OwnedSemaphorePermit,
}

pub async fn live(
    State(state): State<SharedState>,
    ApiQuery(p): ApiQuery<LiveParams>,
) -> ApiResult<Sse<impl Stream<Item = Result<Event, Infallible>>>> {
    let signal = match p.signal.as_deref().unwrap_or("logs") {
        "logs" => Signal::Logs,
        "traces" | "spans" => Signal::Traces,
        "metrics" => Signal::Metrics,
        _ => return Err(ApiError::bad_request("signal must be logs, traces or metrics")),
    };
    let filter = query::compile_query(&p.query, signal)?;
    let permit = state
        .live_permits
        .clone()
        .try_acquire_owned()
        .map_err(|_| ApiError::unavailable("too many live streams open"))?;
    let mut pending = VecDeque::new();
    pending.push_back(Event::default().event("ready").data("{}"));
    let tail =
        Tail { rx: state.live.subscribe(), filter, signal, pending, shutdown: state.shutdown.clone(), _permit: permit };
    let stream = futures_util::stream::unfold(tail, |mut t| async move {
        loop {
            if let Some(ev) = t.pending.pop_front() {
                return Some((Ok(ev), t));
            }
            if *t.shutdown.borrow() {
                return None;
            }
            let next = tokio::select! {
                r = t.rx.recv() => r,
                _ = t.shutdown.changed() => return None,
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
