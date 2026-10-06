//! API behaviour: validation, errors, pagination, time ranges, aggregates.

use serde_json::{Value, json};

use crate::common::*;

async fn error_of(r: reqwest::Response) -> (u16, Value) {
    let status = r.status().as_u16();
    let v: Value = r.json().await.unwrap();
    (status, v["error"].clone())
}

#[tokio::test(flavor = "multi_thread")]
async fn valid_requests_and_single_vs_batch() {
    let s = TestServer::start().await;
    assert_eq!(s.ingest(json!({"message": "one"})).await["accepted"], 1);
    assert_eq!(s.ingest(json!([{"message": "two"}, {"message": "three"}])).await["accepted"], 2);
    assert_eq!(s.ingest(json!({"events": [{"message": "four"}]})).await["accepted"], 1);
    let r = s.ingest_ndjson("{\"message\":\"five\"}\n{\"message\":\"six\"}\n".into()).await;
    assert_eq!(r.status(), 200);
    assert_eq!(s.messages("").await.len(), 6);
    s.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn invalid_payloads_return_structured_errors() {
    let s = TestServer::start_with(|c| c.ingest.max_request_size = server::config::ByteSize(4096)).await;

    let r = s
        .client
        .post(s.u("/api/v1/events"))
        .header("content-type", "application/json")
        .body("{not json")
        .send()
        .await
        .unwrap();
    let (status, e) = error_of(r).await;
    assert_eq!(status, 400);
    assert_eq!(e["code"], "invalid_payload");

    let r = s.post("/api/v1/events", json!([{"message": "ok"}, "nope"])).await;
    let (status, e) = error_of(r).await;
    assert_eq!(status, 400);
    assert_eq!(e["code"], "invalid_event");
    assert_eq!(e["index"], 1);
    // All-or-nothing: the valid event of the rejected batch was not stored.
    assert!(s.messages("").await.is_empty());

    let r = s.post("/api/v1/events", json!([{ "message": "x".repeat(5000) }])).await;
    let (status, e) = error_of(r).await;
    assert_eq!(status, 413);
    assert_eq!(e["code"], "payload_too_large");

    let r = s.client.post(s.u("/api/v1/events")).header("content-type", "text/csv").body("a,b").send().await.unwrap();
    let (status, e) = error_of(r).await;
    assert_eq!(status, 415);
    assert_eq!(e["code"], "unsupported_media_type");

    let r = s.otlp_json("/v1/traces", "[]".into()).await;
    assert_eq!(error_of(r).await.0, 400);

    // Malformed query-API JSON uses the same error shape.
    let r = s
        .client
        .post(s.u("/api/v1/query/logs"))
        .header("content-type", "application/json")
        .body("{")
        .send()
        .await
        .unwrap();
    let (status, e) = error_of(r).await;
    assert_eq!(status, 400);
    assert_eq!(e["code"], "invalid_json");

    let r = s.client.get(s.u("/api/v1/dashboards/not-a-number")).send().await.unwrap();
    assert_eq!(error_of(r).await.1["code"], "invalid_request");

    let r = s.client.get(s.u("/api/v1/does-not-exist")).send().await.unwrap();
    let (status, e) = error_of(r).await;
    assert_eq!((status, e["code"].as_str()), (404, Some("not_found")));
    s.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn invalid_queries_report_position() {
    let s = TestServer::start().await;
    let r = s.post("/api/v1/query/logs", json!({"query": "level = \"Error\" and"})).await;
    let (status, e) = error_of(r).await;
    assert_eq!(status, 400);
    assert_eq!(e["code"], "invalid_query");
    assert_eq!(e["message"], "Expected expression after 'and'");
    assert_eq!(e["position"], 19);

    let r = s.post("/api/v1/query/logs", json!({"query": "", "limit": 0})).await;
    assert_eq!(error_of(r).await.0, 400);
    let r = s.post("/api/v1/query/logs", json!({"query": "", "continuationToken": "garbage"})).await;
    assert_eq!(error_of(r).await.0, 400);
    let r = s.post("/api/v1/query/logs", json!({"query": "", "from": "yesterday-ish"})).await;
    assert_eq!(error_of(r).await.0, 400);
    s.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn pagination_time_ranges_and_empty_results() {
    let s = TestServer::start().await;
    let base = telemetry::Timestamp::parse_rfc3339("2026-01-01T00:00:00Z").unwrap();
    let events: Vec<Value> = (0..25)
        .map(|i| {
            json!({
                "message": format!("m{i:02}"),
                "timestamp": base.saturating_add_nanos(i * 60 * telemetry::NANOS_PER_SEC).to_rfc3339(),
            })
        })
        .collect();
    s.ingest(Value::Array(events)).await;
    s.rotate_all();

    // Backward pagination over everything.
    let mut seen = Vec::new();
    let mut token: Option<String> = None;
    loop {
        let v = s.post_ok("/api/v1/query/logs", json!({"query": "", "limit": 10, "continuationToken": token})).await;
        seen.extend(v["events"].as_array().unwrap().iter().map(|e| e["message"].as_str().unwrap().to_string()));
        token = v["continuationToken"].as_str().map(String::from);
        if token.is_none() {
            break;
        }
    }
    let expected: Vec<String> = (0..25).rev().map(|i| format!("m{i:02}")).collect();
    assert_eq!(seen, expected);

    // Forward direction and a [from, to) window.
    let v = s
        .post_ok(
            "/api/v1/query/logs",
            json!({
                "query": "",
                "direction": "forward",
                "from": "2026-01-01T00:05:00Z",
                "to": "2026-01-01T00:08:00Z",
            }),
        )
        .await;
    let msgs: Vec<&str> = v["events"].as_array().unwrap().iter().map(|e| e["message"].as_str().unwrap()).collect();
    assert_eq!(msgs, ["m05", "m06", "m07"]);

    // A window with no data is skipped without reading blocks.
    let v = s
        .post_ok(
            "/api/v1/query/logs",
            json!({"query": "", "from": "2025-01-01T00:00:00Z", "to": "2025-01-02T00:00:00Z"}),
        )
        .await;
    assert!(v["events"].as_array().unwrap().is_empty());
    assert_eq!(v["continuationToken"], Value::Null);
    assert_eq!(v["diagnostics"]["segmentsSkippedByTime"], 1);

    let v = s.post_ok("/api/v1/query/logs", json!({"query": "message = \"nothing\""})).await;
    assert!(v["events"].as_array().unwrap().is_empty());

    // Histogram and count over the same window.
    let v = s
        .post_ok(
            "/api/v1/query/logs/histogram",
            json!({"query": "", "from": "2026-01-01T00:00:00Z", "to": "2026-01-01T00:30:00Z", "buckets": 30}),
        )
        .await;
    assert_eq!(v["total"], 25);
    assert_eq!(v["stepMs"], 60000);
    let v = s.post_ok("/api/v1/query/logs/count", json!({"query": "", "from": "2026-01-01T00:10:00Z"})).await;
    assert_eq!(v["count"], 15);
    s.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn facets_feed_the_filter_panel() {
    let s = TestServer::start().await;
    s.ingest_ndjson(fixture("logs.ndjson")).await.error_for_status().unwrap();
    let v = s.post_ok("/api/v1/query/logs/facets", json!({"query": ""})).await;
    assert_eq!(v["sampled"], 8);
    let fields = v["fields"].as_array().unwrap();
    assert_eq!(fields[0]["field"], "level");
    let service = fields.iter().find(|f| f["field"] == "service").unwrap();
    let counts: Vec<(String, u64)> = service["values"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| (v["value"].as_str().unwrap().to_string(), v["count"].as_u64().unwrap()))
        .collect();
    // Ties are ordered by value.
    assert_eq!(counts, [("orders".into(), 3), ("payments".into(), 3), ("auth".into(), 2)]);
    let customer = fields.iter().find(|f| f["field"] == "customerId").unwrap();
    assert_eq!(customer["values"][0]["value"], 481);
    s.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn system_endpoints() {
    let s = TestServer::start().await;
    assert_eq!(s.get_ok("/health").await["status"], "ok");
    assert_eq!(s.get_ok("/ready").await["status"], "ready");
    s.ingest_ndjson(fixture("logs.ndjson")).await.error_for_status().unwrap();
    let info = s.get_ok("/api/v1/system/info").await;
    assert_eq!(info["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(info["eventCounts"]["logs"], 8);
    assert!(info["memory"]["budgets"]["total"].as_u64().unwrap() > 0);
    assert!(info["queue"]["logs"]["capacity"].as_u64().unwrap() > 0);
    let cfg = s.get_ok("/api/v1/system/config").await;
    assert!(cfg["config"]["auth"].get("admin_password").is_none(), "secrets are never returned");

    s.query("customerId = 481").await;
    let qs = s.get_ok("/api/v1/system/query-stats").await;
    let f = qs["fields"].as_array().unwrap().iter().find(|f| f["field"] == "customerId").unwrap();
    assert!(f["queries"].as_u64().unwrap() >= 1);
    s.stop().await;
}
