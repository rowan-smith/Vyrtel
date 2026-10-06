//! Minimal alert evaluator: log-count threshold alerts with webhook delivery.
//!
//! Every few seconds, each enabled alert whose interval has elapsed is
//! evaluated: count log events matching its query over the trailing window
//! and compare with the threshold. Status transitions (OK ↔ FIRING, ERROR
//! when the query cannot run) are persisted with history, and each
//! transition is POSTed to the alert's webhook (if any).

use std::time::Duration;

use metadata::{Alert, AlertStatus, now_ms};
use query::TimeRange;
use serde_json::json;
use telemetry::{NANOS_PER_SEC, Signal, Timestamp};
use tokio::sync::watch;

use crate::error::ApiResult;
use crate::state::SharedState;

pub fn condition_holds(value: f64, op: &str, threshold: f64) -> bool {
    match op {
        ">" => value > threshold,
        ">=" => value >= threshold,
        "<" => value < threshold,
        "<=" => value <= threshold,
        "=" => value == threshold,
        "!=" => value != threshold,
        _ => false,
    }
}

pub struct Evaluation {
    pub status: AlertStatus,
    pub value: Option<f64>,
    pub error: Option<String>,
}

pub async fn evaluate(state: &SharedState, alert: &Alert) -> Evaluation {
    let to = Timestamp::now();
    let from = to.saturating_sub_nanos(alert.def.window_secs.saturating_mul(NANOS_PER_SEC));
    let q = alert.def.query.clone();
    let result = state.query(move |e| Ok(e.count(Signal::Logs, &q, TimeRange::new(Some(from), Some(to)))?)).await;
    match result {
        Ok((n, _)) => {
            let value = n as f64;
            Evaluation {
                status: if condition_holds(value, &alert.def.op, alert.def.threshold) {
                    AlertStatus::Firing
                } else {
                    AlertStatus::Ok
                },
                value: Some(value),
                error: None,
            }
        }
        Err(e) => Evaluation { status: AlertStatus::Error, value: None, error: Some(e.message) },
    }
}

/// Evaluate one alert now, persist the result and notify on transition.
pub async fn evaluate_and_record(state: &SharedState, alert: &Alert) -> ApiResult<Alert> {
    let ev = evaluate(state, alert).await;
    let id = alert.id;
    let (status, value, err) = (ev.status, ev.value, ev.error.clone());
    let prev = state.db(move |m| m.record_alert_evaluation(id, status, value, err.as_deref())).await?;
    if prev != ev.status {
        tracing::info!(alert = %alert.def.name, from = prev.as_str(), to = ev.status.as_str(), value = ?ev.value, "alert transition");
        if let Some(url) = alert.def.webhook_url.clone().filter(|u| !u.is_empty()) {
            let payload = json!({
                "source": "observer",
                "alert": {
                    "id": alert.id,
                    "name": alert.def.name,
                    "query": alert.def.query,
                    "condition": format!("count {} {}", alert.def.op, alert.def.threshold),
                    "windowSeconds": alert.def.window_secs,
                },
                "status": ev.status.as_str(),
                "previousStatus": prev.as_str(),
                "value": ev.value,
                "threshold": alert.def.threshold,
                "error": ev.error,
                "evaluatedAt": Timestamp::now(),
            });
            let st = state.clone();
            tokio::spawn(async move {
                let result = send_webhook(&st, &url, &payload).await;
                let err = result.err();
                if let Some(e) = &err {
                    tracing::warn!(alert = id, error = %e, "webhook delivery failed");
                }
                let _ = st.db(move |m| m.record_alert_notification(id, err.as_deref())).await;
            });
        }
    }
    state.db(move |m| m.get_alert(id)).await
}

/// POST with a timeout and a couple of retries for transient failures.
async fn send_webhook(state: &SharedState, url: &str, payload: &serde_json::Value) -> Result<(), String> {
    let mut last = String::new();
    for attempt in 0..3u32 {
        if attempt > 0 {
            tokio::time::sleep(Duration::from_secs(1 << attempt)).await;
        }
        match state.http.post(url).timeout(state.config.alerts.webhook_timeout).json(payload).send().await {
            Ok(r) if r.status().is_success() => return Ok(()),
            Ok(r) if r.status().is_client_error() && r.status().as_u16() != 429 => {
                return Err(format!("webhook returned {}", r.status()));
            }
            Ok(r) => last = format!("webhook returned {}", r.status()),
            Err(e) => last = format!("webhook request failed: {}", e.without_url()),
        }
    }
    Err(last)
}

fn due(a: &Alert, now: i64) -> bool {
    a.def.enabled && a.state.last_evaluated_at.is_none_or(|t| now - t >= a.def.interval_secs * 1000)
}

pub async fn run(state: SharedState, mut shutdown: watch::Receiver<bool>) {
    let mut tick = tokio::time::interval(Duration::from_secs(5));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            _ = tick.tick() => {}
            _ = shutdown.changed() => return,
        }
        let alerts = match state.db(|m| m.list_alerts()).await {
            Ok(a) => a,
            Err(e) => {
                tracing::warn!(error = %e.message, "could not load alerts");
                continue;
            }
        };
        let now = now_ms();
        for a in alerts.iter().filter(|a| due(a, now)) {
            if let Err(e) = evaluate_and_record(&state, a).await {
                tracing::warn!(alert = a.id, error = %e.message, "alert evaluation failed");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conditions() {
        assert!(condition_holds(101.0, ">", 100.0));
        assert!(!condition_holds(100.0, ">", 100.0));
        assert!(condition_holds(100.0, ">=", 100.0));
        assert!(condition_holds(0.0, "<", 1.0));
        assert!(condition_holds(1.0, "<=", 1.0));
        assert!(condition_holds(5.0, "=", 5.0));
        assert!(condition_holds(5.0, "!=", 4.0));
        assert!(!condition_holds(5.0, "??", 4.0));
    }
}
