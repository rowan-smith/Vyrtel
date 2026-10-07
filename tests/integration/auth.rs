//! Missing auth, bad auth, scopes, sessions.

use serde_json::{Value, json};

use crate::common::*;

async fn start_secure() -> TestServer {
    TestServer::start_with(|c| {
        c.auth.enabled = true;
        c.auth.admin_password = Some("correct horse".into());
    })
    .await
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

    // Create a key as admin and ingest with it.
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

    // Keys are listed without secrets and can be revoked.
    let keys: Value =
        s.client.get(s.u("/api/v1/api-keys")).header("cookie", &cookie).send().await.unwrap().json().await.unwrap();
    let listed = &keys["apiKeys"][0];
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
    let mut c = base_config(dir.path());
    c.auth.enabled = true;
    let app = server::App::build(c).unwrap();
    let pw = app.generated_password.clone().expect("password generated on first start");
    assert!(app.banner("127.0.0.1:8080".parse().unwrap()).contains(&pw));
    let user = app.state.metadata.find_user("admin").unwrap().unwrap();
    assert!(server::auth::verify_password(&pw, &user.password_hash));
    app.shutdown().await;
}
