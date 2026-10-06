//! HTTP-level tests: the real router, real Tantivy index and real SQLite database in a temp
//! directory, driven in-process with `tower::ServiceExt::oneshot`.

use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::http::{header, Method, Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;

use crate::config::Config;
use crate::state::AppState;

struct TestApp {
    _dir: tempfile::TempDir,
    state: AppState,
    router: Router,
    token: String,
}

struct Resp {
    status: StatusCode,
    body: Value,
}

impl TestApp {
    async fn new() -> Self {
        Self::with_development(true).await
    }

    async fn with_development(development: bool) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let store = storage::EventStore::open(&dir.path().join("index")).unwrap();
        let metadata = metadata::MetadataStore::open(&dir.path().join("metadata.db"))
            .await
            .unwrap();
        let config = Config {
            development,
            storage: crate::config::StorageConfig { path: dir.path().to_path_buf() },
            ..Config::default()
        };
        let state = AppState {
            store: store.clone(),
            ingest: ingest::start_ingest_worker(store),
            metadata: Arc::new(metadata),
            config,
        };
        let router = super::router_with_state(state.clone()).with_state(state.clone());
        let mut app = Self { _dir: dir, state, router, token: String::new() };
        let login = app
            .post_anon("/api/auth/login", json!({"username": "admin", "password": "admin"}))
            .await;
        assert_eq!(login.status, StatusCode::OK, "{}", login.body);
        app.token = login.body["token"].as_str().unwrap().to_string();
        app
    }

    async fn send(&self, req: Request<Body>) -> Resp {
        let res = self.router.clone().oneshot(req).await.unwrap();
        let status = res.status();
        let bytes = res.into_body().collect().await.unwrap().to_bytes();
        let body = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes)
                .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).into_owned()))
        };
        Resp { status, body }
    }

    fn req(&self, method: Method, uri: &str, body: Option<Value>, auth: bool) -> Request<Body> {
        let mut b = Request::builder().method(method).uri(uri);
        if auth {
            b = b.header("x-observatory-token", &self.token);
        }
        match body {
            Some(v) => b
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(v.to_string()))
                .unwrap(),
            None => b.body(Body::empty()).unwrap(),
        }
    }

    async fn get(&self, uri: &str) -> Resp {
        self.send(self.req(Method::GET, uri, None, true)).await
    }
    async fn post(&self, uri: &str, body: Value) -> Resp {
        self.send(self.req(Method::POST, uri, Some(body), true)).await
    }
    async fn put(&self, uri: &str, body: Value) -> Resp {
        self.send(self.req(Method::PUT, uri, Some(body), true)).await
    }
    async fn delete(&self, uri: &str) -> Resp {
        self.send(self.req(Method::DELETE, uri, None, true)).await
    }
    async fn post_anon(&self, uri: &str, body: Value) -> Resp {
        self.send(self.req(Method::POST, uri, Some(body), false)).await
    }
    async fn get_anon(&self, uri: &str) -> Resp {
        self.send(self.req(Method::GET, uri, None, false)).await
    }

    /// Wait for the background ingest worker to flush `n` events into the index.
    async fn wait_for_events(&self, n: usize) {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let _ = self.state.store.reload();
            let count = self.state.store.event_count().unwrap();
            if count >= n {
                assert_eq!(count, n, "more events than expected were written");
                return;
            }
            assert!(Instant::now() < deadline, "timed out waiting for {n} events (have {count})");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    async fn ingest(&self, events: Value) {
        let n = events.as_array().unwrap().len();
        let before = self.state.store.event_count().unwrap();
        let r = self.post("/api/events/bulk", events).await;
        assert_eq!(r.status, StatusCode::ACCEPTED, "{}", r.body);
        assert_eq!(r.body["accepted"], n);
        self.wait_for_events(before + n).await;
    }

    async fn messages(&self, q: &str) -> Vec<String> {
        let r = self.get(&format!("/api/events?q={}", encode(q))).await;
        assert_eq!(r.status, StatusCode::OK, "{q}: {}", r.body);
        r.body["events"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["message"].as_str().unwrap_or_default().to_string())
            .collect()
    }
}

