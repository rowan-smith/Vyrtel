//! OTLP logs/traces/metrics, trace ↔ log correlation, gzip.

use std::io::Write;

use prost::Message;
use serde_json::{Value, json};

use crate::common::*;

const TRACE: &str = "4bf92f3577b34da6a3ce929d0e0e4736";

#[tokio::test(flavor = "multi_thread")]
async fn otlp_logs_are_queryable_natively() {
    let s = TestServer::start().await;
    let r = s.otlp_json("/v1/logs", fixture("otlp-logs.json")).await;
    assert_eq!(r.status(), 200);
    assert_eq!(r.headers()["content-type"], "application/json");
    assert_eq!(r.text().await.unwrap(), "{}");

    let v = s.query(r#"service = "inventory" and level = Warning and remaining = 3"#).await;
    let e = &v["events"][0];
    assert_eq!(e["message"], "Stock low for ABC-1");
    assert_eq!(e["resource"]["host.name"], "web-1");
    assert_eq!(e["properties"]["otel.scope.name"], "Inventory.Worker");

    let v = s.query(r#"exception.type = "DbException""#).await;
    let e = &v["events"][0];
    assert_eq!(e["level"], "Error");
    assert_eq!(e["traceId"], TRACE);
    assert!(e["exception"]["stackTrace"].as_str().unwrap().contains("Inventory.Reserve"));
    s.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn otlp_protobuf_and_gzip() {
    use ingest::otlp::proto::*;
    let s = TestServer::start().await;
    let req = ExportLogsServiceRequest {
        resource_logs: vec![ResourceLogs {
            resource: Some(Resource {
                attributes: vec![KeyValue {
                    key: "service.name".into(),
                    value: Some(AnyValue { value: Some(any_value::Value::StringValue("pb-service".into())) }),
                }],
            }),
            scope_logs: vec![ScopeLogs {
                scope: None,
                log_records: vec![LogRecord {
                    severity_number: 9,
                    body: Some(AnyValue { value: Some(any_value::Value::StringValue("from protobuf".into())) }),
                    ..Default::default()
                }],
            }],
        }],
    };
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    gz.write_all(&req.encode_to_vec()).unwrap();
    let body = gz.finish().unwrap();
    let r = s
        .client
        .post(s.u("/v1/logs"))
        .header("content-type", "application/x-protobuf")
        .header("content-encoding", "gzip")
        .body(body)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    assert_eq!(r.headers()["content-type"], "application/x-protobuf");
    assert_eq!(s.messages(r#"service = "pb-service""#).await, ["from protobuf"]);
    s.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn spans_traces_and_correlated_logs() {
    let s = TestServer::start().await;
    assert_eq!(s.otlp_json("/v1/traces", fixture("otlp-traces.json")).await.status(), 200);
    s.ingest_ndjson(fixture("logs.ndjson")).await.error_for_status().unwrap();

    // Trace search: newest traces with an error span.
    let v = s.post_ok("/api/v1/query/traces", json!({ "query": "status = Error" })).await;
    let traces = v["traces"].as_array().unwrap();
    assert_eq!(traces.len(), 1);
    let t = &traces[0];
    assert_eq!(t["traceId"], TRACE);
    assert_eq!(t["rootName"], "POST /checkout");
    assert_eq!(t["rootService"], "checkout");
    assert_eq!(t["spanCount"], 3);
    assert_eq!(t["errorCount"], 2);
    assert_eq!(t["services"], json!(["checkout", "payments"]));
    let dur = t["durationMs"].as_f64().unwrap();
    assert!((dur - 800.0).abs() < 1.0, "{dur}");

    // Search by duration, span attributes and service.
    let v = s.post_ok("/api/v1/query/traces", json!({ "query": "durationMs > 600 and service = payments" })).await;
    assert_eq!(v["traces"].as_array().unwrap().len(), 1);
    let v = s.post_ok("/api/v1/query/traces", json!({ "query": "durationMs > 5000" })).await;
    assert!(v["traces"].as_array().unwrap().is_empty());

    // Trace by id, with the span hierarchy.
    let v = s.get_ok(&format!("/api/v1/traces/{TRACE}")).await;
    let spans = v["spans"].as_array().unwrap();
    assert_eq!(spans.len(), 3);
    assert_eq!(spans[0]["name"], "POST /checkout");
    assert!(spans[0].get("parentSpanId").is_none());
    assert_eq!(spans[1]["parentSpanId"], "a1a1a1a1a1a1a1a1");
    assert_eq!(spans[1]["events"][0]["name"], "exception");
    // Uppercase ids work too.
    s.get_ok(&format!("/api/v1/traces/{}", TRACE.to_uppercase())).await;
    let r = s.client.get(s.u("/api/v1/traces/ffffffffffffffffffffffffffffffff")).send().await.unwrap();
    assert_eq!(r.status(), 404);

    // Correlated logs.
    let logs = s.messages(&format!(r#"traceId = "{TRACE}""#)).await;
    assert_eq!(logs, ["Payment 9001 captured", "Payment provider timed out"]);

    // Spans as raw events.
    let v = s.post_ok("/api/v1/query/traces/events", json!({ "query": r#"db.system = "postgresql""# })).await;
    assert_eq!(v["events"][0]["name"], "SELECT payments");
    s.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn metrics_ingest_and_query() {
    let s = TestServer::start().await;
    assert_eq!(s.otlp_json("/v1/metrics", fixture("otlp-metrics.json")).await.status(), 200);
    let v = s.get_ok("/api/v1/metrics").await;
    let names: Vec<&str> = v["metrics"].as_array().unwrap().iter().map(|m| m["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["http.server.duration", "http.server.requests", "process.memory.usage"]);

    let q = |agg: &str, group: Value| json!({ "name": "http.server.requests", "from": "-10m", "agg": agg, "stepMs": 600000, "groupBy": group });
    let total = |v: &Value| -> f64 {
        v["series"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|s| s["points"].as_array().unwrap().iter())
            .filter_map(|p| p["value"].as_f64())
            .sum()
    };
    let v = s.post_ok("/api/v1/query/metrics", q("sum", Value::Null)).await;
    assert_eq!(total(&v), 34.0);
    assert_eq!(v["kind"], "sum");
    assert_eq!(v["unit"], "1");
    let v = s.post_ok("/api/v1/query/metrics", q("count", Value::Null)).await;
    assert_eq!(total(&v), 3.0);
    let v = s.post_ok("/api/v1/query/metrics", q("max", json!("route"))).await;
    let series = v["series"].as_array().unwrap();
    assert_eq!(series.len(), 2);
    let v = s
        .post_ok(
            "/api/v1/query/metrics",
            json!({ "name": "http.server.requests", "query": r#"route = "/health""#, "from": "-10m", "agg": "sum", "stepMs": 600000 }),
        )
        .await;
    assert_eq!(total(&v), 4.0);
    let v = s
        .post_ok(
            "/api/v1/query/metrics",
            json!({ "name": "http.server.duration", "from": "-10m", "agg": "avg", "stepMs": 600000 }),
        )
        .await;
    assert_eq!(total(&v), 205.0, "histogram avg = sum / count");
    s.stop().await;
}
