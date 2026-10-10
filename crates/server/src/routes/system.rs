//! Health, readiness, system information and storage statistics.

use std::sync::atomic::Ordering;

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};
use storage::{RecoveryReport, StreamStats};
use telemetry::Signal;

use crate::error::ApiResult;
use crate::state::SharedState;

pub async fn health() -> Json<Value> {
    Json(json!({ "status": "ok" }))
}

/// Ready when every storage stream is healthy. Unauthenticated, so it
/// reports only a status, never error details.
pub async fn ready(State(state): State<SharedState>) -> Response {
    let stats = state.storage.stats();
    let degraded: Vec<&str> =
        Signal::ALL.iter().filter(|s| stats.get(**s).last_error.is_some()).map(|s| s.as_str()).collect();
    if degraded.is_empty() {
        Json(json!({ "status": "ready" })).into_response()
    } else {
        (StatusCode::SERVICE_UNAVAILABLE, Json(json!({ "status": "degraded", "signals": degraded }))).into_response()
    }
}

fn stream_json(s: &StreamStats) -> Value {
    let compressed = s.segment_data_bytes + s.segment_index_bytes;
    json!({
        "events": s.segment_events + s.active_events + s.frozen_events,
        "segments": s.segments,
        "rawBytes": s.segment_raw_bytes + s.wal_bytes,
        "storedBytes": compressed + s.wal_bytes,
        "segmentBytes": s.segment_data_bytes,
        "indexBytes": s.segment_index_bytes,
        "walBytes": s.wal_bytes,
        "compressionRatio": ratio(s.segment_raw_bytes, compressed),
        "activeEvents": s.active_events + s.frozen_events,
        "activeBytes": s.active_bytes,
        "queueDepth": s.queue_depth,
        "queueCapacity": s.queue_capacity,
        "ingestedSinceStart": s.ingested_events,
        "rejectedBatches": s.rejected_batches,
        "oldest": s.oldest,
        "newest": s.newest,
        "healthy": s.last_error.is_none(),
        "recovery": recovery_json(&s.recovery),
    })
}

/// What startup recovery did for one signal. Quarantine paths are relative
/// to the data directory so the response never discloses host paths.
fn recovery_json(r: &RecoveryReport) -> Value {
    json!({
        "segmentsLoaded": r.segments_loaded,
        "walRecordsReplayed": r.wal_records_replayed,
        "walEventsReplayed": r.wal_events_replayed,
        "segmentsSealed": r.segments_sealed,
        "staleWalsRemoved": r.stale_wals_removed,
        "replacedSegmentsRemoved": r.replaced_segments_removed,
        "walBytesDiscarded": r.wal_bytes_discarded,
        "quarantined": r.quarantined.iter().map(|q| json!({
            "signal": q.signal.as_str(),
            "kind": q.kind.as_str(),
            "source": q.source,
            "file": q.path.file_name().map(|n| format!("quarantine/{}/{}", q.signal.as_str(), n.to_string_lossy())),
            "bytes": q.bytes,
            "reason": q.reason,
        })).collect::<Vec<_>>(),
    })
}

fn ratio(raw: u64, stored: u64) -> Option<f64> {
    (stored > 0).then(|| (raw as f64 / stored as f64 * 100.0).round() / 100.0)
}

fn metadata_bytes(state: &SharedState) -> u64 {
    storage::fsutil::dir_size(&state.config.storage.path.join("metadata"))
}

pub async fn storage_stats(State(state): State<SharedState>) -> ApiResult<Json<Value>> {
    let st = state.clone();
    let (stats, meta) = state.blocking(move || Ok((st.storage.stats(), metadata_bytes(&st)))).await?;
    let all = [&stats.logs, &stats.traces, &stats.metrics];
    let sum = |f: fn(&StreamStats) -> u64| all.iter().map(|s| f(s)).sum::<u64>();
    let raw_sealed = sum(|s| s.segment_raw_bytes);
    let data = sum(|s| s.segment_data_bytes);
    let index = sum(|s| s.segment_index_bytes);
    let wal = sum(|s| s.wal_bytes);
    Ok(Json(json!({
        "rawBytes": raw_sealed + wal,
        "storedBytes": data + index + wal + meta,
        "segmentBytes": data,
        "indexBytes": index,
        "walBytes": wal,
        "metadataBytes": meta,
        "tmpBytes": stats.tmp_bytes,
        "quarantineBytes": stats.quarantine_bytes,
        "eventCount": sum(|s| s.segment_events + s.active_events + s.frozen_events),
        "segmentCount": all.iter().map(|s| s.segments).sum::<usize>(),
        "compressionRatio": ratio(raw_sealed, data + index),
        "indexOverhead": (data > 0).then(|| ((index as f64 / data as f64) * 1000.0).round() / 1000.0),
        "receivedBytes": state.received_bytes.load(Ordering::Relaxed),
        "signals": {
            "logs": stream_json(&stats.logs),
            "traces": stream_json(&stats.traces),
            "metrics": stream_json(&stats.metrics),
        },
        "indexCache": {
            "budgetBytes": stats.cache.budget_bytes,
            "usedBytes": stats.cache.used_bytes,
            "entries": stats.cache.entries,
            "hits": stats.cache.hits,
            "misses": stats.cache.misses,
        },
        "notes": "rawBytes is the uncompressed size in Vyrtel's binary event encoding (sealed segments plus WAL).",
    })))
}