fn encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn error_code(r: &Resp) -> &str {
    r.body["error"]["code"].as_str().unwrap_or("<none>")
}

fn sample_events() -> Value {
    json!([
        {"timestamp": "2026-01-01T10:00:00Z", "level": "info", "message": "user signed in",
         "service": "api", "attributes": {"userId": 1}},
        {"timestamp": "2026-01-01T10:01:00Z", "level": "warn", "message": "slow database query",
         "service": "api", "attributes": {"userId": 2}},
        {"timestamp": "2026-01-01T10:02:00Z", "level": "error", "message": "Payment failed",
         "service": "billing", "attributes": {"customerId": 42, "provider": "stripe"}},
        {"timestamp": "2026-01-01T10:03:00Z", "level": "fatal", "message": "billing crashed",
         "service": "billing", "exception": "at Billing.run()"}
    ])
}

// --- auth -------------------------------------------------------------------

#[tokio::test]
async fn login_rejects_bad_credentials() {
    let app = TestApp::new().await;
    let r = app.post_anon("/api/auth/login", json!({"username": "admin", "password": "nope"})).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    assert_eq!(error_code(&r), "unauthorized");

    let r = app.post_anon("/api/auth/login", json!({"username": "  ", "password": "x"})).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    assert_eq!(error_code(&r), "invalid_request");
}

#[tokio::test]
async fn login_returns_token_account_and_expiry() {
    let app = TestApp::new().await;
    let r = app.post_anon("/api/auth/login", json!({"username": " admin ", "password": "admin"})).await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(r.body["token"].is_string());
    assert_eq!(r.body["account"]["username"], "admin");
    assert!(r.body["account"].get("passwordHash").is_none(), "hash must never be serialised");
    assert!(r.body["expiresAt"].is_string());
}

#[tokio::test]
async fn protected_routes_require_a_session() {
    let app = TestApp::new().await;
    for uri in [
        "/api/events",
        "/api/query?q=",
        "/api/traces",
        "/api/filters",
        "/api/dashboards",
        "/api/settings",
        "/api/about",
        "/api/metrics",
        "/api/alerts",
        "/api/accounts",
        "/api/auth/me",
    ] {
        let r = app.get_anon(uri).await;
        assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{uri}");
        assert_eq!(error_code(&r), "unauthorized");
    }
}

