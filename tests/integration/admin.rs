//! Dashboards, saved queries, API keys and alerts with webhook delivery.

use std::time::Duration;

use axum::routing::post;
use axum::{Json, Router};
use serde_json::{Value, json};
use tokio::sync::mpsc;

use crate::common::*;

#[tokio::test(flavor = "multi_thread")]
async fn dashboard_lifecycle() {
    let s = TestServer::start().await;
    let d = s.post_ok("/api/v1/dashboards", json!({"name": "Payments"})).await;
    let id = d["id"].as_i64().unwrap();

    let p1 = s
        .post_ok(
            &format!("/api/v1/dashboards/{id}/panels"),
            json!({"title": "Errors", "kind": "log_count", "config": {"query": "level = Error"}}),
        )
        .await;
    s.post_ok(
        &format!("/api/v1/dashboards/{id}/panels"),
        json!({"title": "Requests", "kind": "metric_line", "config": {"metric": "http.server.requests", "agg": "sum"}}),
    )
    .await;
    // Invalid panel configs are rejected.
    let r = s
        .post(&format!("/api/v1/dashboards/{id}/panels"), json!({"title": "x", "kind": "metric_line", "config": {}}))
        .await;
    assert_eq!(r.status(), 400);
    let r = s
        .post(
            &format!("/api/v1/dashboards/{id}/panels"),
            json!({"title": "x", "kind": "log_count", "config": {"query": "level ="}}),
        )
        .await;
    assert_eq!(r.status(), 400);
    let r = s
        .post(&format!("/api/v1/dashboards/{id}/panels"), json!({"title": "x", "kind": "pie_chart", "config": {}}))
        .await;
    assert_eq!(r.status(), 422, "unknown panel kinds are rejected by the schema");

    let r = s
        .client
        .put(s.u(&format!("/api/v1/dashboards/{id}")))
        .json(&json!({"name": "Payments (prod)"}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);

    let pid = p1["id"].as_i64().unwrap();
    let r = s
        .client
        .put(s.u(&format!("/api/v1/dashboards/{id}/panels/{pid}")))
        .json(&json!({"title": "Errors (5m)", "kind": "single_stat", "config": {"query": "level = Error", "window": "5m"}}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);

    let d = s.get_ok(&format!("/api/v1/dashboards/{id}")).await;
    assert_eq!(d["name"], "Payments (prod)");
    assert_eq!(d["panels"].as_array().unwrap().len(), 2);
    assert_eq!(d["panels"][0]["kind"], "single_stat");

    let r = s.client.delete(s.u(&format!("/api/v1/dashboards/{id}/panels/{pid}"))).send().await.unwrap();
    assert_eq!(r.status(), 204);
    let list = s.get_ok("/api/v1/dashboards").await;
    assert_eq!(list["dashboards"][0]["panels"].as_array().unwrap().len(), 1);
    let r = s.client.delete(s.u(&format!("/api/v1/dashboards/{id}"))).send().await.unwrap();
    assert_eq!(r.status(), 204);
    let r = s.client.get(s.u(&format!("/api/v1/dashboards/{id}"))).send().await.unwrap();
    assert_eq!(r.status(), 404);

    // Dashboards persist in the metadata store across restarts.
    s.post_ok("/api/v1/dashboards", json!({"name": "Kept"})).await;
    let s = s.restart().await;
    assert_eq!(s.get_ok("/api/v1/dashboards").await["dashboards"][0]["name"], "Kept");
    s.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn saved_queries() {
    let s = TestServer::start().await;
    let q = s.post_ok("/api/v1/saved-queries", json!({"name": "Errors", "query": "level = Error"})).await;
    assert_eq!(q["signal"], "logs");
    assert_eq!(s.post("/api/v1/saved-queries", json!({"name": "Bad", "query": "level ="})).await.status(), 400);
    assert_eq!(s.get_ok("/api/v1/saved-queries").await["savedQueries"].as_array().unwrap().len(), 1);
    let r = s.client.delete(s.u(&format!("/api/v1/saved-queries/{}", q["id"]))).send().await.unwrap();
    assert_eq!(r.status(), 204);
    s.stop().await;
}

async fn webhook_receiver() -> (String, mpsc::UnboundedReceiver<Value>) {
    let (tx, rx) = mpsc::unbounded_channel();
    let app = Router::new().route(
        "/hook",
        post(move |Json(v): Json<Value>| {
            let tx = tx.clone();
            async move {
                tx.send(v).unwrap();
                "ok"
            }
        }),
    );
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/hook", l.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    (url, rx)
}

#[tokio::test(flavor = "multi_thread")]
async fn threshold_alert_fires_and_resolves_with_webhook() {
    let s = TestServer::start_with(|c| c.alerts.enabled = false).await;
    let (url, mut hooks) = webhook_receiver().await;
    let a = s
        .post_ok(
            "/api/v1/alerts",
            json!({
                "name": "Payment error spike",
                "query": "level = \"Error\" and service = \"payments\"",
                "op": ">",
                "threshold": 2,
                "windowSecs": 300,
                "intervalSecs": 60,
                "webhookUrl": url,
            }),
        )
        .await;
    let id = a["id"].as_i64().unwrap();
    assert_eq!(a["state"]["status"], "OK");

    let eval = || async { s.post_ok(&format!("/api/v1/alerts/{id}/evaluate"), json!({})).await };
    let v = eval().await;
    assert_eq!(v["state"]["status"], "OK");
    assert_eq!(v["state"]["lastValue"], 0.0);

    for _ in 0..3 {
        s.ingest(json!({"level": "Error", "service": "payments", "message": "boom"})).await;
    }
    let v = eval().await;
    assert_eq!(v["state"]["status"], "FIRING");
    assert_eq!(v["state"]["lastValue"], 3.0);
    let hook = tokio::time::timeout(Duration::from_secs(10), hooks.recv()).await.unwrap().unwrap();
    assert_eq!(hook["status"], "FIRING");
    assert_eq!(hook["previousStatus"], "OK");
    assert_eq!(hook["value"], 3.0);
    assert_eq!(hook["alert"]["name"], "Payment error spike");

    // Raise the threshold: the alert resolves and notifies again.
    let r = s
        .client
        .put(s.u(&format!("/api/v1/alerts/{id}")))
        .json(&json!({
            "name": "Payment error spike",
            "query": "level = \"Error\" and service = \"payments\"",
            "op": ">", "threshold": 100, "windowSecs": 300, "intervalSecs": 60, "webhookUrl": url,
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    assert_eq!(eval().await["state"]["status"], "OK");
    let hook = tokio::time::timeout(Duration::from_secs(10), hooks.recv()).await.unwrap().unwrap();
    assert_eq!((hook["status"].as_str(), hook["previousStatus"].as_str()), (Some("OK"), Some("FIRING")));

    let detail = s.get_ok(&format!("/api/v1/alerts/{id}")).await;
    let history = detail["history"].as_array().unwrap();
    assert_eq!(history.len(), 2);
    assert_eq!(history[0]["to"], "OK");
    // Delivery is recorded asynchronously.
    for _ in 0..50 {
        if s.get_ok(&format!("/api/v1/alerts/{id}")).await["state"]["lastNotificationAt"].is_number() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let st = s.get_ok(&format!("/api/v1/alerts/{id}")).await;
    assert!(st["state"]["lastNotificationAt"].is_number());
    assert!(st["state"]["lastNotificationError"].is_null());

    // Alert state persists.
    let s = s.restart().await;
    assert_eq!(s.get_ok(&format!("/api/v1/alerts/{id}")).await["state"]["status"], "OK");
    s.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn alert_validation_and_error_state() {
    let s = TestServer::start_with(|c| c.alerts.enabled = false).await;
    let bad = |field: &str, value: Value| {
        let mut a = json!({"name": "a", "query": "level = Error", "op": ">", "threshold": 1, "windowSecs": 60, "intervalSecs": 60});
        a[field] = value;
        a
    };
    assert_eq!(s.post("/api/v1/alerts", bad("query", json!("level ="))).await.status(), 400);
    assert_eq!(s.post("/api/v1/alerts", bad("op", json!("~"))).await.status(), 400);
    assert_eq!(s.post("/api/v1/alerts", bad("intervalSecs", json!(1))).await.status(), 400);
    assert_eq!(s.post("/api/v1/alerts", bad("webhookUrl", json!("file:///etc/passwd"))).await.status(), 400);
    let list = s.get_ok("/api/v1/alerts").await;
    assert!(list["alerts"].as_array().unwrap().is_empty());
    s.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn background_evaluator_runs_due_alerts() {
    let s = TestServer::start().await;
    s.ingest(json!({"level": "Error", "message": "x"})).await;
    let a = s
        .post_ok(
            "/api/v1/alerts",
            json!({"name": "any error", "query": "level = Error", "op": ">=", "threshold": 1, "windowSecs": 600, "intervalSecs": 10}),
        )
        .await;
    let id = a["id"].as_i64().unwrap();
    let mut status = String::new();
    for _ in 0..100 {
        status = s.get_ok(&format!("/api/v1/alerts/{id}")).await["state"]["status"].as_str().unwrap().to_string();
        if status == "FIRING" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    assert_eq!(status, "FIRING");
    s.stop().await;
}
