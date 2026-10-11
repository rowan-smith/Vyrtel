//! REL-04 end to end: storage write failures injected below a real server
//! (`--features fault-injection`). Ingest must answer 503 and store
//! nothing, readiness must turn degraded, diagnostics must say what failed
//! and where without disclosing host paths, and committed data must survive
//! a restart.

use std::time::Duration;

use serde_json::{Value, json};
use storage::fault::inject::{Injected, inject};

use crate::common::*;

async fn ready(s: &TestServer) -> (u16, Value) {
    let r = s.client.get(s.u("/ready")).send().await.unwrap();
    (r.status().as_u16(), r.json().await.unwrap())
}

/// Check a 503 `storage_write_failed` response and return its body.
async fn assert_write_failed(r: reqwest::Response, reason: &str, host: &str) -> Value {
    assert_eq!(r.status(), 503);
    assert_eq!(r.headers()["retry-after"], "1");
    let text = r.text().await.unwrap();
    assert!(!text.contains(host), "response leaks the data directory: {text}");
    let body: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(body["error"]["code"], "storage_write_failed", "{body}");
    assert_eq!(body["error"]["reason"], reason, "{body}");
    body
}

/// A rejected append never reached the WAL whole: nothing was stored.
async fn assert_not_stored(r: reqwest::Response, reason: &str, host: &str) -> Value {
    let body = assert_write_failed(r, reason, host).await;
    assert_eq!(body["error"]["outcome"], "notStored", "{body}");
    body
}

#[tokio::test]
async fn ingest_is_rejected_while_the_wal_cannot_write() {
    let s = TestServer::start().await;
    let host = s.data_dir().to_string_lossy().to_string();
    s.ingest(json!({"message": "before"})).await;

    let guard = inject(&s.data_dir(), "wal.append", Injected::NoSpace);
    let r = s.client.post(s.u("/api/v1/events")).json(&json!({"message": "rejected"})).send().await.unwrap();
    let body = assert_not_stored(r, "noSpace", &host).await;
    assert!(body["error"]["message"].as_str().unwrap().contains("no space left"), "{body}");
    assert_not_stored(s.ingest_ndjson(fixture("logs.ndjson")).await, "noSpace", &host).await;
    assert_not_stored(s.otlp_json("/v1/logs", fixture("otlp-logs.json")).await, "noSpace", &host).await;
    assert_not_stored(s.otlp_json("/v1/traces", fixture("otlp-traces.json")).await, "noSpace", &host).await;
    assert_not_stored(s.otlp_json("/v1/metrics", fixture("otlp-metrics.json")).await, "noSpace", &host).await;
    assert_eq!(s.messages("").await, ["before"], "nothing rejected was stored");

    // Readiness names the affected signals; the probe itself still writes.
    let (status, body) = ready(&s).await;
    assert_eq!(status, 503, "{body}");
    assert_eq!(body, json!({"status": "degraded", "storageWritable": true, "signals": ["logs", "traces", "metrics"]}));

    // Diagnostics say what failed and where, relative to the data directory.
    let r = s.client.get(s.u("/api/v1/system/storage")).send().await.unwrap();
    let text = r.text().await.unwrap();
    assert!(!text.contains(&host), "diagnostics leak the data directory: {text}");
    let storage: Value = serde_json::from_str(&text).unwrap();
    let logs = &storage["signals"]["logs"];
    assert_eq!(logs["healthy"], false);
    assert_eq!(logs["ingestHealthy"], false);
    let f = &logs["faults"][0];
    assert_eq!(f["area"], "wal", "{f}");
    assert_eq!(f["operation"], "walAppend");
    assert_eq!(f["reason"], "noSpace");
    assert!(f["path"].as_str().unwrap().starts_with("wal/logs/"), "{f}");
    assert!(f["occurrences"].as_u64().unwrap() >= 3, "{f}");
    assert!(f["firstSeen"].is_string() && f["lastSeen"].is_string(), "{f}");
    assert_eq!(storage["writeProbe"], json!({"writable": true, "fault": null}));

    // Storage recovers: ingest succeeds and readiness clears without a restart.
    drop(guard);
    for path in ["/v1/logs", "/v1/traces", "/v1/metrics"] {
        let name = format!("otlp-{}.json", path.trim_start_matches("/v1/"));
        let r = s.otlp_json(path, fixture(&name)).await;
        assert_eq!(r.status(), 200, "{path}");
    }
    let (status, body) = ready(&s).await;
    assert_eq!((status, body), (200, json!({"status": "ready", "storageWritable": true})));
    assert_eq!(s.get_ok("/api/v1/system/storage").await["signals"]["logs"]["faults"], json!([]));
    s.stop().await;
}