#[tokio::test]
async fn session_token_accepted_via_header_bearer_and_query() {
    let app = TestApp::new().await;
    assert_eq!(app.get("/api/auth/me").await.body["username"], "admin");

    let bearer = Request::get("/api/auth/me")
        .header(header::AUTHORIZATION, format!("Bearer {}", app.token))
        .body(Body::empty())
        .unwrap();
    assert_eq!(app.send(bearer).await.status, StatusCode::OK);

    // EventSource can't set headers, so the stream endpoint relies on ?token=.
    let r = app.get_anon(&format!("/api/auth/me?foo=1&token={}", app.token)).await;
    assert_eq!(r.status, StatusCode::OK);

    let r = app.get_anon("/api/auth/me?token=bogus").await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn api_keys_are_not_accepted_as_sessions() {
    let app = TestApp::new().await;
    let key = app.post("/api/accounts/api-keys", json!({"name": "ci"})).await.body["key"]
        .as_str()
        .unwrap()
        .to_string();
    let req = Request::get("/api/auth/me")
        .header(header::AUTHORIZATION, format!("Bearer {key}"))
        .body(Body::empty())
        .unwrap();
    assert_eq!(app.send(req).await.status, StatusCode::UNAUTHORIZED);
    let r = app.get_anon(&format!("/api/auth/me?token={key}")).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn logout_invalidates_session() {
    let app = TestApp::new().await;
    let r = app.post("/api/auth/logout", json!({})).await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);
    assert_eq!(app.get("/api/auth/me").await.status, StatusCode::UNAUTHORIZED);
}

// --- ingest auth ------------------------------------------------------------

#[tokio::test]
async fn development_mode_allows_anonymous_ingest() {
    let app = TestApp::with_development(true).await;
    let r = app.post_anon("/api/events", json!({"message": "hi"})).await;
    assert_eq!(r.status, StatusCode::ACCEPTED);
}

#[tokio::test]
async fn production_mode_requires_valid_api_key() {
    let app = TestApp::with_development(false).await;
    let ingest_uris = ["/api/events", "/api/metrics", "/v1/logs", "/v1/traces"];
    for uri in ingest_uris {
        let r = app.post_anon(uri, json!({})).await;
        assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{uri}");
        assert!(r.body["error"]["message"].as_str().unwrap().contains("API key required"));
    }

    // A session token is not an API key.
    let r = app.post("/api/events", json!({"message": "x"})).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);

    let bad = Request::post("/api/events")
        .header("x-api-key", "obs_wrong")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(r#"{"message":"x"}"#))
        .unwrap();
    let r = app.send(bad).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    assert_eq!(r.body["error"]["message"], "Invalid API key");

    let key = app.post("/api/accounts/api-keys", json!({"name": "ci"})).await.body["key"]
        .as_str()
        .unwrap()
        .to_string();
    for (name, value) in [
        ("x-api-key", key.clone()),
        ("authorization", format!("Bearer {key}")),
        ("authorization", format!("ApiKey {key}")),
    ] {
        let req = Request::post("/api/events")
            .header(name, value.clone())
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(r#"{"message":"x"}"#))
            .unwrap();
        assert_eq!(app.send(req).await.status, StatusCode::ACCEPTED, "{name}: {value}");
    }
}

#[tokio::test]
async fn deleted_api_key_stops_working() {
    let app = TestApp::with_development(false).await;
    let created = app.post("/api/accounts/api-keys", json!({"name": "ci"})).await.body;
    let key = created["key"].as_str().unwrap().to_string();
    let id = created["id"].as_str().unwrap();
    assert_eq!(app.delete(&format!("/api/accounts/api-keys/{id}")).await.status, StatusCode::NO_CONTENT);

    let req = Request::post("/api/events")
        .header("x-api-key", key)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(r#"{"message":"x"}"#))
        .unwrap();
    assert_eq!(app.send(req).await.status, StatusCode::UNAUTHORIZED);
}

// --- events -----------------------------------------------------------------

#[tokio::test]
async fn ingest_single_event_returns_id_and_is_searchable() {
    let app = TestApp::new().await;
    let r = app
        .post("/api/events", json!({"level": "error", "message": "boom", "service": "svc"}))
        .await;
    assert_eq!(r.status, StatusCode::ACCEPTED);
    let id = r.body["id"].as_str().unwrap().to_string();
    app.wait_for_events(1).await;

    let r = app.get("/api/events").await;
    let e = &r.body["events"][0];
    assert_eq!(e["id"], id);
    assert_eq!(e["level"], "error");
    assert_eq!(e["eventType"], "log");
    assert_eq!(e["service"], "svc");
}

#[tokio::test]
async fn malformed_ingest_body_is_rejected() {
    let app = TestApp::new().await;
    let req = Request::post("/api/events")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from("{not json"))
        .unwrap();
    assert!(app.send(req).await.status.is_client_error());

    let r = app.post("/api/events/bulk", json!({"message": "not an array"})).await;
    assert!(r.status.is_client_error());

    let r = app.post("/api/events", json!({"eventType": "banana"})).await;
    assert!(r.status.is_client_error());
}

#[tokio::test]
async fn filtering_via_events_endpoint() {
    let app = TestApp::new().await;
    app.ingest(sample_events()).await;

    assert_eq!(app.messages("").await.len(), 4);
    assert_eq!(
        app.messages(r#"service = "billing""#).await,
        ["billing crashed", "Payment failed"]
    );
    assert_eq!(app.messages("level >= error").await, ["billing crashed", "Payment failed"]);
    assert_eq!(app.messages("customerId = 42").await, ["Payment failed"]);
    assert_eq!(app.messages("database").await, ["slow database query"]);
}

#[tokio::test]
async fn events_endpoint_time_range_and_limit() {
    let app = TestApp::new().await;
    app.ingest(sample_events()).await;

    let r = app
        .get("/api/events?from=2026-01-01T10:01:00Z&to=2026-01-01T10:02:00Z")
        .await;
    let msgs: Vec<_> = r.body["events"].as_array().unwrap().iter().map(|e| e["message"].clone()).collect();
    assert_eq!(msgs, [json!("Payment failed"), json!("slow database query")]);

    let r = app.get("/api/events?limit=1").await;
    assert_eq!(r.body["events"].as_array().unwrap().len(), 1);
    assert!(r.body["nextCursor"].is_string());

    // limit=0 is clamped up to 1 rather than returning nothing.
    let r = app.get("/api/events?limit=0").await;
    assert_eq!(r.body["events"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn events_endpoint_follows_cursor_to_next_page() {
    let app = TestApp::new().await;
    app.ingest(sample_events()).await;

    let page1 = app.get("/api/events?limit=2").await.body;
    let cursor = page1["nextCursor"].as_str().unwrap();
    let page2 = app.get(&format!("/api/events?limit=2&cursor={cursor}")).await.body;
    let all: Vec<_> = page1["events"]
        .as_array()
        .unwrap()
        .iter()
        .chain(page2["events"].as_array().unwrap())
        .map(|e| e["message"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(all, ["billing crashed", "Payment failed", "slow database query", "user signed in"]);
}

#[tokio::test]
async fn invalid_queries_return_400_with_details() {
    let app = TestApp::new().await;
    let r = app.get(&format!("/api/events?q={}", encode("service = "))).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    assert_eq!(error_code(&r), "invalid_query");
    assert!(r.body["error"]["position"].is_number());

    let r = app.get(&format!("/api/events?q={}", encode("level >= loud"))).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    assert_eq!(error_code(&r), "invalid_query");

    let r = app.get(&format!("/api/events?q={}", encode("| count"))).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    assert!(r.body["error"]["message"].as_str().unwrap().contains("/api/query"));

    let r = app.get("/api/events?cursor=garbage").await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    assert_eq!(error_code(&r), "invalid_cursor");

    let r = app.get("/api/events?from=yesterday").await;
    assert!(r.status.is_client_error());
}

#[tokio::test]
async fn stacktrace_is_lifted_out_of_payload() {
    let app = TestApp::new().await;
    app.ingest(sample_events()).await;
    let r = app.get(&format!("/api/events?q={}", encode("level = fatal"))).await;
    let e = &r.body["events"][0];
    assert_eq!(e["stacktrace"], "at Billing.run()");
    assert!(e["attributes"].get("exception").is_none());
}

// --- /api/query -------------------------------------------------------------

#[tokio::test]
async fn query_endpoint_returns_events_or_aggregations() {
    let app = TestApp::new().await;
    app.ingest(sample_events()).await;

    let r = app.get(&format!("/api/query?q={}", encode("service = api"))).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.body["events"].as_array().unwrap().len(), 2);

    let r = app.get(&format!("/api/query?q={}", encode("| count"))).await;
    assert_eq!(r.body, json!({"result": {"type": "number", "value": 4.0}}));

    let r = app.get(&format!("/api/query?q={}", encode("| count by service"))).await;
    assert_eq!(
        r.body["result"]["points"],
        json!([{"key": "api", "value": 2.0}, {"key": "billing", "value": 2.0}])
    );

    let r = app
        .get(&format!(
            "/api/query?q={}&from=2026-01-01T10:02:00Z",
            encode("| count by time(1h)")
        ))
        .await;
    assert_eq!(r.body["result"]["type"], "timeSeries");
    assert_eq!(r.body["result"]["points"][0]["value"], 2.0);

    let r = app.get(&format!("/api/query?q={}", encode("| median(x)"))).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
}

// --- metrics ----------------------------------------------------------------

#[tokio::test]
async fn metrics_ingest_list_and_series() {
    let app = TestApp::new().await;
    let r = app.post("/api/metrics", json!({"name": " ", "value": 1})).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);

    let r = app
        .post(
            "/api/metrics/bulk",
            json!([
                {"name": "cpu", "value": 0.5, "unit": "ratio", "service": "a", "timestamp": "2026-01-01T00:00:00Z"},
                {"name": "cpu", "value": 0.7, "unit": "ratio", "service": "b", "timestamp": "2026-01-01T00:01:00Z"},
                {"name": "cpu", "value": 0.9, "unit": "ratio", "service": "a", "timestamp": "2026-01-01T00:02:00Z"},
                {"name": "mem", "value": 1024, "timestamp": "2026-01-01T00:00:30Z"}
            ]),
        )
        .await;
    assert_eq!(r.status, StatusCode::ACCEPTED);
    assert_eq!(r.body["accepted"], 4);
    app.wait_for_events(4).await;

    let r = app.get("/api/metrics").await;
    let metrics = r.body["metrics"].as_array().unwrap();
    assert_eq!(metrics.len(), 2);
    let cpu = &metrics[0];
    assert_eq!(cpu["name"], "cpu");
    assert_eq!(cpu["pointCount"], 3);
    assert_eq!(cpu["lastValue"], 0.9, "latest point wins");
    assert_eq!(cpu["unit"], "ratio");
    assert_eq!(metrics[1]["name"], "mem");
    assert_eq!(metrics[1]["lastValue"], 1024.0);

    let r = app.get("/api/metrics/series?name=cpu").await;
    let values: Vec<_> = r.body["points"].as_array().unwrap().iter().map(|p| p["value"].as_f64().unwrap()).collect();
    assert_eq!(values, [0.5, 0.7, 0.9], "chronological order");

    let r = app.get("/api/metrics/series?name=cpu&service=a").await;
    assert_eq!(r.body["points"].as_array().unwrap().len(), 2);

    let r = app.get("/api/metrics/series?name=nope").await;
    assert_eq!(r.body["points"], json!([]));

    // Metrics are events too, but aren't confused with logs.
    assert!(app.messages("event_type = log").await.is_empty());
}

// --- OTLP & traces ----------------------------------------------------------

fn otlp_traces() -> Value {
    let span = |trace: &str, id: &str, parent: &str, name: &str, start: u64, end: u64, code: Option<i64>| {
        let mut s = json!({
            "traceId": trace, "spanId": id, "parentSpanId": parent, "name": name,
            "startTimeUnixNano": start.to_string(), "endTimeUnixNano": end.to_string(),
            "attributes": [{"key": "http.method", "value": {"stringValue": "GET"}}]
        });
        if let Some(code) = code {
            s["status"] = json!({"code": code});
        }
        s
    };
    let base: u64 = 1_767_261_600_000_000_000; // 2026-01-01T10:00:00Z
    json!({"resourceSpans": [{
        "resource": {"attributes": [{"key": "service.name", "value": {"stringValue": "checkout"}}]},
        "scopeSpans": [{"spans": [
            span("aaaa", "0001", "", "POST /checkout", base, base + 300_000_000, Some(2)),
            span("aaaa", "0002", "0001", "charge card", base + 10_000_000, base + 200_000_000, None),
            span("bbbb", "0003", "", "GET /health", base + 60_000_000_000, base + 60_001_000_000, None),
        ]}]
    }]})
}

#[tokio::test]
async fn otlp_traces_feed_trace_list_and_detail() {
    let app = TestApp::new().await;
    let r = app.post("/v1/traces", otlp_traces()).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.body["accepted"], 3);

    let logs = json!({"resourceLogs": [{
        "resource": {"attributes": [{"key": "service.name", "value": {"stringValue": "checkout"}}]},
        "scopeLogs": [{"logRecords": [{
            "timeUnixNano": "1767261600050000000", "severityText": "WARN",
            "body": {"stringValue": "card declined, retrying"}, "traceId": "aaaa", "spanId": "0002"
        }]}]
    }]});
    let r = app.post("/v1/logs", logs).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.body["accepted"], 1);
    app.wait_for_events(4).await;

    let r = app.get("/api/traces").await;
    let traces = r.body["traces"].as_array().unwrap();
    assert_eq!(traces.len(), 2, "logs don't create traces");
    assert_eq!(traces[0]["traceId"], "bbbb", "newest first");
    let checkout = &traces[1];
    assert_eq!(checkout["rootOperation"], "POST /checkout");
    assert_eq!(checkout["service"], "checkout");
    assert_eq!(checkout["spanCount"], 2);
    assert_eq!(checkout["durationNs"], 300_000_000);
    assert_eq!(checkout["status"], "2");
    assert_eq!(traces[0]["status"], "unset");

    let r = app.get(&format!("/api/traces?q={}", encode("duration_ns > 250000000"))).await;
    let ids: Vec<_> = r.body["traces"].as_array().unwrap().iter().map(|t| t["traceId"].clone()).collect();
    assert_eq!(ids, [json!("aaaa")]);

    let r = app.get("/api/traces?limit=1").await;
    assert_eq!(r.body["traces"].as_array().unwrap().len(), 1);

    let r = app.get("/api/traces/aaaa").await;
    assert_eq!(r.status, StatusCode::OK);
    let spans: Vec<_> = r.body["spans"].as_array().unwrap().iter().map(|s| s["message"].clone()).collect();
    assert_eq!(spans, [json!("POST /checkout"), json!("charge card")]);
    assert_eq!(r.body["logs"][0]["message"], "card declined, retrying");
    assert_eq!(r.body["logs"][0]["level"], "warning");

    let r = app.get("/api/traces/doesnotexist").await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn otlp_rejects_malformed_payload() {
    let app = TestApp::new().await;
    let r = app.post("/v1/logs", json!({"resourceLogs": "nope"})).await;
    assert!(r.status.is_client_error());
}

// --- saved filters ----------------------------------------------------------

#[tokio::test]
async fn saved_filters_crud() {
    let app = TestApp::new().await;
    let r = app.post("/api/filters", json!({"name": "  ", "query": "x"})).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);

    let r = app.post("/api/filters", json!({"name": " Errors ", "query": "level = error"})).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.body["name"], "Errors", "name is trimmed");
    let id = r.body["id"].as_str().unwrap().to_string();

    let r = app.get("/api/filters").await;
    assert_eq!(r.body.as_array().unwrap().len(), 1);
    assert_eq!(r.body[0]["query"], "level = error");

    assert_eq!(app.delete(&format!("/api/filters/{id}")).await.status, StatusCode::OK);
    assert_eq!(app.get("/api/filters").await.body, json!([]));
}

// --- dashboards -------------------------------------------------------------

#[tokio::test]
async fn dashboards_and_widgets() {
    let app = TestApp::new().await;
    app.ingest(sample_events()).await;

    let d = app.post("/api/dashboards", json!({"name": "Ops"})).await.body;
    let id = d["id"].as_str().unwrap().to_string();

    let w = app
        .post(
            &format!("/api/dashboards/{id}/widgets"),
            json!({"title": "Errors", "query": "level >= error | count", "visualization": "number"}),
        )
        .await;
    assert_eq!(w.status, StatusCode::OK, "{}", w.body);
    assert_eq!(w.body["position"], json!({"x": 0, "y": 0, "w": 1, "h": 1}), "default position");
    let wid = w.body["id"].as_str().unwrap().to_string();

    let r = app
        .post(
            &format!("/api/dashboards/{id}/widgets"),
            json!({"title": "x", "query": "| count", "visualization": "pie"}),
        )
        .await;
    assert!(r.status.is_client_error(), "unknown visualization rejected");

    let detail = app.get(&format!("/api/dashboards/{id}")).await.body;
    assert_eq!(detail["name"], "Ops");
    assert_eq!(detail["widgets"].as_array().unwrap().len(), 1);

    let r = app
        .put(
            &format!("/api/dashboards/{id}/widgets/{wid}"),
            json!({"title": "Fatal", "query": "level = fatal | count", "visualization": "table",
                   "position": {"x": 1, "y": 2, "w": 3, "h": 4}}),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK);
    let widgets = app.get(&format!("/api/dashboards/{id}/widgets")).await.body;
    assert_eq!(widgets[0]["title"], "Fatal");
    assert_eq!(widgets[0]["visualization"], "table");

    let r = app.get(&format!("/api/dashboards/{id}/query?q={}", encode("level >= error | count"))).await;
    assert_eq!(r.body["result"]["value"], 2.0);
    let r = app.get(&format!("/api/dashboards/{id}/query?q={}", encode("level >= error"))).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "dashboard queries need an aggregation");

    assert_eq!(app.put(&format!("/api/dashboards/{id}"), json!({"name": "Ops 2"})).await.status, StatusCode::OK);
    assert_eq!(app.get("/api/dashboards").await.body[0]["name"], "Ops 2");

    assert_eq!(app.delete(&format!("/api/dashboards/{id}/widgets/{wid}")).await.status, StatusCode::OK);
    assert_eq!(app.delete(&format!("/api/dashboards/{id}")).await.status, StatusCode::OK);
    let r = app.get(&format!("/api/dashboards/{id}")).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    assert_eq!(error_code(&r), "not_found");
}

// --- settings & about -------------------------------------------------------

#[tokio::test]
async fn retention_settings() {
    let app = TestApp::new().await;
    let r = app.put("/api/settings", json!({"retentionDays": 14})).await;
    assert_eq!(r.body, json!({"retentionDays": 14}));

    let r = app.put("/api/settings", json!({"retentionDays": "7"})).await;
    assert_eq!(r.body["retentionDays"], 7);

    let r = app.put("/api/settings", json!({"retentionDays": "forever"})).await;
    assert_eq!(r.body["retentionDays"], Value::Null);

    let r = app.put("/api/settings", json!({"retentionDays": [1]})).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);

    // Omitting the field leaves the value alone.
    app.put("/api/settings", json!({"retentionDays": 5})).await;
    let r = app.put("/api/settings", json!({})).await;
    assert_eq!(r.body["retentionDays"], 5);
    assert_eq!(app.get("/api/settings").await.body["retentionDays"], 5);
}

#[tokio::test]
async fn null_retention_means_forever() {
    let app = TestApp::new().await;
    app.put("/api/settings", json!({"retentionDays": 3})).await;
    let r = app.put("/api/settings", json!({"retentionDays": null})).await;
    assert_eq!(r.body["retentionDays"], Value::Null);
}

#[tokio::test]
async fn invalid_retention_value_is_rejected() {
    let app = TestApp::new().await;
    app.put("/api/settings", json!({"retentionDays": 5})).await;
    for bad in [json!("abc"), json!(7.5)] {
        let r = app.put("/api/settings", json!({"retentionDays": bad})).await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{bad}");
    }
    assert_eq!(app.get("/api/settings").await.body["retentionDays"], 5);
}

#[tokio::test]
async fn about_reports_instance_info() {
    let app = TestApp::new().await;
    app.ingest(sample_events()).await;
    let r = app.get("/api/about").await;
    assert_eq!(r.body["name"], "Observatory");
    assert_eq!(r.body["eventCount"], 4);
    assert_eq!(r.body["development"], true);
    assert_eq!(r.body["version"], env!("CARGO_PKG_VERSION"));
}

// --- accounts ---------------------------------------------------------------

#[tokio::test]
async fn account_management() {
    let app = TestApp::new().await;
    let r = app.post("/api/accounts", json!({"username": "bob", "password": "abc"})).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "password too short");

    let r = app.post("/api/accounts", json!({"username": "", "password": "abcd"})).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);

    let r = app.post("/api/accounts", json!({"username": "bob", "password": "abcd"})).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.body["displayName"], "bob", "display name defaults to username");

    let r = app.post("/api/accounts", json!({"username": "bob", "password": "abcd"})).await;
    assert_eq!(r.status, StatusCode::CONFLICT);

    let r = app
        .post("/api/accounts", json!({"username": "carol", "password": "abcd", "displayName": "Carol C"}))
        .await;
    assert_eq!(r.body["displayName"], "Carol C");

    let names: Vec<_> = app.get("/api/accounts").await.body.as_array().unwrap().iter().map(|a| a["username"].clone()).collect();
    assert_eq!(names, [json!("admin"), json!("bob"), json!("carol")]);

    // New account can log in.
    let r = app.post_anon("/api/auth/login", json!({"username": "bob", "password": "abcd"})).await;
    assert_eq!(r.status, StatusCode::OK);
}

