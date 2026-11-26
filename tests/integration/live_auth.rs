//! API-01A (#128): a live-tail stream sends only while the credential that
//! opened it is valid. Logout, revocation, password changes and
//! deactivation end it as soon as they return; expiry within
//! `REAUTHORIZE_EVERY`; ingestion-key changes never open or close query
//! access.

use std::time::{Duration, Instant};

use serde_json::{Value, json};
use server::auth::REAUTHORIZE_EVERY;

use crate::common::*;

/// Well under [`REAUTHORIZE_EVERY`]: a change ends a stream on its own
/// notification, not the periodic recheck.
const PROMPT: Duration = Duration::from_secs(2);

/// A server with an admin password, whose `client` sends no credentials.
async fn start_secure_with(f: impl FnOnce(&mut server::config::Config)) -> TestServer {
    let mut s = TestServer::start_with(|c| {
        c.auth.admin_password = Some("correct horse".into());
        f(c);
    })
    .await;
    s.client = reqwest::Client::new();
    s
}

async fn start_secure() -> TestServer {
    start_secure_with(|_| {}).await
}

fn admin(s: &TestServer) -> reqwest::Client {
    authorized_client("authorization", &format!("Bearer {}", s.api_key))
}

fn by_key(key: &str) -> reqwest::Client {
    authorized_client("authorization", &format!("Bearer {key}"))
}

