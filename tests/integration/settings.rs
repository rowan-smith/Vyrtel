//! SET-04 (#253): admin-managed settings, end to end. Saved values are
//! validated whole, committed with a revision check, reported as pending,
//! and used from the next start; deployment values win; only
//! administrators change them; the offline CLI inspects and repairs them.

use std::time::Duration;

use serde_json::{Value, json};

use crate::common::{Admin, TestServer, authorized_client};

const PASSWORD: &str = "correct horse";

async fn start() -> TestServer {
    TestServer::start_with(|c| c.auth.admin_password = Some(PASSWORD.into())).await
}

fn field<'a>(doc: &'a Value, key: &str) -> &'a Value {
    let (group, name) = key.split_once('.').unwrap();
    let g = doc["groups"].as_array().unwrap().iter().find(|g| g["name"] == group).unwrap();
    g["fields"].as_array().unwrap().iter().find(|f| f["name"] == name).unwrap()
}

fn revision(doc: &Value, group: &str) -> u64 {
    doc["groups"].as_array().unwrap().iter().find(|g| g["name"] == group).unwrap()["revision"].as_u64().unwrap()
}

async fn put(s: &TestServer, client: &reqwest::Client, group: &str, body: Value) -> (u16, Value) {
    let r = client.put(s.u(&format!("/api/v1/settings/{group}"))).json(&body).send().await.unwrap();
    (r.status().as_u16(), r.json().await.unwrap_or(Value::Null))
}

/// TC: a saved value is pending (never claimed live), survives a restart,
/// and is then what the server runs with; the system config agrees.
#[tokio::test(flavor = "multi_thread")]
async fn saved_settings_are_pending_until_the_next_start() {
    let s = start().await;
    let doc = s.get_ok("/api/v1/settings").await;
    assert_eq!(doc["restartRequired"], false);
    assert_eq!(field(&doc, "query.max_results")["effective"], 1000);
    assert_eq!(field(&doc, "query.max_results")["apply"], "restart");
    let rev = revision(&doc, "query");

    let (status, doc) =
        put(&s, &s.client, "query", json!({"revision": rev, "values": {"max_results": 7, "timeout": "45s"}})).await;
    assert_eq!(status, 200, "{doc}");
    assert_eq!(doc["restartRequired"], true);
    let f = field(&doc, "query.max_results");
    assert_eq!((&f["effective"], &f["configured"], &f["pending"]), (&json!(1000), &json!(7), &json!(7)));
    // Nothing running changed: the engine still has its old limit.
    assert_eq!(s.state().config.query.max_results, 1000);
    let change = doc["changes"].as_array().unwrap().last().unwrap().clone();
    assert_eq!(change["outcome"], "committed");
    assert!(change["actor"].as_str().unwrap().starts_with("api-key:"), "{change}");
    assert_eq!(change["fields"], json!(["query.timeout", "query.max_results"]));
    assert!(!change.to_string().contains("45s"), "records carry no values");

    let s = s.restart().await;
    let doc = s.get_ok("/api/v1/settings").await;
    assert_eq!(doc["restartRequired"], false);
    let f = field(&doc, "query.max_results");
    assert_eq!((&f["effective"], &f["source"], &f["pending"]), (&json!(7), &json!("settings"), &Value::Null));
    assert_eq!(s.state().config.query.max_results, 7);
    assert_eq!(s.state().config.query.timeout, Duration::from_secs(45));
    let cfg = s.get_ok("/api/v1/system/config").await;
    assert_eq!(cfg["config"]["query"]["max_results"], 7);
    assert_eq!(revision(&doc, "query"), rev + 1);
    s.stop().await;
}