#[tokio::test]
async fn api_key_management() {
    let app = TestApp::new().await;
    let r = app.post("/api/accounts/api-keys", json!({"name": " "})).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);

    let created = app.post("/api/accounts/api-keys", json!({"name": "ci"})).await.body;
    assert!(created["key"].as_str().unwrap().starts_with("obs_"));

    let list = app.get("/api/accounts/api-keys").await.body;
    assert_eq!(list.as_array().unwrap().len(), 1);
    assert!(list[0].get("key").is_none(), "full key is only shown at creation");
    assert_eq!(list[0]["keyPrefix"], created["keyPrefix"]);
}

// --- alerts -----------------------------------------------------------------

#[tokio::test]
async fn alert_validation() {
    let app = TestApp::new().await;
    let r = app
        .post("/api/alerts", json!({"name": "a", "query": "| count", "operator": "~", "threshold": 1}))
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);

    let r = app
        .post("/api/alerts", json!({"name": "a", "query": "level = error", "operator": "gt", "threshold": 1}))
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    assert!(r.body["error"]["message"].as_str().unwrap().contains("aggregation"));

    let r = app
        .post("/api/alerts", json!({"name": "a", "query": "level = | count", "operator": "gt", "threshold": 1}))
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    assert_eq!(error_code(&r), "invalid_query");
}

