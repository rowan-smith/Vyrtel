//! REL-11: storage recovery health as operators see it through the API.
//! Damage is real (a torn WAL record, a segment left in quarantine by an
//! interrupted recovery); nothing is injected into the server.

use std::path::Path;

use serde_json::{Value, json};

use crate::common::*;

const PASSWORD: &str = "correct horse";
/// Written into the torn WAL record; it must never come back over the API.
const SECRET: &str = "card=4111111111111111 token=sk_live_rel11";

fn signal_status(storage: &Value, signal: &str) -> String {
    storage["signals"][signal]["recovery"]["status"].as_str().unwrap().to_string()
}

/// `/health` and `/ready` as an anonymous client sees them.
async fn public_probes(s: &TestServer) -> (Value, Value) {
    let get = |path: &'static str| async move {
        let r = s.client.get(s.u(path)).send().await.unwrap();
        assert_eq!(r.status(), 200, "{path}");
        r.json::<Value>().await.unwrap()
    };
    (get("/health").await, get("/ready").await)
}

fn truncate_wal(data_dir: &Path, signal: &str, by: u64) -> String {
    let wal = std::fs::read_dir(data_dir.join("wal").join(signal)).unwrap().flatten().map(|e| e.path()).next().unwrap();
    let len = std::fs::metadata(&wal).unwrap().len();
    std::fs::OpenOptions::new().write(true).open(&wal).unwrap().set_len(len - by).unwrap();
    wal.file_name().unwrap().to_string_lossy().into_owned()
}

#[tokio::test(flavor = "multi_thread")]
async fn clean_restart_does_not_suggest_damage() {
    let s = TestServer::start().await;
    s.ingest(json!([{"message": "kept"}])).await;
    let s = s.restart().await;

    let storage = s.get_ok("/api/v1/system/storage").await;
    assert_eq!(storage["recovery"], json!({ "status": "clean", "signals": [] }), "{storage}");
    for signal in ["logs", "traces", "metrics"] {
        let r = &storage["signals"][signal]["recovery"];
        assert_eq!(r["status"], "clean", "{signal}: {r}");
        assert_eq!(r["walBytesDiscarded"], 0, "{signal}");
        assert_eq!(r["quarantinedBytes"], 0, "{signal}");
        assert_eq!(r["quarantineDir"], json!({ "files": 0, "bytes": 0 }), "{signal}");
        assert_eq!(r["quarantined"], json!([]), "{signal}");
    }
    // The WAL was replayed, which is normal and not reported as damage.
    assert!(storage["signals"]["logs"]["recovery"]["walFilesReplayed"].as_u64().unwrap() >= 1);
    assert_eq!(storage["quarantineBytes"], 0);
    assert_eq!(s.get_ok("/api/v1/system/info").await["recovery"], json!({ "status": "clean", "signals": [] }));
    s.stop().await;
}