/// TC: stale writes conflict, invalid ones (on their own or with other
/// groups) are refused, and none of them changes anything.
#[tokio::test(flavor = "multi_thread")]
async fn stale_and_invalid_writes_change_nothing() {
    let s = start().await;
    let (status, _) = put(&s, &s.client, "query", json!({"revision": 0, "values": {"max_results": 5}})).await;
    assert_eq!(status, 200);
    let before = s.get_ok("/api/v1/settings/export").await;

    let (status, e) = put(&s, &s.client, "query", json!({"revision": 0, "values": {"max_results": 6}})).await;
    assert_eq!((status, e["error"]["code"].as_str()), (409, Some("settings_conflict")), "{e}");
    assert_eq!(e["error"]["current"], 1, "{e}");
    for (group, body, code) in [
        ("storage", json!({"revision": 0, "values": {"max_memory": "64MB"}}), "invalid_setting"),
        ("query", json!({"revision": 1, "values": {"max_concurrent": -1}}), "invalid_setting"),
        ("query", json!({"revision": 1, "values": {"max_concurrent": 1e40}}), "invalid_setting"),
        ("query", json!({"revision": 1, "values": {"nope": 1}}), "unknown_setting"),
        ("server", json!({"revision": 0, "values": {"bind": "0.0.0.0:1"}}), "unknown_setting"),
        ("auth", json!({"revision": 0, "values": {"admin_password": "x"}}), "unknown_setting"),
        ("retention", json!({"revision": 0, "values": {"logs": "1d"}}), "confirmation_required"),
    ] {
        let (status, e) = put(&s, &s.client, group, body.clone()).await;
        assert_eq!(e["error"]["code"], code, "{body}: {status} {e}");
        assert!(matches!(status, 400 | 409 | 428), "{body}: {status}");
    }
    let r = s.client.put(s.u("/api/v1/settings/nope")).json(&json!({"revision": 0, "values": {}})).send().await;
    assert_eq!(r.unwrap().status(), 404);
    assert_eq!(s.get_ok("/api/v1/settings/export").await["settings"], before["settings"]);

    // Confirmed, the dangerous change saves; no GET changes anything.
    let body = json!({"revision": 0, "values": {"logs": "1d"}, "confirm": true});
    assert_eq!(put(&s, &s.client, "retention", body).await.0, 200);
    let doc = s.get_ok("/api/v1/settings").await;
    let refused: Vec<&str> =
        doc["changes"].as_array().unwrap().iter().map(|c| c["outcome"].as_str().unwrap()).collect();
    assert!(refused.contains(&"conflict") && refused.contains(&"confirmation_required"), "{refused:?}");
    s.stop().await;
}

/// TC: cancelling pending values, with its own revision check.
#[tokio::test(flavor = "multi_thread")]
async fn pending_values_can_be_cancelled() {
    let s = start().await;
    let (_, doc) = put(&s, &s.client, "alerts", json!({"revision": 0, "values": {"webhook_timeout": "3s"}})).await;
    assert_eq!(field(&doc, "alerts.webhook_timeout")["pending"], "3s");
    let stale =
        s.client.post(s.u("/api/v1/settings/alerts/cancel")).json(&json!({"revision": 0})).send().await.unwrap();
    assert_eq!(stale.status(), 409);
    let doc = s.post_ok("/api/v1/settings/alerts/cancel", json!({"revision": 1})).await;
    assert_eq!(field(&doc, "alerts.webhook_timeout")["pending"], Value::Null);
    assert_eq!(field(&doc, "alerts.webhook_timeout")["configured"], Value::Null);
    assert_eq!(doc["restartRequired"], false);
    assert_eq!(revision(&doc, "alerts"), 2);
    s.stop().await;
}

/// TC: export and import round-trip, atomically; the latch, credentials
/// and deployment locators are never part of either.
#[tokio::test(flavor = "multi_thread")]
async fn export_and_import_roundtrip() {
    let s = start().await;
    put(&s, &s.client, "query", json!({"revision": 0, "values": {"max_results": 5}})).await;
    put(&s, &s.client, "alerts", json!({"revision": 0, "values": {"webhook_timeout": "3s"}})).await;
    let export = s.get_ok("/api/v1/settings/export").await;
    assert_eq!(export["settings"], json!({"alerts.webhook_timeout": "3s", "query.max_results": 5}));
    for secret in ["ingest_protection", "ingest_auth_mode", "admin_password", "storage.path", "server.bind", "revision"]
    {
        assert!(!export.to_string().contains(secret), "{secret} in {export}");
    }
    let doc = s.get_ok("/api/v1/settings").await;
    let revisions: Value = doc["groups"]
        .as_array()
        .unwrap()
        .iter()
        .map(|g| (g["name"].as_str().unwrap().to_string(), g["revision"].clone()))
        .collect::<serde_json::Map<_, _>>()
        .into();

    // A bad entry refuses the whole import.
    let mut bad = export.clone();
    bad["settings"]["query.max_trace_spans"] = json!(3);
    bad["settings"]["storage.path"] = json!("/elsewhere");
    let r = s.post("/api/v1/settings/import", json!({"revisions": revisions, "export": bad})).await;
    assert_eq!(r.status(), 400);
    assert_eq!(s.get_ok("/api/v1/settings/export").await["settings"], export["settings"]);

    let mut next = export.clone();
    next["settings"] = json!({"query.max_trace_spans": 3});
    let doc = s.post_ok("/api/v1/settings/import", json!({"revisions": revisions, "export": next})).await;
    assert_eq!(field(&doc, "query.max_trace_spans")["configured"], 3);
    assert_eq!(field(&doc, "query.max_results")["configured"], Value::Null);
    // The same revisions are stale now.
    let r = s.post("/api/v1/settings/import", json!({"revisions": revisions, "export": export})).await;
    assert_eq!(r.status(), 409);
    s.stop().await;
}