#[tokio::test]
async fn alerts_crud_and_evaluation() {
    let app = TestApp::new().await;
    // Alerts look at the last hour, so these events need current timestamps.
    app.ingest(json!([
        {"level": "error", "message": "e1"},
        {"level": "error", "message": "e2"},
        {"level": "info", "message": "i1"}
    ]))
    .await;

    let fire = app
        .post("/api/alerts", json!({"name": "many errors", "query": "level = error | count", "operator": ">=", "threshold": 2}))
        .await;
    assert_eq!(fire.status, StatusCode::OK);
    assert_eq!(fire.body["status"], "ok");
    let fire_id = fire.body["id"].as_str().unwrap().to_string();

    let quiet = app
        .post("/api/alerts", json!({"name": "no fatals", "query": "level = fatal | count", "operator": "gt", "threshold": 0}))
        .await
        .body;
    let quiet_id = quiet["id"].as_str().unwrap().to_string();

    let by_service = app
        .post("/api/alerts", json!({"name": "grouped", "query": "| count by level", "operator": "eq", "threshold": 3}))
        .await
        .body;

    super::evaluate_alerts(&app.state).await.unwrap();

    let alerts = app.get("/api/alerts").await.body;
    let find = |id: &str| alerts.as_array().unwrap().iter().find(|a| a["id"] == id).unwrap().clone();
    let f = find(&fire_id);
    assert_eq!(f["status"], "firing");
    assert_eq!(f["value"], 2.0);
    assert!(f["message"].as_str().unwrap().contains("gte"));
    assert!(f["lastEvaluatedAt"].is_string());
    let q = find(&quiet_id);
    assert_eq!(q["status"], "ok");
    assert_eq!(q["value"], 0.0);
    assert_eq!(find(by_service["id"].as_str().unwrap())["status"], "firing", "groups are summed");

    // Disabled rules are skipped by evaluation.
    let r = app.post(&format!("/api/alerts/{quiet_id}/enabled"), json!({"enabled": false})).await;
    assert_eq!(r.status, StatusCode::OK);
    app.ingest(json!([{"level": "fatal", "message": "f1"}])).await;
    super::evaluate_alerts(&app.state).await.unwrap();
    let alerts = app.get("/api/alerts").await.body;
    let q = alerts.as_array().unwrap().iter().find(|a| a["id"] == quiet_id.as_str()).unwrap();
    assert_eq!(q["status"], "ok");
    assert_eq!(q["enabled"], false);

    assert_eq!(app.delete(&format!("/api/alerts/{fire_id}")).await.status, StatusCode::OK);
    assert_eq!(app.get("/api/alerts").await.body.as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn alert_with_unplannable_query_records_error_instead_of_failing() {
    let app = TestApp::new().await;
    // Parses fine but fails planning (attribute ranges are unsupported).
    let r = app
        .post("/api/alerts", json!({"name": "bad", "query": "customerId > 5 | count", "operator": "gt", "threshold": 0}))
        .await;
    assert_eq!(r.status, StatusCode::OK);
    super::evaluate_alerts(&app.state).await.unwrap();
    let a = &app.get("/api/alerts").await.body[0];
    assert_eq!(a["status"], "ok");
    assert!(a["message"].as_str().unwrap().starts_with("plan error"));
}
