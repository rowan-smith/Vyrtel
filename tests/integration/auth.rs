//! Missing auth, bad auth, scopes, sessions, and ingestion protection
//! following the ingest-scoped keys.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};

use serde_json::{Value, json};

use crate::common::*;

/// A server with an admin password, whose `client` sends no credentials.
async fn start_secure() -> TestServer {
    let mut s = TestServer::start_with(|c| {
        c.auth.admin_password = Some("correct horse".into());
    })
    .await;
    s.client = reqwest::Client::new();
    s
}

async fn login(s: &TestServer, password: &str) -> reqwest::Response {
    s.post("/api/v1/auth/login", json!({"username": "admin", "password": password})).await
}

fn session_cookie(r: &reqwest::Response) -> String {
    let c = r.headers()["set-cookie"].to_str().unwrap();
    c.split(';').next().unwrap().to_string()
}

#[tokio::test(flavor = "multi_thread")]
async fn ingestion_requires_a_valid_key() {
    let s = start_secure().await;
    // Create an ingest key as admin: ingestion now needs one.
    let r = login(&s, "correct horse").await;
    assert_eq!(r.status(), 200);
    let cookie = session_cookie(&r);
    let key: Value = s
        .client
        .post(s.u("/api/v1/api-keys"))
        .header("cookie", &cookie)
        .json(&json!({"name": "ci"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let plaintext = key["key"].as_str().unwrap().to_string();
    assert_eq!(key["scope"], "ingest");

    // Missing auth.
    let r = s.post("/api/v1/events", json!({"message": "x"})).await;
    assert_eq!(r.status(), 401);
    let e: Value = r.json().await.unwrap();
    assert_eq!(e["error"]["code"], "unauthorized");
    let r = s.otlp_json("/v1/logs", "{}".into()).await;
    assert_eq!(r.status(), 401);

    // Bad auth.
    let r = s
        .client
        .post(s.u("/api/v1/events"))
        .bearer_auth("vyr_not_a_real_key")
        .json(&json!({"message": "x"}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 401);
    let e: Value = r.json().await.unwrap();
    assert_eq!(e["error"]["message"], "invalid API key");

    // Ingest with the key.
    let r = s
        .client
        .post(s.u("/api/v1/events"))
        .bearer_auth(&plaintext)
        .json(&json!({"message": "authorised"}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    let r = s
        .client
        .post(s.u("/v1/logs"))
        .header("x-api-key", &plaintext)
        .header("content-type", "application/json")
        .body("{}")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);

    // An ingest key cannot read data.
    let r = s
        .client
        .post(s.u("/api/v1/query/logs"))
        .bearer_auth(&plaintext)
        .json(&json!({"query": ""}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 403);

    // Keys are listed without secrets and can be revoked. Another ingest
    // key keeps ingestion protected, so the revoked one is refused.
    let other: Value = s
        .client
        .post(s.u("/api/v1/api-keys"))
        .header("cookie", &cookie)
        .json(&json!({"name": "other"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(other["scope"], "ingest");
    let keys: Value =
        s.client.get(s.u("/api/v1/api-keys")).header("cookie", &cookie).send().await.unwrap().json().await.unwrap();
    let listed = keys["apiKeys"].as_array().unwrap().iter().find(|k| k["name"] == "ci").unwrap();
    assert!(listed.get("key").is_none());
    assert!(plaintext.starts_with(listed["prefix"].as_str().unwrap()));
    let r = s
        .client
        .delete(s.u(&format!("/api/v1/api-keys/{}", listed["id"])))
        .header("cookie", &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 204);
    let r = s
        .client
        .post(s.u("/api/v1/events"))
        .bearer_auth(&plaintext)
        .json(&json!({"message": "revoked"}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 401);
    s.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn ui_api_requires_session_or_admin_key() {
    let s = start_secure().await;
    assert_eq!(s.post("/api/v1/query/logs", json!({"query": ""})).await.status(), 401);
    assert_eq!(s.client.get(s.u("/api/v1/system/info")).send().await.unwrap().status(), 401);
    // Health/readiness and static UI stay public.
    assert_eq!(s.client.get(s.u("/health")).send().await.unwrap().status(), 200);
    assert_eq!(s.client.get(s.u("/ready")).send().await.unwrap().status(), 200);
    assert_eq!(s.client.get(s.u("/")).send().await.unwrap().status(), 200);
    let me: Value = s.client.get(s.u("/api/v1/auth/me")).send().await.unwrap().json().await.unwrap();
    assert_eq!(me, json!({"authEnabled": true, "user": null}));

    assert_eq!(login(&s, "wrong").await.status(), 401);
    let r = login(&s, "correct horse").await;
    let cookie = session_cookie(&r);
    assert!(r.headers()["set-cookie"].to_str().unwrap().contains("HttpOnly"));
    let r = s
        .client
        .post(s.u("/api/v1/query/logs"))
        .header("cookie", &cookie)
        .json(&json!({"query": ""}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    let me: Value =
        s.client.get(s.u("/api/v1/auth/me")).header("cookie", &cookie).send().await.unwrap().json().await.unwrap();
    assert_eq!(me["user"], "admin");

    // Admin-scoped keys may use the API.
    let key: Value = s
        .client
        .post(s.u("/api/v1/api-keys"))
        .header("cookie", &cookie)
        .json(&json!({"name": "automation", "scope": "admin"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let r = s.client.get(s.u("/api/v1/system/info")).bearer_auth(key["key"].as_str().unwrap()).send().await.unwrap();
    assert_eq!(r.status(), 200);

    // Logout invalidates the session.
    s.client.post(s.u("/api/v1/auth/logout")).header("cookie", &cookie).send().await.unwrap();
    let r = s
        .client
        .post(s.u("/api/v1/query/logs"))
        .header("cookie", &cookie)
        .json(&json!({"query": ""}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 401);
    s.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn generated_admin_password_when_none_configured() {
    let dir = tempfile::tempdir().unwrap();
    let app = server::App::build(base_config(dir.path())).unwrap();
    let pw = app.generated_password.clone().expect("password generated on first start");
    assert!(app.banner("127.0.0.1:8080".parse().unwrap()).contains(&pw));
    let user = app.state.metadata.find_user("admin").unwrap().unwrap();
    assert!(server::auth::verify_password(&pw, &user.password_hash));
    app.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn banner_shows_a_browsable_web_ui_url() {
    let dir = tempfile::tempdir().unwrap();
    let app = server::App::build(base_config(dir.path())).unwrap();
    // Wildcard binds can't be opened in a browser: point at localhost, keep the real bind for HTTP.
    let b = app.banner("0.0.0.0:8080".parse().unwrap());
    assert!(b.contains("Web UI              http://localhost:8080\n"), "{b}");
    assert!(b.contains("HTTP                http://0.0.0.0:8080\n"), "{b}");
    let b = app.banner("[::]:9000".parse().unwrap());
    assert!(b.contains("Web UI              http://localhost:9000\n"), "{b}");
    assert!(b.contains("OTLP HTTP           http://[::]:9000/v1/*\n"), "{b}");
    // A specific address is used as-is.
    let b = app.banner("192.168.1.20:8080".parse().unwrap());
    assert!(b.contains("Web UI              http://192.168.1.20:8080\n"), "{b}");
    app.shutdown().await.unwrap();
}

/// By default ingestion is open, and only ingestion: everything else still
/// needs a login or an admin key.
#[tokio::test(flavor = "multi_thread")]
async fn ingestion_without_keys_leaves_the_rest_of_the_api_closed() {
    let mut s = TestServer::start().await;
    s.client = reqwest::Client::new();
    assert_eq!(s.post("/api/v1/events", json!({"message": "open"})).await.status(), 200);
    assert_eq!(s.otlp_json("/v1/logs", "{}".into()).await.status(), 200);
    for path in ["/api/v1/query/logs", "/api/v1/api-keys"] {
        assert_eq!(s.post(path, json!({})).await.status(), 401, "{path}");
    }
    let me: Value = s.client.get(s.u("/api/v1/auth/me")).send().await.unwrap().json().await.unwrap();
    assert_eq!(me, json!({"authEnabled": true, "user": null}));
    s.stop().await;
}

/// Ingest one event with `key` (or none); returns the status code.
async fn ingest(s: &TestServer, key: Option<&str>) -> u16 {
    ingest_from(&reqwest::Client::new(), s, key).await
}

async fn ingest_from(client: &reqwest::Client, s: &TestServer, key: Option<&str>) -> u16 {
    let mut r = client.post(s.u("/api/v1/events")).json(&json!({"message": "x"}));
    if let Some(k) = key {
        r = r.bearer_auth(k);
    }
    r.send().await.unwrap().status().as_u16()
}

/// Create a key with the harness admin key; returns (id, plaintext).
async fn create_key(s: &TestServer, name: &str, scope: &str) -> (i64, String) {
    let r = s.post("/api/v1/api-keys", json!({"name": name, "scope": scope})).await;
    assert_eq!(r.status(), 201);
    let v: Value = r.json().await.unwrap();
    (v["id"].as_i64().unwrap(), v["key"].as_str().unwrap().to_string())
}

async fn revoke_key(s: &TestServer, id: i64) {
    let r = s.client.delete(s.u(&format!("/api/v1/api-keys/{id}"))).send().await.unwrap();
    assert_eq!(r.status(), 204);
}

/// Whether ingestion needs a key, as system info and the key list (the
/// UI's sources) report it; asserts they agree.
async fn key_required(s: &TestServer) -> bool {
    let info: Value = s.client.get(s.u("/api/v1/system/info")).send().await.unwrap().json().await.unwrap();
    let keys: Value = s.client.get(s.u("/api/v1/api-keys")).send().await.unwrap().json().await.unwrap();
    assert_eq!(info["ingestKeyRequired"], keys["ingestKeyRequired"]);
    info["ingestKeyRequired"].as_bool().unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn ingest_protection_follows_the_ingest_keys() {
    let s = TestServer::start().await;
    // No keys but the harness's admin key: open.
    assert!(!key_required(&s).await);
    assert_eq!(ingest(&s, None).await, 200);

    // Admin keys do not protect ingestion.
    let (admin_id, admin) = create_key(&s, "automation", "admin").await;
    assert!(!key_required(&s).await);
    assert_eq!(ingest(&s, None).await, 200);

    // The first ingest key protects it at once.
    let (a_id, a) = create_key(&s, "a", "ingest").await;
    assert!(key_required(&s).await);
    assert_eq!(ingest(&s, None).await, 401);
    assert_eq!(ingest(&s, Some("vyr_not_a_real_key")).await, 401);
    assert_eq!(ingest(&s, Some(&a)).await, 200);
    assert_eq!(ingest(&s, Some(&admin)).await, 200, "any valid key may ingest");

    // Revoking one of two ingest keys keeps it protected.
    let (b_id, b) = create_key(&s, "b", "ingest").await;
    revoke_key(&s, a_id).await;
    assert!(key_required(&s).await);
    assert_eq!(ingest(&s, Some(&a)).await, 401);
    assert_eq!(ingest(&s, Some(&b)).await, 200);
    assert_eq!(ingest(&s, None).await, 401);

    // Revoking an admin key changes nothing; revoking the last ingest key
    // reopens ingestion at once.
    revoke_key(&s, admin_id).await;
    assert!(key_required(&s).await);
    revoke_key(&s, b_id).await;
    assert!(!key_required(&s).await);
    assert_eq!(ingest(&s, None).await, 200);
    s.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn ingest_protection_survives_restarts() {
    let s = TestServer::start().await;
    let (id, key) = create_key(&s, "producers", "ingest").await;
    let (config, dir) = s.stop().await;

    let s = TestServer::launch(config, dir).await;
    assert!(key_required(&s).await);
    assert_eq!(ingest(&s, None).await, 401);
    assert_eq!(ingest(&s, Some(&key)).await, 200);
    revoke_key(&s, id).await;
    let (config, dir) = s.stop().await;

    let s = TestServer::launch(config, dir).await;
    assert!(!key_required(&s).await);
    assert_eq!(ingest(&s, None).await, 200);
    assert_eq!(ingest(&s, Some(&key)).await, 200, "open again, so the revoked key is not checked");
    s.stop().await;
}

/// Producers keep sending while the first ingest key is created and the
/// last revoked: a request that starts after either change returns sees
/// it, none fails otherwise, and a valid key is accepted throughout.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ingest_protection_changes_are_immediate_under_concurrent_requests() {
    const OPEN: u8 = 0;
    const PROTECTED: u8 = 1;
    const CHANGING: u8 = 2;
    let s = Arc::new(TestServer::start().await);
    let phase = Arc::new(AtomicU8::new(OPEN));
    let done = Arc::new(AtomicBool::new(false));
    let (_, admin) = create_key(&s, "automation", "admin").await;

    let producers: Vec<_> = (0..8)
        .map(|i| {
            let (s, phase, done, admin) = (s.clone(), phase.clone(), done.clone(), admin.clone());
            tokio::spawn(async move {
                // 429 is ingest admission turning away a request that
                // authentication let through.
                let authorized = |status: u16| status == 200 || status == 429;
                let client = reqwest::Client::new();
                let mut checked = 0;
                while !done.load(Ordering::SeqCst) {
                    if i % 2 == 1 {
                        let status = ingest_from(&client, &s, Some(&admin)).await;
                        assert!(authorized(status), "a valid key is always accepted, got {status}");
                        continue;
                    }
                    let before = phase.load(Ordering::SeqCst);
                    let status = ingest_from(&client, &s, None).await;
                    let after = phase.load(Ordering::SeqCst);
                    assert!(authorized(status) || status == 401, "unexpected {status}");
                    if before == after && before != CHANGING {
                        assert_eq!(
                            authorized(status),
                            before == OPEN,
                            "a keyless request after the change returned got {status}"
                        );
                        checked += 1;
                    }
                }
                checked
            })
        })
        .collect();

    for round in 0..20 {
        phase.store(CHANGING, Ordering::SeqCst);
        let (id, _) = create_key(&s, &format!("k{round}"), "ingest").await;
        phase.store(PROTECTED, Ordering::SeqCst);
        assert_eq!(ingest(&s, None).await, 401);
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        phase.store(CHANGING, Ordering::SeqCst);
        revoke_key(&s, id).await;
        phase.store(OPEN, Ordering::SeqCst);
        assert_ne!(ingest(&s, None).await, 401, "open again at once");
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    done.store(true, Ordering::SeqCst);
    let mut checked = 0;
    for p in producers {
        checked += p.await.unwrap();
    }
    assert!(checked > 0, "no request ran between changes");

    // Many keys created and revoked at once leave the count exact.
    let created = futures_util::future::join_all((0..16).map(|i| {
        let s = s.clone();
        async move { create_key(&s, &format!("c{i}"), "ingest").await.0 }
    }))
    .await;
    assert!(key_required(&s).await);
    let (last, rest) = created.split_last().unwrap();
    futures_util::future::join_all(rest.iter().map(|&id| {
        let s = s.clone();
        async move { revoke_key(&s, id).await }
    }))
    .await;
    assert!(key_required(&s).await, "one ingest key is left");
    assert_eq!(ingest(&s, None).await, 401);
    revoke_key(&s, *last).await;
    assert!(!key_required(&s).await);
    assert_eq!(ingest(&s, None).await, 200);
    Arc::into_inner(s).unwrap().stop().await;
}

/// AUTH-PERF-01: a key write stalled in SQLite (another connection holds
/// the write lock) holds up only key changes. Ingestion keeps being
/// decided by the keys in force; the revoke applies once it commits; a
/// write that cannot commit fails and changes nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stalled_key_writes_do_not_hold_up_ingestion() {
    let s = Arc::new(TestServer::start().await);
    let (id, key) = create_key(&s, "producers", "ingest").await;
    let db = rusqlite::Connection::open(s.data_dir().join(server::datadir::METADATA_DB)).unwrap();

    db.execute_batch("BEGIN IMMEDIATE").unwrap();
    let revoking = tokio::spawn({
        let s = s.clone();
        async move { s.client.delete(s.u(&format!("/api/v1/api-keys/{id}"))).send().await.unwrap().status().as_u16() }
    });
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    for _ in 0..20 {
        assert_eq!(ingest(&s, Some(&key)).await, 200);
        assert_eq!(ingest(&s, None).await, 401, "still protected: the revoke has not committed");
    }
    assert!(!revoking.is_finished(), "requests did not wait for the stalled revoke");
    db.execute_batch("COMMIT").unwrap();
    assert_eq!(revoking.await.unwrap(), 204);
    assert!(!key_required(&s).await);
    assert_eq!(ingest(&s, None).await, 200, "open once the revoke committed");

    // Held past the store's 5 s busy timeout: the create fails, and no key
    // is accepted or protects ingestion.
    db.execute_batch("BEGIN IMMEDIATE").unwrap();
    let r = s.post("/api/v1/api-keys", json!({"name": "never", "scope": "ingest"})).await;
    assert_eq!(r.status(), 500);
    db.execute_batch("ROLLBACK").unwrap();
    assert!(!key_required(&s).await);
    assert_eq!(ingest(&s, None).await, 200);
    let keys: Value = s.client.get(s.u("/api/v1/api-keys")).send().await.unwrap().json().await.unwrap();
    assert!(keys["apiKeys"].as_array().unwrap().iter().all(|k| k["name"] != "never"));
    drop(db);
    Arc::into_inner(s).unwrap().stop().await;
}