/// TC: only administrators change settings: ingest and query keys get 403,
/// no credential 401. A session cookie works from Vyrtel's own pages and is
/// refused cross-site.
#[tokio::test(flavor = "multi_thread")]
async fn only_administrators_change_settings_from_their_own_origin() {
    let s = start().await;
    for scope in ["ingest", "query"] {
        let (_, key) =
            server::auth::create_api_key(s.state(), &format!("settings-{scope}"), scope, None).await.unwrap();
        let c = authorized_client("authorization", &format!("Bearer {key}"));
        assert_eq!(c.get(s.u("/api/v1/settings")).send().await.unwrap().status(), 403, "{scope}");
        assert_eq!(put(&s, &c, "query", json!({"revision": 0, "values": {"max_results": 5}})).await.0, 403, "{scope}");
        assert_eq!(c.get(s.u("/api/v1/settings/export")).send().await.unwrap().status(), 403, "{scope}");
    }
    let anon = reqwest::Client::new();
    assert_eq!(anon.get(s.u("/api/v1/settings")).send().await.unwrap().status(), 401);
    assert_eq!(put(&s, &anon, "query", json!({"revision": 0, "values": {"max_results": 5}})).await.0, 401);

    let admin = Admin::login(&s, PASSWORD).await;
    let body = json!({"revision": 0, "values": {"max_results": 5}});
    let send = |site: Option<&'static str>, origin: Option<String>| {
        let mut r = anon.put(s.u("/api/v1/settings/query")).header("cookie", &admin.cookie).json(&body);
        if let Some(site) = site {
            r = r.header("sec-fetch-site", site);
        }
        if let Some(origin) = origin {
            r = r.header("origin", origin);
        }
        r.send()
    };
    assert_eq!(send(Some("cross-site"), Some("http://evil.example".into())).await.unwrap().status(), 403);
    assert_eq!(send(Some("same-site"), None).await.unwrap().status(), 403);
    assert_eq!(send(None, Some("http://evil.example".into())).await.unwrap().status(), 403);
    assert_eq!(s.get_ok("/api/v1/settings/export").await["settings"], json!({}));
    let r = send(Some("same-origin"), Some(s.url.clone())).await.unwrap();
    assert_eq!(r.status(), 200);
    let doc: Value = r.json().await.unwrap();
    assert_eq!(doc["changes"].as_array().unwrap().last().unwrap()["actor"], "user:admin");
    s.stop().await;
}

/// TC: a value the deployment sets wins over a saved one and cannot be
/// saved; the document says where it comes from.
#[tokio::test(flavor = "multi_thread")]
async fn deployment_values_win() {
    let s = start().await;
    put(&s, &s.client, "query", json!({"revision": 0, "values": {"max_results": 5, "max_trace_spans": 9}})).await;
    let (mut config, dir) = s.stop().await;
    config.query.max_results = 11;
    config.origin.set("query.max_results", server::config::Source::File);
    let s = TestServer::launch(config, dir).await;
    let doc = s.get_ok("/api/v1/settings").await;
    let f = field(&doc, "query.max_results");
    assert_eq!((&f["effective"], &f["managedBy"], &f["overridden"]), (&json!(11), &json!("file"), &json!(true)));
    assert_eq!(field(&doc, "query.max_trace_spans")["effective"], 9);
    let (status, e) = put(&s, &s.client, "query", json!({"revision": 1, "values": {"max_results": 6}})).await;
    assert_eq!((status, e["error"]["code"].as_str()), (409, Some("setting_managed_by_deployment")));
    let pw = doc["deployment"].as_array().unwrap().iter().find(|d| d["key"] == "auth.admin_password").unwrap();
    assert_eq!((&pw["set"], &pw["value"]), (&json!(true), &Value::Null));
    assert!(!doc.to_string().contains(PASSWORD));
    s.stop().await;
}

fn vyrtel(dir: &std::path::Path, args: &[&str]) -> std::process::Output {
    std::process::Command::new(env!("CARGO_BIN_EXE_vyrtel"))
        .args(["--data-dir", dir.to_str().unwrap()])
        .args(args)
        .env_remove("VYRTEL_LOG_LEVEL")
        .output()
        .unwrap()
}