/// A torn WAL record is reported per signal with counts and discarded
/// bytes; the API never returns its contents, credentials or host paths;
/// the public probes do not change; and once the operator has removed the
/// quarantined files, a restart is clean again.
#[tokio::test(flavor = "multi_thread")]
async fn torn_wal_tail_is_reported_until_the_quarantine_is_cleared() {
    let s = TestServer::start_with(|c| {
        c.auth.enabled = true;
        c.auth.admin_password = Some(PASSWORD.into());
    })
    .await;
    let admin = Admin::login(&s, PASSWORD).await;
    let api_key = admin.post("/api/v1/api-keys", json!({"name": "rel11"})).await["key"].as_str().unwrap().to_string();
    for message in ["committed", SECRET] {
        let r = s.client.post(s.u("/api/v1/events")).bearer_auth(&api_key).json(&json!({ "message": message })).send();
        assert!(r.await.unwrap().status().is_success());
    }
    let clean_probes = public_probes(&s).await;
    let (config, dir) = s.stop().await;
    let data_dir = config.storage.path.clone();

    as_after_crash(&data_dir);
    let wal = truncate_wal(&data_dir, "logs", 5);

    let s = TestServer::launch(config, dir).await;
    let admin = Admin::login(&s, PASSWORD).await;
    let storage = admin.get("/api/v1/system/storage").await;
    let logs = &storage["signals"]["logs"]["recovery"];
    assert_eq!(logs["status"], "damaged", "{logs}");
    assert_eq!(logs["walTailsQuarantined"], 1);
    assert_eq!(logs["walFilesQuarantined"], 0);
    assert_eq!(logs["segmentsQuarantined"], 0);
    let discarded = logs["walBytesDiscarded"].as_u64().unwrap();
    assert!(discarded > 0, "{logs}");
    assert_eq!(logs["quarantinedBytes"], discarded);
    assert_eq!(logs["quarantineDir"], json!({ "files": 1, "bytes": discarded }));
    assert_eq!(logs["quarantined"][0]["source"], wal);
    assert!(logs["quarantined"][0]["reason"].as_str().unwrap().starts_with("incomplete record"));
    for signal in ["traces", "metrics"] {
        assert_eq!(signal_status(&storage, signal), "clean", "{signal}");
    }
    assert_eq!(storage["recovery"], json!({ "status": "damaged", "signals": ["logs"] }));
    let info = admin.get("/api/v1/system/info").await;
    assert_eq!(info["recovery"], json!({ "status": "damaged", "signals": ["logs"] }));

    // Diagnostics name the damage, never what was in it or how to sign in.
    // (`/api/v1/system/info` reports the configured data directory by design.)
    let quarantined = std::fs::read_dir(data_dir.join("quarantine").join("logs")).unwrap().flatten().next().unwrap();
    let evidence = std::fs::read(quarantined.path()).unwrap();
    let storage_text = storage.to_string();
    for body in [&storage_text, &info.to_string()] {
        assert!(!body.contains(SECRET) && !body.contains("4111111111111111"), "telemetry leaked: {body}");
        assert!(!body.contains(PASSWORD) && !body.contains(&api_key), "credentials leaked: {body}");
    }
    assert!(!storage_text.contains(&*data_dir.to_string_lossy()), "host path leaked: {storage_text}");
    assert!(!storage_text.contains(&String::from_utf8_lossy(&evidence).into_owned()), "quarantined bytes leaked");
    // The details need a signed-in user; the public probes are unchanged.
    let anon = s.client.get(s.u("/api/v1/system/storage")).send().await.unwrap();
    assert_eq!(anon.status(), 401);
    assert_eq!(public_probes(&s).await, clean_probes, "damage does not change /health or /ready");

    // A later start found nothing new, but the evidence is still there.
    let s = s.restart().await;
    let admin = Admin::login(&s, PASSWORD).await;
    let storage = admin.get("/api/v1/system/storage").await;
    let logs = &storage["signals"]["logs"]["recovery"];
    assert_eq!(logs["status"], "quarantineRetained", "{logs}");
    assert_eq!(logs["walBytesDiscarded"], 0);
    assert_eq!(logs["quarantined"], json!([]));
    assert_eq!(logs["quarantineDir"], json!({ "files": 1, "bytes": discarded }));
    assert_eq!(storage["recovery"], json!({ "status": "quarantineRetained", "signals": ["logs"] }));
    assert_eq!(public_probes(&s).await, clean_probes);

    // The operator copies the evidence off and deletes it: clean again.
    let (config, dir) = s.stop().await;
    std::fs::remove_dir_all(data_dir.join("quarantine")).unwrap();
    let s = TestServer::launch(config, dir).await;
    let admin = Admin::login(&s, PASSWORD).await;
    let storage = admin.get("/api/v1/system/storage").await;
    assert_eq!(storage["recovery"], json!({ "status": "clean", "signals": [] }), "{storage}");
    assert_eq!(signal_status(&storage, "logs"), "clean");
    assert_eq!(storage["quarantineBytes"], 0);
    assert_eq!(public_probes(&s).await, clean_probes);
    assert_eq!(messages(&admin).await, ["committed"]);
    s.stop().await;
}

/// Recovery fsyncs a damaged file into `quarantine/<signal>/` before it
/// carries on, so a crash during recovery leaves the evidence there but no
/// report of it. The next start must still point at the affected signal.
#[tokio::test(flavor = "multi_thread")]
async fn evidence_from_an_interrupted_recovery_is_reported() {
    let s = TestServer::start().await;
    s.otlp_json("/v1/traces", fixture("otlp-traces.json")).await.error_for_status().unwrap();
    s.ingest(json!([{"message": "unaffected"}])).await;
    s.rotate_all();
    let (config, dir) = s.stop().await;
    let data_dir = config.storage.path.clone();

    // What recovery had done to a damaged segment when the process died:
    // moved it to quarantine under a timestamped name.
    let seg_dir = data_dir.join("segments").join("traces");
    let seg = std::fs::read_dir(&seg_dir).unwrap().flatten().map(|e| e.path()).next().unwrap();
    let bytes = std::fs::metadata(&seg).unwrap().len();
    let q = data_dir.join("quarantine").join("traces");
    std::fs::create_dir_all(&q).unwrap();
    std::fs::rename(&seg, q.join(format!("{}.1791622413000", seg.file_name().unwrap().to_string_lossy()))).unwrap();
    as_after_crash(&data_dir);

    let s = TestServer::launch(config, dir).await;
    let storage = s.get_ok("/api/v1/system/storage").await;
    let traces = &storage["signals"]["traces"]["recovery"];
    assert_eq!(traces["status"], "quarantineRetained", "{traces}");
    assert_eq!(traces["quarantineDir"], json!({ "files": 1, "bytes": bytes }));
    assert_eq!(traces["quarantined"], json!([]), "this start moved nothing");
    assert_eq!(storage["recovery"], json!({ "status": "quarantineRetained", "signals": ["traces"] }));
    assert_eq!(signal_status(&storage, "logs"), "clean");
    assert_eq!(signal_status(&storage, "metrics"), "clean");
    assert_eq!(storage["quarantineBytes"], bytes);
    assert_eq!(s.messages("").await, ["unaffected"]);
    s.stop().await;
}

/// Log messages as a signed-in user sees them.
async fn messages(admin: &Admin) -> Vec<String> {
    let v = admin.post("/api/v1/query/logs", json!({ "query": "", "limit": 1000 })).await;
    let mut m: Vec<String> =
        v["events"].as_array().unwrap().iter().map(|e| e["message"].as_str().unwrap().to_string()).collect();
    m.sort();
    m
}