#[tokio::test]
async fn readiness_reports_a_full_or_read_only_data_directory() {
    for (fault, reason) in [(Injected::NoSpace, "noSpace"), (Injected::ReadOnly, "readOnly")] {
        let s = TestServer::start().await;
        let guard = inject(&s.data_dir(), "probe", fault);
        let (status, body) = ready(&s).await;
        assert_eq!(status, 503);
        // Unauthenticated: which signals, never why or where.
        assert_eq!(
            body,
            json!({"status": "degraded", "storageWritable": false, "signals": ["logs", "traces", "metrics"]})
        );

        let storage = s.get_ok("/api/v1/system/storage").await;
        let probe = &storage["writeProbe"];
        assert_eq!(probe["writable"], false, "{probe}");
        assert_eq!(probe["fault"]["operation"], "probe");
        assert_eq!(probe["fault"]["reason"], reason);
        assert_eq!(probe["fault"]["path"], "tmp/.write-probe");

        drop(guard);
        // Probe results are reused for a second.
        tokio::time::sleep(Duration::from_millis(1100)).await;
        assert_eq!(ready(&s).await.0, 200);
        s.stop().await;
    }
}

#[tokio::test]
async fn committed_data_is_queryable_after_a_failed_write_and_restart() {
    let s = TestServer::start_with(|c| c.storage.durability = server::config::Durability::Strict).await;
    let host = s.data_dir().to_string_lossy().to_string();
    // A known dataset, partly sealed into a segment, partly still in the WAL.
    assert_eq!(s.ingest_ndjson(fixture("logs.ndjson")).await.status(), 200);
    s.rotate_all();
    s.ingest(json!({"message": "in the wal"})).await;
    let before = s.messages("").await;
    assert_eq!(before.len(), 9);

    {
        let _short = inject(&s.data_dir(), "wal.append", Injected::ShortWrite);
        let r = s.client.post(s.u("/api/v1/events")).json(&json!({"message": "short write"})).send().await.unwrap();
        assert_not_stored(r, "shortWrite", &host).await;
    }
    {
        let _denied = inject(&s.data_dir(), "wal.sync", Injected::PermissionDenied);
        let r = s.client.post(s.u("/api/v1/events")).json(&json!({"message": "fsync denied"})).send().await.unwrap();
        // The rollback's own fsync is denied too, so it cannot be confirmed
        // durable: the client is told the outcome is unknown.
        let body = assert_write_failed(r, "permissionDenied", &host).await;
        assert_eq!(body["error"]["outcome"], "unknown", "{body}");
        assert!(body["error"]["message"].as_str().unwrap().contains("may still be stored"), "{body}");
    }

    let s = s.restart().await;
    assert_eq!(s.messages("").await, before);
    let recovery = &s.get_ok("/api/v1/system/storage").await["signals"]["logs"]["recovery"];
    assert_eq!(recovery["quarantined"], json!([]), "{recovery}");
    assert_eq!(recovery["walBytesDiscarded"], 0, "{recovery}");
    assert_eq!(ready(&s).await.0, 200);
    s.stop().await;
}

#[tokio::test]
async fn readiness_recovers_without_new_traffic() {
    let s = TestServer::start().await;
    let host = s.data_dir().to_string_lossy().to_string();
    {
        let _full = inject(&s.data_dir(), "wal.append", Injected::NoSpace);
        let r = s.client.post(s.u("/api/v1/events")).json(&json!({"message": "rejected"})).send().await.unwrap();
        assert_not_stored(r, "noSpace", &host).await;
        assert_eq!(ready(&s).await.0, 503);
    }
    // An orchestrator sends no traffic to an unready instance, so readiness
    // must come back on its own once storage works.
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while ready(&s).await.0 != 200 {
        assert!(std::time::Instant::now() < deadline, "readiness never recovered");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    s.stop().await;
}
