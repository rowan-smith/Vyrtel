//! Query endpoints for logs, traces and metrics.

use axum::Json;
use axum::extract::State;
use query::{Cursor, Direction, MetricQuery, TimeRange};
use serde::Deserialize;
use serde_json::{Value as Json_, json};
use telemetry::{Signal, Timestamp};

use crate::error::{ApiError, ApiJson, ApiPath, ApiQuery, ApiResult};
use crate::state::SharedState;

/// Accepts RFC 3339 (`2026-10-06T10:00:00Z`), Unix numbers (s/ms/µs/ns),
/// `now`, and relative offsets from now (`-15m`, `now-1h`).
pub fn parse_time(v: Option<&Json_>, field: &str) -> ApiResult<Option<Timestamp>> {
    let Some(v) = v else { return Ok(None) };
    if v.is_null() {
        return Ok(None);
    }
    if let Some(s) = v.as_str() {
        let t = s.trim();
        if t.is_empty() {
            return Ok(None);
        }
        if t.eq_ignore_ascii_case("now") {
            return Ok(Some(Timestamp::now()));
        }
        let rel = t.strip_prefix("now").unwrap_or(t);
        if let Some(d) = rel.strip_prefix('-')
            && let Ok(d) = crate::config::parse_duration(d)
        {
            return Ok(Some(Timestamp::now().saturating_sub_nanos(d.as_nanos().min(i64::MAX as u128) as i64)));
        }
    }
    telemetry::parse_json_timestamp(v).map(Some).ok_or_else(|| {
        ApiError::bad_request(format!("'{field}' is not a valid time (use RFC 3339, a Unix timestamp, or e.g. '-1h')"))
    })
}

fn range(from: Option<&Json_>, to: Option<&Json_>) -> ApiResult<TimeRange> {
    let from = parse_time(from, "from")?;
    let to = parse_time(to, "to")?;
    if let (Some(f), Some(t)) = (from, to)
        && f >= t
    {
        return Err(ApiError::bad_request("'from' must be before 'to'"));
    }
    Ok(TimeRange::new(from, to))
}

