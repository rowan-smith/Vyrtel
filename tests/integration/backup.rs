//! REL-06: back up a stopped server's data directory, restore it on a clean
//! host, and refuse backups that are incomplete or from a newer Vyrtel
//! without changing them. The verification steps are the ones in
//! docs/configuration.md#backup-and-restore.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use server::App;
use server::config::Config;

use crate::common::*;

const PASSWORD: &str = "correct horse";

/// Copy a directory tree, as `cp -a` or `tar` would.
fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap().flatten() {
        let dest = to.join(e.file_name());
        if e.file_type().unwrap().is_dir() {
            copy_tree(&e.path(), &dest);
        } else {
            std::fs::copy(e.path(), dest).unwrap();
        }
    }
}

/// Every file and directory under `dir`, with file contents.
fn snapshot(dir: &Path) -> BTreeMap<PathBuf, Option<Vec<u8>>> {
    fn walk(root: &Path, d: &Path, out: &mut BTreeMap<PathBuf, Option<Vec<u8>>>) {
        for e in std::fs::read_dir(d).unwrap().flatten() {
            let p = e.path();
            let rel = p.strip_prefix(root).unwrap().to_path_buf();
            if p.is_dir() {
                out.insert(rel, None);
                walk(root, &p, out);
            } else if rel == Path::new(".lock") {
                // Empty, and unreadable on Windows while someone holds it.
                out.insert(rel, Some(Vec::new()));
            } else {
                out.insert(rel, Some(std::fs::read(&p).unwrap()));
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(dir, dir, &mut out);
    out
}

fn secure(dir: &Path) -> Config {
    let mut c = base_config(dir);
    c.auth.enabled = true;
    c.auth.admin_password = Some(PASSWORD.into());
    c
}

/// Ingest with an API key; returns the status code.
async fn ingest_with(s: &TestServer, key: Option<&str>, path: &str, content_type: &str, body: String) -> u16 {
    let mut r = s.client.post(s.u(path)).header("content-type", content_type).body(body);
    if let Some(k) = key {
        r = r.bearer_auth(k);
    }
    r.send().await.unwrap().status().as_u16()
}

/// A stopped, populated instance: every signal (in both segments and the
/// WAL), a dashboard with panels, an alert, a saved query, an active and a
/// revoked API key, an admin session. Returns its directory, the active key,
/// the revoked key, the session cookie and the verification answers.
async fn populated() -> (tempfile::TempDir, String, String, String, Value) {
    let dir = tempfile::tempdir().unwrap();
    let s = TestServer::launch(secure(dir.path()), None).await;
    let a = Admin::login(&s, PASSWORD).await;
    let key = a.post("/api/v1/api-keys", json!({"name": "producers"})).await["key"].as_str().unwrap().to_string();
    let old = a.post("/api/v1/api-keys", json!({"name": "retired"})).await;
    let r = s
        .client
        .delete(s.u(&format!("/api/v1/api-keys/{}", old["id"])))
        .header("cookie", &a.cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 204);
    let old = old["key"].as_str().unwrap().to_string();

    let k = Some(key.as_str());
    assert_eq!(ingest_with(&s, k, "/api/v1/events", "application/x-ndjson", fixture("logs.ndjson")).await, 200);
    assert_eq!(ingest_with(&s, k, "/v1/traces", "application/json", fixture("otlp-traces.json")).await, 200);
    // Sealed into segments, then more data that stays in the WAL.
    s.rotate_all();
    assert_eq!(ingest_with(&s, k, "/v1/logs", "application/json", fixture("otlp-logs.json")).await, 200);
    assert_eq!(ingest_with(&s, k, "/v1/metrics", "application/json", fixture("otlp-metrics.json")).await, 200);

    let d = a.post("/api/v1/dashboards", json!({"name": "Checkout", "description": "restore drill"})).await;
    a.post(
        &format!("/api/v1/dashboards/{}/panels", d["id"]),
        json!({"title": "Errors", "kind": "log_count", "config": {"query": "level = Error"}}),
    )
    .await;
    a.post(
        "/api/v1/alerts",
        json!({"name": "Error spike", "query": "level = Error", "op": ">", "threshold": 5.0,
               "windowSecs": 300, "intervalSecs": 60, "enabled": false}),
    )
    .await;
    a.post(
        "/api/v1/saved-queries",
        json!({"name": "Inventory warnings", "signal": "logs", "query": "level = Warning"}),
    )
    .await;

    let answers = verification(&a).await;
    // The comparison after restore is only as good as what is compared.
    for signal in ["logs", "traces", "metrics"] {
        assert!(answers["counts"][signal].as_u64().unwrap() > 0, "{signal} populated: {answers}");
    }
    for (field, n) in [("errorLogs", 2), ("traceSpans", 3), ("alerts", 1), ("apiKeys", 1)] {
        assert!(answers[field].as_array().unwrap().len() >= n, "{field}: {answers}");
    }
    assert_eq!(answers["apiKeys"][0]["name"], "producers", "the revoked key is not listed");
    assert_eq!(answers["dashboards"]["dashboards"][0]["panels"][0]["title"], "Errors");
    assert_eq!(answers["savedQueries"]["savedQueries"][0]["name"], "Inventory warnings");
    assert!(!answers["metricNames"].as_array().unwrap().is_empty(), "{answers}");
    let cookie = a.cookie.clone();
    s.stop().await;
    (dir, key, old, cookie, answers)
}

#[tokio::test(flavor = "multi_thread")]
async fn restored_backup_keeps_telemetry_configuration_and_authentication() {
    let (source, key, revoked, cookie, before) = populated().await;
    // The backup: a plain copy of the stopped server's directory. Then the
    // original host is lost.
    let backup = tempfile::tempdir().unwrap();
    copy_tree(source.path(), backup.path());
    drop(source);
    assert!(backup.path().join("vyrtel.json").is_file(), "the backup carries the manifest");

    // A clean host restores it, without the admin password in its config.
    let host = tempfile::tempdir().unwrap();
    let restored = host.path().join("data");
    copy_tree(backup.path(), &restored);
    let mut c = secure(&restored);
    c.auth.admin_password = None;
    let s = TestServer::launch(c, Some(host)).await;
    // The stored admin user is used: no new password is generated.
    let a = Admin::login(&s, PASSWORD).await;
    assert_eq!(verification(&a).await, before, "every verification query answers as before the backup");

    // Authentication behaves as before: the restored key ingests, the
    // revoked one and anonymous requests are refused, and a session from
    // before the backup is still valid until it expires.
    let k = Some(key.as_str());
    assert_eq!(
        ingest_with(&s, k, "/api/v1/events", "application/json", r#"{"message":"after restore"}"#.into()).await,
        200
    );
    assert_eq!(ingest_with(&s, Some(&revoked), "/api/v1/events", "application/json", "{}".into()).await, 401);
    assert_eq!(ingest_with(&s, None, "/api/v1/events", "application/json", "{}".into()).await, 401);
    let r = s.client.get(s.u("/api/v1/dashboards")).send().await.unwrap();
    assert_eq!(r.status(), 401);
    let r = s.client.get(s.u("/api/v1/auth/me")).header("cookie", &cookie).send().await.unwrap();
    assert_eq!(r.status(), 200);
    let r = s.client.get(s.u("/api/v1/dashboards")).bearer_auth(&key).send().await.unwrap();
    assert_eq!(r.status(), 403, "an ingest key still cannot read configuration");

    // A stopped server's backup restores like a clean stop: nothing damaged.
    let storage = a.get("/api/v1/system/storage").await;
    for signal in ["logs", "traces", "metrics"] {
        assert_eq!(storage["signals"][signal]["recovery"]["quarantined"], json!([]), "{signal}");
    }
    let after = a.post("/api/v1/query/logs/count", json!({"query": ""})).await["count"].as_u64().unwrap();
    assert_eq!(after, before["counts"]["logs"].as_u64().unwrap() + 1);
    s.stop().await;
}

/// Start `dir` and expect it to be refused with a message containing
/// `expected`, leaving every file and directory as it was (no lock file,
/// no `tmp/`, no new database).
fn assert_refused_unchanged(dir: &Path, expected: &str) -> String {
    let before = snapshot(dir);
    let err = match App::build(secure(dir)) {
        Ok(_) => panic!("the backup must be refused"),
        Err(e) => format!("{e:#}"),
    };
    assert!(err.contains(expected), "{err}");
    assert!(err.contains("Vyrtel did not start and changed nothing in it"), "{err}");
    assert!(err.contains("docs/configuration.md#backup-and-restore"), "{err}");
    assert_eq!(snapshot(dir), before, "the refused backup is unchanged");
    err
}

async fn stopped_backup() -> tempfile::TempDir {
    let (source, ..) = populated().await;
    let backup = tempfile::tempdir().unwrap();
    copy_tree(source.path(), backup.path());
    backup
}

#[tokio::test(flavor = "multi_thread")]
async fn incomplete_backups_are_refused_without_changes() {
    let backup = stopped_backup().await;
    let dir = backup.path();

    // The metadata database left out of the backup. Before REL-06 this
    // started with an empty database and a newly generated admin password.
    let db = dir.join("metadata").join("metadata.db");
    let db_bytes = std::fs::read(&db).unwrap();
    std::fs::remove_file(&db).unwrap();
    assert_refused_unchanged(dir, "is incomplete: metadata/metadata.db is missing");
    std::fs::write(&db, &db_bytes).unwrap();

    // A signal's WAL directory left out.
    let wal = dir.join("wal").join("metrics");
    let parked = dir.join("parked-wal-metrics");
    std::fs::rename(&wal, &parked).unwrap();
    assert_refused_unchanged(dir, "is incomplete: wal/metrics is missing");
    std::fs::rename(&parked, &wal).unwrap();

    // Telemetry without metadata and without a manifest (an older backup,
    // or a copy of only `wal/` and `segments/`).
    std::fs::remove_file(dir.join("vyrtel.json")).unwrap();
    std::fs::remove_dir_all(dir.join("metadata")).unwrap();
    assert_refused_unchanged(dir, "is incomplete: metadata/metadata.db is missing");
}

/// The first `*.<ext>` file in `dir/<sub>`, relative to `dir`.
fn first_file(dir: &Path, sub: &str, ext: &str) -> String {
    let mut names: Vec<String> = std::fs::read_dir(dir.join(sub))
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|n| n.ends_with(ext))
        .collect();
    names.sort();
    format!("{sub}/{}", names.first().unwrap_or_else(|| panic!("a {ext} file in {sub}")))
}

#[tokio::test(flavor = "multi_thread")]
async fn backups_missing_single_files_or_an_emptied_database_are_refused() {
    let backup = stopped_backup().await;
    let dir = backup.path();
    let manifest: Value = serde_json::from_slice(&std::fs::read(dir.join("vyrtel.json")).unwrap()).unwrap();
    let wal = first_file(dir, "wal/logs", ".wal");
    let seg = first_file(dir, "segments/traces", ".seg");
    let size = |rel: &str| std::fs::metadata(dir.join(rel)).unwrap().len();
    assert_eq!(manifest["inventory"]["files"][&wal], size(&wal), "{manifest}");
    assert_eq!(manifest["inventory"]["files"][&seg], size(&seg), "{manifest}");

    // One populated WAL and one segment left out, every directory present.
    let (wal_bytes, seg_bytes) = (std::fs::read(dir.join(&wal)).unwrap(), std::fs::read(dir.join(&seg)).unwrap());
    assert!(wal_bytes.len() > 24, "the WAL holds records");
    std::fs::remove_file(dir.join(&wal)).unwrap();
    std::fs::remove_file(dir.join(&seg)).unwrap();
    let err = assert_refused_unchanged(dir, "is incomplete: ");
    assert!(err.contains(&format!("{seg} is missing; {wal} is missing")), "{err}");
    assert!(err.contains("start once with --accept-incomplete-restore"), "{err}");

    // A truncated copy.
    std::fs::write(dir.join(&wal), &wal_bytes).unwrap();
    std::fs::write(dir.join(&seg), &seg_bytes[..seg_bytes.len() / 2]).unwrap();
    let half = seg_bytes.len() / 2;
    assert_refused_unchanged(
        dir,
        &format!("is incomplete: {seg} has {half} bytes, {} at the last clean shutdown", seg_bytes.len()),
    );
    std::fs::write(dir.join(&seg), &seg_bytes).unwrap();

    // The metadata database replaced by an empty file: not consent to lose
    // dashboards, alerts, users and keys.
    let db = dir.join("metadata").join("metadata.db");
    let db_bytes = std::fs::read(&db).unwrap();
    std::fs::write(&db, b"").unwrap();
    assert_refused_unchanged(dir, "is incomplete: metadata/metadata.db is empty (it has no schema)");
    std::fs::write(&db, &db_bytes).unwrap();

    // Complete again: starts, and the start drops the inventory.
    let s = TestServer::launch(secure(dir), None).await;
    let m: Value = serde_json::from_slice(&std::fs::read(dir.join("vyrtel.json")).unwrap()).unwrap();
    assert_eq!(m.get("inventory"), None, "a running directory has no inventory: {m}");
    s.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn missing_files_are_accepted_only_on_request() {
    let (source, _, _, _, before) = populated().await;
    let dir = source.path();
    let wal = first_file(dir, "wal/logs", ".wal");
    std::fs::remove_file(dir.join(&wal)).unwrap();
    assert_refused_unchanged(dir, &format!("is incomplete: {wal} is missing"));

    // --accept-incomplete-restore: starts without the WAL's events, keeps
    // everything else.
    let options = server::app::StartOptions { accept_incomplete_restore: true };
    let app = App::build_with(secure(dir), options).expect("accepted");
    let logs = app
        .state
        .engine
        .count(telemetry::Signal::Logs, "", query::TimeRange::new(None, None), &query::CancelToken::new())
        .unwrap()
        .0;
    let all = before["counts"]["logs"].as_u64().unwrap();
    assert!(logs > 0 && logs < all, "segment events remain, the WAL's are gone: {logs} of {all}");
    app.shutdown().await.unwrap();

    // Accepted once: the clean shutdown recorded the directory as it is now.
    let s = TestServer::launch(secure(dir), None).await;
    let a = Admin::login(&s, PASSWORD).await;
    assert_eq!(a.get("/api/v1/dashboards").await, before["dashboards"]);
    s.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn backups_from_a_newer_version_are_refused_without_changes() {
    let backup = stopped_backup().await;
    let dir = backup.path();
    let manifest = std::fs::read(dir.join("vyrtel.json")).unwrap();

    // A newer data-directory format.
    std::fs::write(dir.join("vyrtel.json"), r#"{"format": 3, "writtenBy": "9.0.0"}"#).unwrap();
    assert_refused_unchanged(dir, "has format 3 (written by Vyrtel 9.0.0)");
    std::fs::write(dir.join("vyrtel.json"), b"{ not json").unwrap();
    assert_refused_unchanged(dir, "has an unusable vyrtel.json");
    std::fs::write(dir.join("vyrtel.json"), &manifest).unwrap();

    // A segment in a newer format (a consistent header and checksummed footer).
    let seg = std::fs::read_dir(dir.join("segments").join("traces")).unwrap().flatten().next().unwrap().path();
    let original = std::fs::read(&seg).unwrap();
    let mut b = original.clone();
    b[8..10].copy_from_slice(&2u16.to_le_bytes());
    let f = b.len() - storage::segment::FOOTER_LEN;
    b[f + 40..f + 42].copy_from_slice(&2u16.to_le_bytes());
    let crc = crc32fast::hash(&b[f..f + 44]);
    b[f + 44..f + 48].copy_from_slice(&crc.to_le_bytes());
    std::fs::write(&seg, &b).unwrap();
    let name = seg.file_name().unwrap().to_str().unwrap().to_string();
    assert_refused_unchanged(dir, &format!("segments/traces/{name} uses segment format version 2"));
    std::fs::write(&seg, &original).unwrap();

    // A WAL in a newer format.
    let wal = std::fs::read_dir(dir.join("wal").join("metrics")).unwrap().flatten().next().unwrap().path();
    let original = std::fs::read(&wal).unwrap();
    let mut b = original.clone();
    b[8..10].copy_from_slice(&5u16.to_le_bytes());
    std::fs::write(&wal, &b).unwrap();
    assert_refused_unchanged(dir, "uses WAL file format version 5");
    std::fs::write(&wal, &original).unwrap();

    // A metadata database migrated by a newer version.
    let db = dir.join("metadata").join("metadata.db");
    rusqlite::Connection::open(&db)
        .unwrap()
        .execute("INSERT INTO schema_migrations (version, applied_at) VALUES (99, 0)", [])
        .unwrap();
    assert_refused_unchanged(dir, "metadata/metadata.db has schema version 99");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_directory_from_before_manifests_is_started_and_recorded() {
    let backup = stopped_backup().await;
    let dir = backup.path().to_path_buf();
    // Data directories written by 0.1.1 and earlier have no manifest.
    std::fs::remove_file(dir.join("vyrtel.json")).unwrap();
    let s = TestServer::launch(secure(&dir), Some(backup)).await;
    let a = Admin::login(&s, PASSWORD).await;
    assert_eq!(a.get("/api/v1/dashboards").await["dashboards"][0]["name"], "Checkout");
    let m: Value = serde_json::from_slice(&std::fs::read(dir.join("vyrtel.json")).unwrap()).unwrap();
    assert_eq!(m, json!({"format": 2, "writtenBy": env!("CARGO_PKG_VERSION")}));
    s.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn new_directories_start_and_get_a_manifest() {
    let parent = tempfile::tempdir().unwrap();
    // A fresh volume may already hold unrelated entries.
    std::fs::create_dir(parent.path().join("lost+found")).unwrap();
    let s = TestServer::launch(base_config(parent.path()), None).await;
    assert!(parent.path().join("vyrtel.json").is_file());
    assert!(parent.path().join("metadata").join("metadata.db").is_file());
    s.stop().await;
}

/// The real binary prints the reason and exits with status 1.
#[tokio::test(flavor = "multi_thread")]
async fn refused_backup_exits_with_status_1_and_the_reason() {
    let backup = stopped_backup().await;
    std::fs::remove_file(backup.path().join("metadata").join("metadata.db")).unwrap();
    let before = snapshot(backup.path());
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_vyrtel"))
        .args(["--data-dir", backup.path().to_str().unwrap(), "--bind", "127.0.0.1:0", "--log-level", "warn"])
        .env("VYRTEL_AUTH_ENABLED", "true")
        .env("VYRTEL_AUTH_ADMIN_PASSWORD", PASSWORD)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.starts_with("vyrtel: data directory "), "{stderr}");
    assert!(stderr.contains("is incomplete: metadata/metadata.db is missing"), "{stderr}");
    assert_eq!(snapshot(backup.path()), before);
}

/// The inventory is removed only by a start that holds the data directory
/// lock: a start refused because another process has the directory must not
/// cost the next start its check for missing files.
#[tokio::test(flavor = "multi_thread")]
async fn a_start_refused_by_the_lock_keeps_the_inventory() {
    let backup = stopped_backup().await;
    let dir = backup.path();
    let before = snapshot(dir);
    {
        let _held = storage::DataDirLock::acquire(dir).unwrap();
        let err = match App::build(secure(dir)) {
            Ok(_) => panic!("the directory is locked"),
            Err(e) => format!("{e:#}"),
        };
        assert!(err.contains("in use by another Vyrtel process"), "{err}");
        assert_eq!(snapshot(dir), before, "the locked directory is unchanged, its inventory included");
    }
    // The inventory still protects the next start.
    let wal = first_file(dir, "wal/logs", ".wal");
    std::fs::remove_file(dir.join(&wal)).unwrap();
    assert_refused_unchanged(dir, &format!("is incomplete: {wal} is missing"));
}