/// Resident set size of this process, where the OS makes it cheap to read.
pub fn process_rss() -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        let s = std::fs::read_to_string("/proc/self/status").ok()?;
        let line = s.lines().find(|l| l.starts_with("VmRSS:"))?;
        let kb: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
        Some(kb * 1024)
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

pub async fn info(State(state): State<SharedState>) -> ApiResult<Json<Value>> {
    let st = state.clone();
    let stats = state.blocking(move || Ok(st.storage.stats())).await?;
    let queue = |s: &StreamStats| json!({ "depth": s.queue_depth, "capacity": s.queue_capacity });
    let active = |s: &StreamStats| json!({ "events": s.active_events + s.frozen_events, "bytes": s.active_bytes });
    let b = &state.budgets;
    let storage_bytes: u64 = [&stats.logs, &stats.traces, &stats.metrics]
        .iter()
        .map(|s| s.segment_data_bytes + s.segment_index_bytes + s.wal_bytes)
        .sum();
    Ok(Json(json!({
        "name": "Vyrtel",
        "version": env!("CARGO_PKG_VERSION"),
        "startedAt": state.started_ts,
        "uptimeSeconds": state.started_at.elapsed().as_secs(),
        "dataDir": state.config.storage.path,
        "durability": state.config.storage.durability,
        "authEnabled": state.config.auth.enabled,
        "eventCounts": {
            "logs": stats.logs.segment_events + stats.logs.active_events + stats.logs.frozen_events,
            "traces": stats.traces.segment_events + stats.traces.active_events + stats.traces.frozen_events,
            "metrics": stats.metrics.segment_events + stats.metrics.active_events + stats.metrics.frozen_events,
        },
        "storageBytes": storage_bytes,
        "queue": { "logs": queue(&stats.logs), "traces": queue(&stats.traces), "metrics": queue(&stats.metrics) },
        "activeSegment": { "logs": active(&stats.logs), "traces": active(&stats.traces), "metrics": active(&stats.metrics) },
        "memory": {
            "limitBytes": b.total,
            "rssBytes": process_rss(),
            "budgets": b,
            "indexCacheUsedBytes": stats.cache.used_bytes,
        },
        "retention": state.config.retention,
        "receivedRequests": state.received_requests.load(Ordering::Relaxed),
        "liveSubscribers": state.live.receiver_count(),
    })))
}

pub async fn query_stats(State(state): State<SharedState>) -> ApiResult<Json<Value>> {
    let stored = state.db(|m| m.query_stats()).await?;
    let pending = state.engine.stats().pending();
    // Merge persisted totals with not-yet-flushed increments.
    let mut rows: Vec<metadata::StoredQueryStats> = stored;
    for p in pending {
        if let Some(r) = rows.iter_mut().find(|r| r.signal == p.signal && r.field == p.field) {
            r.queries += p.queries;
            r.bytes_read += p.bytes_read;
            r.segments_scanned += p.segments_scanned;
            r.segments_skipped += p.segments_skipped;
            r.total_ms += p.total_ms;
            r.index = p.index;
        } else {
            rows.push(metadata::StoredQueryStats {
                signal: p.signal,
                field: p.field,
                index: p.index,
                queries: p.queries,
                bytes_read: p.bytes_read,
                segments_scanned: p.segments_scanned,
                segments_skipped: p.segments_skipped,
                total_ms: p.total_ms,
                last_seen: metadata::now_ms(),
            });
        }
    }
    rows.sort_by(|a, b| b.queries.cmp(&a.queries).then(a.field.cmp(&b.field)));
    Ok(Json(json!({ "fields": rows })))
}

/// Effective configuration (secrets omitted).
pub async fn config(State(state): State<SharedState>) -> Json<Value> {
    Json(json!({ "config": state.config, "budgets": state.budgets }))
}