/// Bounded range for aggregations; defaults to the last hour.
fn bounded(from: Option<&Json_>, to: Option<&Json_>) -> ApiResult<TimeRange> {
    let r = range(from, to)?;
    let to = if r.to == Timestamp::MAX { Timestamp::now().saturating_add_nanos(1) } else { r.to };
    let from =
        if r.from == Timestamp::MIN { to.saturating_sub_nanos(3_600 * telemetry::NANOS_PER_SEC) } else { r.from };
    if from >= to {
        return Err(ApiError::bad_request("'from' must be before 'to'"));
    }
    Ok(TimeRange { from, to })
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchBody {
    #[serde(default)]
    pub query: String,
    pub from: Option<Json_>,
    pub to: Option<Json_>,
    pub limit: Option<usize>,
    #[serde(default)]
    pub direction: Direction,
    pub continuation_token: Option<String>,
}

fn signal_of(s: &str) -> ApiResult<Signal> {
    match s {
        "logs" => Ok(Signal::Logs),
        "traces" | "spans" => Ok(Signal::Traces),
        "metrics" => Ok(Signal::Metrics),
        _ => Err(ApiError::not_found("signal")),
    }
}

async fn search(state: SharedState, signal: Signal, b: SearchBody) -> ApiResult<Json<Json_>> {
    let range = range(b.from.as_ref(), b.to.as_ref())?;
    let cursor = match b.continuation_token.as_deref().filter(|t| !t.is_empty()) {
        Some(t) => Some(Cursor::decode(t).ok_or_else(|| ApiError::bad_request("invalid continuationToken"))?),
        None => None,
    };
    let max = state.config.query.max_results;
    let limit = b.limit.unwrap_or(200);
    if limit == 0 || limit > max {
        return Err(ApiError::bad_request(format!("limit must be between 1 and {max}")));
    }
    let out = state.query(move |e| Ok(e.search(signal, &b.query, range, limit, b.direction, cursor)?)).await?;
    Ok(Json(json!({
        "events": out.events,
        "diagnostics": out.diagnostics,
        "continuationToken": out.next.map(|c| c.encode()),
    })))
}

pub async fn logs(State(state): State<SharedState>, ApiJson(b): ApiJson<SearchBody>) -> ApiResult<Json<Json_>> {
    search(state, Signal::Logs, b).await
}

/// `POST /api/v1/query/{signal}` for spans and metric points as raw events.
pub async fn events(
    State(state): State<SharedState>,
    ApiPath(signal): ApiPath<String>,
    ApiJson(b): ApiJson<SearchBody>,
) -> ApiResult<Json<Json_>> {
    search(state, signal_of(&signal)?, b).await
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AggBody {
    #[serde(default)]
    pub query: String,
    pub from: Option<Json_>,
    pub to: Option<Json_>,
    pub buckets: Option<usize>,
    pub sample: Option<usize>,
}

pub async fn histogram(
    State(state): State<SharedState>,
    ApiPath(signal): ApiPath<String>,
    ApiJson(b): ApiJson<AggBody>,
) -> ApiResult<Json<Json_>> {
    let signal = signal_of(&signal)?;
    let range = bounded(b.from.as_ref(), b.to.as_ref())?;
    let buckets = b.buckets.unwrap_or(60).clamp(1, 500);
    let (h, d) = state.query(move |e| Ok(e.histogram(signal, &b.query, range, buckets)?)).await?;
    Ok(Json(json!({
        "from": range.from,
        "to": range.to,
        "stepMs": h.step_ms,
        "total": h.total,
        "buckets": h.buckets,
        "diagnostics": d,
    })))
}

pub async fn count(
    State(state): State<SharedState>,
    ApiPath(signal): ApiPath<String>,
    ApiJson(b): ApiJson<AggBody>,
) -> ApiResult<Json<Json_>> {
    let signal = signal_of(&signal)?;
    let range = range(b.from.as_ref(), b.to.as_ref())?;
    let (n, d) = state.query(move |e| Ok(e.count(signal, &b.query, range)?)).await?;
    Ok(Json(json!({ "count": n, "diagnostics": d })))
}

pub async fn facets(
    State(state): State<SharedState>,
    ApiPath(signal): ApiPath<String>,
    ApiJson(b): ApiJson<AggBody>,
) -> ApiResult<Json<Json_>> {
    let signal = signal_of(&signal)?;
    let range = range(b.from.as_ref(), b.to.as_ref())?;
    let sample = b.sample.unwrap_or(2000).clamp(1, 10_000);
    let (f, d) = state.query(move |e| Ok(e.facets(signal, &b.query, range, sample)?)).await?;
    Ok(Json(json!({ "sampled": f.sampled, "fields": f.fields, "diagnostics": d })))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TraceSearchBody {
    #[serde(default)]
    pub query: String,
    pub from: Option<Json_>,
    pub to: Option<Json_>,
    pub limit: Option<usize>,
}

pub async fn trace_search(
    State(state): State<SharedState>,
    ApiJson(b): ApiJson<TraceSearchBody>,
) -> ApiResult<Json<Json_>> {
    let range = range(b.from.as_ref(), b.to.as_ref())?;
    let limit = b.limit.unwrap_or(50).clamp(1, 200);
    let (traces, d) = state.query(move |e| Ok(e.trace_search(&b.query, range, limit)?)).await?;
    Ok(Json(json!({ "traces": traces, "diagnostics": d })))
}

#[derive(Deserialize)]
pub struct TraceSearchParams {
    #[serde(default)]
    pub query: String,
    pub from: Option<String>,
    pub to: Option<String>,
    pub limit: Option<usize>,
}

pub async fn trace_search_get(
    State(state): State<SharedState>,
    ApiQuery(p): ApiQuery<TraceSearchParams>,
) -> ApiResult<Json<Json_>> {
    trace_search(
        State(state),
        ApiJson(TraceSearchBody {
            query: p.query,
            from: p.from.map(Json_::String),
            to: p.to.map(Json_::String),
            limit: p.limit,
        }),
    )
    .await
}

#[derive(Deserialize)]
pub struct RangeParams {
    pub from: Option<String>,
    pub to: Option<String>,
}

pub async fn trace_get(
    State(state): State<SharedState>,
    ApiPath(id): ApiPath<String>,
    ApiQuery(p): ApiQuery<RangeParams>,
) -> ApiResult<Json<Json_>> {
    let range = range(p.from.map(Json_::String).as_ref(), p.to.map(Json_::String).as_ref())?;
    let id2 = id.clone();
    let (spans, d) = state.query(move |e| Ok(e.trace(&id2, range)?)).await?;
    if spans.is_empty() {
        return Err(ApiError::not_found("trace"));
    }
    let tid = telemetry::normalize_id(id.trim());
    let summary = query::traces::summarize(&tid, &spans);
    Ok(Json(json!({ "traceId": tid, "summary": summary, "spans": spans, "diagnostics": d })))
}

pub async fn metric_names(State(state): State<SharedState>) -> ApiResult<Json<Json_>> {
    let names = state.query(|e| Ok(e.metric_names())).await?;
    Ok(Json(json!({ "metrics": names })))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MetricBody {
    pub name: String,
    pub query: Option<String>,
    pub from: Option<Json_>,
    pub to: Option<Json_>,
    pub step_ms: Option<i64>,
    #[serde(default)]
    pub agg: query::aggregate::Agg,
    pub group_by: Option<String>,
}

pub async fn metric_query(State(state): State<SharedState>, ApiJson(b): ApiJson<MetricBody>) -> ApiResult<Json<Json_>> {
    let range = bounded(b.from.as_ref(), b.to.as_ref())?;
    let q = MetricQuery {
        name: b.name,
        filter: b.query.filter(|q| !q.trim().is_empty()),
        range,
        step_ms: b.step_ms,
        agg: b.agg,
        group_by: b.group_by,
    };
    let out = state.query(move |e| Ok(e.metric_query(&q)?)).await?;
    Ok(Json(serde_json::to_value(out).map_err(ApiError::internal)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn time_inputs() {
        let t = parse_time(Some(&json!("2026-10-06T10:00:00Z")), "from").unwrap().unwrap();
        assert_eq!(t.to_rfc3339(), "2026-10-06T10:00:00Z");
        let now = Timestamp::now();
        let rel = parse_time(Some(&json!("-1h")), "from").unwrap().unwrap();
        assert!((now.0 - rel.0 - 3_600_000_000_000).abs() < 5_000_000_000);
        let rel2 = parse_time(Some(&json!("now-15m")), "from").unwrap().unwrap();
        assert!(rel2 > rel);
        assert!(parse_time(Some(&json!(1_791_282_013_000i64)), "from").unwrap().is_some());
        assert!(parse_time(Some(&json!("garbage")), "from").is_err());
        assert_eq!(parse_time(None, "from").unwrap(), None);
        assert!(range(Some(&json!("2026-01-02T00:00:00Z")), Some(&json!("2026-01-01T00:00:00Z"))).is_err());
    }
}