/// A session of `username`, and a client that sends only its cookie.
async fn sign_in(s: &TestServer, username: &str, password: &str) -> (String, reqwest::Client) {
    let r = reqwest::Client::new()
        .post(s.u("/api/v1/auth/login"))
        .json(&json!({"username": username, "password": password}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200, "sign in as {username}");
    let cookie = r.headers()["set-cookie"].to_str().unwrap().split(';').next().unwrap().to_string();
    (cookie.clone(), authorized_client("cookie", &cookie))
}

async fn create_key(s: &TestServer, body: Value) -> (i64, String) {
    let r = admin(s).post(s.u("/api/v1/api-keys")).json(&body).send().await.unwrap();
    assert_eq!(r.status(), 201, "{body}");
    let v: Value = r.json().await.unwrap();
    (v["id"].as_i64().unwrap(), v["key"].as_str().unwrap().to_string())
}

async fn revoke_key(s: &TestServer, id: i64) {
    let r = admin(s).delete(s.u(&format!("/api/v1/api-keys/{id}"))).send().await.unwrap();
    assert_eq!(r.status(), 204);
}

async fn set_mode(s: &TestServer, mode: &str) {
    let r = admin(s).put(s.u("/api/v1/ingest-auth")).json(&json!({"mode": mode})).send().await.unwrap();
    assert_eq!(r.status(), 200, "mode {mode}");
}

/// Ingest one event with the harness admin key (works in every mode).
async fn send(s: &TestServer, message: &str) {
    let r = admin(s).post(s.u("/api/v1/events")).json(&json!({"message": message})).send().await.unwrap();
    assert!(r.status().is_success(), "ingest {message}: {}", r.status());
}

/// The stream's next frame is the event `message`.
async fn receives(sse: &mut Sse, message: &str) {
    let (kind, data) = sse.next().await;
    assert_eq!(kind, "event", "{data}");
    assert_eq!(serde_json::from_str::<Value>(&data).unwrap()["message"], message);
}

/// The stream sends `unauthorized` with `code` next (after `ready`, if
/// it had not sent that yet), then ends cleanly; no event comes first.
async fn ends_refused(sse: &mut Sse, code: &str) -> String {
    let (mut kind, mut data) = sse.next().await;
    if kind == "ready" {
        (kind, data) = sse.next().await;
    }
    assert_eq!(kind, "unauthorized", "{data}");
    let v: Value = serde_json::from_str(&data).unwrap();
    assert_eq!(v["error"]["code"], code, "{v}");
    assert_eq!(sse.next_or_end().await, None, "the stream ends after `unauthorized`");
    v["error"]["message"].as_str().unwrap().to_string()
}

async fn live_subscribers(s: &TestServer) -> u64 {
    let info: Value = admin(s).get(s.u("/api/v1/system/info")).send().await.unwrap().json().await.unwrap();
    info["liveSubscribers"].as_u64().unwrap()
}

async fn subscribers_drop_to(s: &TestServer, want: u64) {
    let deadline = Instant::now() + PROMPT;
    while live_subscribers(s).await != want {
        assert!(Instant::now() < deadline, "live subscribers never fell to {want}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Revoking a key ends its streams at once, with nothing matched after the
/// revocation sent; other keys' streams and sessions carry on. Rotation
/// keeps the old key's stream until the old key is revoked.
#[tokio::test(flavor = "multi_thread")]
async fn revoking_or_rotating_a_key_reauthorizes_its_streams() {
    let s = start_secure().await;
    let (id, query_key) = create_key(&s, json!({"name": "reader", "scope": "query"})).await;
    let (admin_id, admin_key) = create_key(&s, json!({"name": "ops", "scope": "admin"})).await;
    let (_, session) = sign_in(&s, "admin", "correct horse").await;
    let mut by_query = Sse::open_as(&by_key(&query_key), &s, "").await;
    let mut by_admin = Sse::open_as(&by_key(&admin_key), &s, "").await;
    let mut by_session = Sse::open_as(&session, &s, "").await;
    send(&s, "before").await;
    for sse in [&mut by_query, &mut by_admin, &mut by_session] {
        receives(sse, "before").await;
    }

    revoke_key(&s, id).await;
    let revoked = Instant::now();
    send(&s, "after revoke").await;
    assert_eq!(ends_refused(&mut by_query, "unauthorized").await, "invalid API key");
    assert!(revoked.elapsed() < PROMPT, "ended by the revocation, not the periodic recheck");
    receives(&mut by_admin, "after revoke").await;
    receives(&mut by_session, "after revoke").await;
    assert_eq!(Sse::connect(&by_key(&query_key), &s, "").await.err(), Some(401));

    // Rotation leaves the old key active, so its stream stays; revoking the
    // old key ends it and not the replacement's.
    let r = admin(&s).post(s.u(&format!("/api/v1/api-keys/{admin_id}/rotate"))).json(&json!({})).send().await;
    let rotated: Value = r.unwrap().json().await.unwrap();
    let mut by_rotated = Sse::open_as(&by_key(rotated["key"].as_str().unwrap()), &s, "").await;
    send(&s, "after rotate").await;
    receives(&mut by_admin, "after rotate").await;
    receives(&mut by_rotated, "after rotate").await;
    revoke_key(&s, admin_id).await;
    send(&s, "old key revoked").await;
    ends_refused(&mut by_admin, "unauthorized").await;
    receives(&mut by_rotated, "old key revoked").await;
    receives(&mut by_session, "after rotate").await;
    receives(&mut by_session, "old key revoked").await;
    drop((by_rotated, by_session));
    subscribers_drop_to(&s, 0).await;
    s.stop().await;
}

/// Logout ends that session's streams at once; a password change or a
/// deactivation ends every stream of that account only.
#[tokio::test(flavor = "multi_thread")]
async fn logout_password_changes_and_deactivation_end_session_streams() {
    let s = start_secure().await;
    let m = &s.state().metadata;
    let ops = m.create_user("ops", None, &server::auth::hash_password("ops pass").unwrap()).unwrap();
    let dev = m.create_user("dev", None, &server::auth::hash_password("dev pass").unwrap()).unwrap();
    let (cookie, first) = sign_in(&s, "admin", "correct horse").await;
    let (_, second) = sign_in(&s, "admin", "correct horse").await;
    let (_, by_ops) = sign_in(&s, "ops", "ops pass").await;
    let (_, by_dev) = sign_in(&s, "dev", "dev pass").await;
    let mut streams: Vec<Sse> = Vec::new();
    for client in [&first, &second, &by_ops, &by_dev] {
        streams.push(Sse::open_as(client, &s, "").await);
    }
    let [mut first_s, mut second_s, mut ops_s, mut dev_s] = streams.try_into().ok().unwrap();

    let r = reqwest::Client::new().post(s.u("/api/v1/auth/logout")).header("cookie", &cookie).send().await.unwrap();
    assert_eq!(r.status(), 200);
    let started = Instant::now();
    send(&s, "after logout").await;
    assert_eq!(ends_refused(&mut first_s, "unauthorized").await, "the session has ended; sign in again");
    assert!(started.elapsed() < PROMPT);
    for sse in [&mut second_s, &mut ops_s, &mut dev_s] {
        receives(sse, "after logout").await;
    }

    let hash = server::auth::hash_password("ops pass 2").unwrap();
    assert_eq!(server::auth::set_account_password(s.state(), ops.id, hash).await.unwrap(), 1);
    send(&s, "after password change").await;
    ends_refused(&mut ops_s, "unauthorized").await;
    receives(&mut dev_s, "after password change").await;
    assert_eq!(server::auth::set_account_active(s.state(), dev.id, false).await.unwrap(), 1);
    send(&s, "after deactivation").await;
    ends_refused(&mut dev_s, "unauthorized").await;
    for message in ["after password change", "after deactivation"] {
        receives(&mut second_s, message).await;
    }
    assert!(started.elapsed() < PROMPT * 2, "every change ended its streams on notification");
    drop(second_s);
    subscribers_drop_to(&s, 0).await;
    s.stop().await;
}

/// Expiry has no notification: a key or session that expires ends its
/// stream within `REAUTHORIZE_EVERY` of its expiry, and it delivers until
/// then.
#[tokio::test(flavor = "multi_thread")]
async fn expired_keys_and_sessions_end_their_streams_within_the_recheck_interval() {
    let ttl = Duration::from_secs(2);
    let s = start_secure_with(|c| c.auth.session_ttl = ttl).await;
    let (_, session) = sign_in(&s, "admin", "correct horse").await;
    let signed_in = Instant::now();
    let expires_at = metadata::now_ms() + 1_500;
    let (_, key) = create_key(&s, json!({"name": "short", "scope": "query", "expiresAt": expires_at})).await;
    let mut by_session = Sse::open_as(&session, &s, "").await;
    let mut by_expiring = Sse::open_as(&by_key(&key), &s, "").await;
    send(&s, "while valid").await;
    receives(&mut by_session, "while valid").await;
    receives(&mut by_expiring, "while valid").await;

    assert!(ends_refused(&mut by_expiring, "unauthorized").await.contains("expired"));
    let late = metadata::now_ms() - expires_at;
    assert!(late >= 0, "not before its expiry");
    assert!(late <= (REAUTHORIZE_EVERY + PROMPT).as_millis() as i64, "{late} ms after its expiry");
    ends_refused(&mut by_session, "unauthorized").await;
    let after = signed_in.elapsed();
    assert!(after >= ttl && after <= ttl + REAUTHORIZE_EVERY + PROMPT, "{after:?} after sign-in");
    s.stop().await;
}

/// Ingestion-key changes (the first ingest key latching protection, mode
/// changes, the last ingest key revoked) neither end query streams nor let
/// an anonymous or ingest-only caller open one, in any state.
#[tokio::test(flavor = "multi_thread")]
async fn ingest_key_transitions_never_open_or_close_query_streams() {
    let s = start_secure().await;
    let (_, query_key) = create_key(&s, json!({"name": "reader", "scope": "query"})).await;
    let (_, session) = sign_in(&s, "admin", "correct horse").await;
    let mut streams = vec![
        Sse::open_as(&admin(&s), &s, "").await,
        Sse::open_as(&by_key(&query_key), &s, "").await,
        Sse::open_as(&session, &s, "").await,
    ];
    let anonymous = reqwest::Client::new();
    let mut ingest_key = None;
    let steps = ["no ingest key", "first ingest key", "open", "required", "automatic", "last ingest key revoked"];
    for (i, step) in steps.into_iter().enumerate() {
        match step {
            "first ingest key" => ingest_key = Some(create_key(&s, json!({"name": "sender", "scope": "ingest"})).await),
            "open" | "required" | "automatic" => set_mode(&s, step).await,
            "last ingest key revoked" => revoke_key(&s, ingest_key.as_ref().unwrap().0).await,
            _ => {}
        }
        send(&s, step).await;
        for sse in &mut streams {
            receives(sse, step).await;
        }
        assert_eq!(Sse::connect(&anonymous, &s, "").await.err(), Some(401), "anonymous, {step}");
        if let Some((_, key)) = &ingest_key {
            let refused = Sse::connect(&by_key(key), &s, "").await.err();
            assert_eq!(refused, Some(if i < 5 { 403 } else { 401 }), "ingest key, {step}");
        }
    }
    assert_eq!(live_subscribers(&s).await, 3, "refused opens hold no subscription");
    drop(streams);
    s.stop().await;
}

/// A stream opened while its key is being revoked is either refused or
/// ends once the revocation returns: it never outlives it, whichever won.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn streams_admitted_while_a_key_is_revoked_cannot_outlive_it() {
    let s = std::sync::Arc::new(start_secure().await);
    let (mut admitted, mut refused) = (0, 0);
    for round in 0..10 {
        let (id, key) = create_key(&s, json!({"name": format!("racer {round}"), "scope": "query"})).await;
        let opens: Vec<_> = (0..6)
            .map(|_| {
                let (s, client) = (s.clone(), by_key(&key));
                tokio::spawn(async move { Sse::connect(&client, &s, "").await })
            })
            .collect();
        revoke_key(&s, id).await;
        let revoked = Instant::now();
        send(&s, "after the race").await;
        for open in opens {
            match open.await.unwrap() {
                Ok(mut sse) => {
                    ends_refused(&mut sse, "unauthorized").await;
                    admitted += 1;
                }
                Err(status) => {
                    assert_eq!(status, 401);
                    refused += 1;
                }
            }
        }
        assert!(revoked.elapsed() < PROMPT * 2, "round {round}");
    }
    assert!(admitted + refused == 60, "{admitted} admitted, {refused} refused");
    subscribers_drop_to(&s, 0).await;
    std::sync::Arc::into_inner(s).unwrap().stop().await;
}

/// When a session cannot be rechecked (its store is unreadable) the stream
/// fails closed with `unavailable` and a retry, never carrying on unchecked.
/// Many changes wake a stream once each at most, from the cache.
#[tokio::test(flavor = "multi_thread")]
async fn unavailable_authority_fails_closed_and_changes_are_coalesced() {
    let s = start_secure().await;
    let (_, session) = sign_in(&s, "admin", "correct horse").await;
    let mut sse = Sse::open_as(&session, &s, "").await;
    let lookups = async || -> Value {
        let info: Value = admin(&s).get(s.u("/api/v1/system/info")).send().await.unwrap().json().await.unwrap();
        info["sessionAuth"].clone()
    };
    let before = lookups().await;
    for i in 0..50 {
        create_key(&s, json!({"name": format!("burst {i}"), "scope": "admin"})).await;
    }
    send(&s, "after the burst").await;
    receives(&mut sse, "after the burst").await;
    let after = lookups().await;
    let delta = |k: &str| after[k].as_u64().unwrap() - before[k].as_u64().unwrap();
    assert_eq!(delta("lookups"), 0, "rechecks of a cached session read no SQLite");
    assert!(delta("cacheHits") <= 50 + 2, "at most one recheck per change: {}", delta("cacheHits"));

    let db = rusqlite::Connection::open(s.data_dir().join(server::datadir::METADATA_DB)).unwrap();
    db.execute_batch("ALTER TABLE sessions RENAME TO sessions_away").unwrap();
    s.state().sessions.clear();
    send(&s, "unchecked").await;
    let (kind, data) = sse.next().await;
    assert_eq!(kind, "unavailable", "{data}");
    assert_eq!(serde_json::from_str::<Value>(&data).unwrap()["error"]["code"], "internal");
    assert_eq!(sse.retry, Some(2000));
    assert_eq!(sse.next_or_end().await, None);
    db.execute_batch("ALTER TABLE sessions_away RENAME TO sessions").unwrap();
    let mut again = Sse::open_as(&session, &s, "").await;
    send(&s, "readable again").await;
    receives(&mut again, "readable again").await;
    drop((again, db));
    s.stop().await;
}

/// Across a restart: a revoked key or ended session cannot open a stream
/// again, and a valid session can.
#[tokio::test(flavor = "multi_thread")]
async fn revoked_credentials_stay_refused_for_streams_after_a_restart() {
    let s = start_secure().await;
    let (id, key) = create_key(&s, json!({"name": "reader", "scope": "query"})).await;
    let (ended, ended_client) = sign_in(&s, "admin", "correct horse").await;
    let (_, kept) = sign_in(&s, "admin", "correct horse").await;
    revoke_key(&s, id).await;
    let r = reqwest::Client::new().post(s.u("/api/v1/auth/logout")).header("cookie", &ended).send().await.unwrap();
    assert_eq!(r.status(), 200);
    let mut s = s.restart().await;
    s.client = reqwest::Client::new();
    assert_eq!(Sse::connect(&by_key(&key), &s, "").await.err(), Some(401));
    assert_eq!(Sse::connect(&ended_client, &s, "").await.err(), Some(401));
    let mut sse = Sse::open_as(&kept, &s, "").await;
    send(&s, "after restart").await;
    receives(&mut sse, "after restart").await;
    drop(sse);
    s.stop().await;
}