/// TC: saved settings that no longer form a valid configuration stop the
/// start with how to fix them; the offline CLI shows, repairs, exports and
/// imports them without hand-editing SQLite, and refuses while a server
/// holds the data directory.
#[tokio::test(flavor = "multi_thread")]
async fn offline_recovery() {
    let s = start().await;
    put(&s, &s.client, "query", json!({"revision": 0, "values": {"max_results": 5}})).await;
    // While it runs, the CLI may read but not write.
    let out = vyrtel(&s.data_dir(), &["settings", "set", "query.max_results", "6"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("lock"), "{}", String::from_utf8_lossy(&out.stderr));
    let out = vyrtel(&s.data_dir(), &["settings", "diff"]);
    assert!(out.status.success());
    let diff = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(
        diff.lines().any(|l| l.starts_with("query.max_results") && l.contains("5") && l.ends_with("settings")),
        "{diff}"
    );
    let (config, dir) = s.stop().await;
    let path = config.storage.path.clone();

    // Damage one stored value as a bad hand edit or a future version would.
    let db = rusqlite::Connection::open(path.join(server::datadir::METADATA_DB)).unwrap();
    db.execute("UPDATE settings SET value = '\"lots\"' WHERE key = 'config.query.max_results'", []).unwrap();
    drop(db);
    let out = vyrtel(&path, &["--bind", "127.0.0.1:0"]);
    assert_eq!(out.status.code(), Some(2));
    let err = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(
        err.contains("saved setting query.max_results") && err.contains("vyrtel settings reset query.max_results"),
        "{err}"
    );
    let e = format!("{:#}", server::App::build(config.clone()).err().unwrap());
    assert!(e.contains("saved setting query.max_results"), "{e}");

    let out = vyrtel(&path, &["settings", "reset", "query.max_results"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let out = vyrtel(&path, &["settings", "set", "query.max_trace_spans", "77"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let out = vyrtel(&path, &["settings", "set", "retention.logs", "1d"]);
    assert!(!out.status.success(), "dangerous needs --confirm");
    let out = vyrtel(&path, &["settings", "set", "storage.max_memory", "64MB"]);
    assert!(String::from_utf8_lossy(&out.stderr).contains("ingest budget too small"));

    let out = vyrtel(&path, &["settings", "export"]);
    let export: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(export["settings"], json!({"query.max_trace_spans": 77}));
    let file = path.join("export.json");
    std::fs::write(
        &file,
        json!({"format": "vyrtel.settings", "version": 1, "settings": {"alerts.webhook_timeout": "4s"}}).to_string(),
    )
    .unwrap();
    let out = vyrtel(&path, &["settings", "import", file.to_str().unwrap()]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    std::fs::remove_file(&file).unwrap();

    let s = TestServer::launch(config, dir).await;
    let doc = s.get_ok("/api/v1/settings").await;
    assert_eq!(field(&doc, "alerts.webhook_timeout")["effective"], "4s");
    assert_eq!(field(&doc, "query.max_trace_spans")["configured"], Value::Null);
    assert_eq!(field(&doc, "query.max_results")["effective"], 1000);
    let last = doc["changes"].as_array().unwrap().last().unwrap().clone();
    assert_eq!((&last["actor"], &last["action"]), (&json!("cli"), &json!("import")));
    s.stop().await;
}

/// TC: saved log settings decide the process's logging from its first line.
#[tokio::test(flavor = "multi_thread")]
async fn saved_log_settings_apply_from_the_next_start() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path()).unwrap();
    // A first start creates the data directory; then save json logging offline.
    let p =
        crate::common::Process::start_binary(std::path::Path::new(env!("CARGO_BIN_EXE_vyrtel")), dir.path(), &[]).await;
    p.kill();
    let out = vyrtel(dir.path(), &["settings", "set", "log.format", "json"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let out = vyrtel(dir.path(), &["settings", "set", "log.level", "info"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));

    let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_vyrtel"))
        .args(["--data-dir", dir.path().to_str().unwrap(), "--bind", "127.0.0.1:0"])
        .env_remove("VYRTEL_LOG_LEVEL")
        .env_remove("VYRTEL_LOG_FORMAT")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let mut stderr = child.stderr.take().unwrap();
    let reader = std::thread::spawn(move || {
        use std::io::Read;
        let mut buf = [0u8; 4096];
        let mut out = Vec::new();
        while !out.contains(&b'\n') {
            match stderr.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => out.extend_from_slice(&buf[..n]),
            }
        }
        String::from_utf8_lossy(&out).to_string()
    });
    let first = reader.join().unwrap();
    child.kill().unwrap();
    let _ = child.wait();
    let line = first.lines().next().unwrap_or_default();
    assert!(serde_json::from_str::<Value>(line).is_ok(), "the first log line is JSON: {first}");
}
