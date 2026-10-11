//! Live stream (SSE): new matching events arrive; filters apply.

use serde_json::{Value, json};

use crate::common::*;

#[tokio::test(flavor = "multi_thread")]
async fn live_stream_delivers_matching_events() {
    let s = TestServer::start().await;
    let mut all = Sse::open(&s, "").await;
    let mut errors = Sse::open(&s, "level = Error").await;

    s.ingest(json!([{"message": "info event", "level": "Information"}, {"message": "error event", "level": "Error"}]))
        .await;

    let (k, d) = all.next().await;
    assert_eq!(k, "event");
    assert_eq!(serde_json::from_str::<Value>(&d).unwrap()["message"], "info event");
    let (_, d) = all.next().await;
    assert_eq!(serde_json::from_str::<Value>(&d).unwrap()["message"], "error event");

    let (k, d) = errors.next().await;
    assert_eq!(k, "event");
    let v: Value = serde_json::from_str(&d).unwrap();
    assert_eq!(v["message"], "error event");
    assert_eq!(v["level"], "Error");

    let info = s.get_ok("/api/v1/system/info").await;
    assert_eq!(info["liveSubscribers"], 2);
    drop(all);
    drop(errors);
    s.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn live_stream_rejects_bad_queries_and_ends_on_shutdown() {
    let s = TestServer::start().await;
    let r = s.client.get(s.u("/api/v1/live/logs")).query(&[("query", "level =")]).send().await.unwrap();
    assert_eq!(r.status(), 400);
    // An open stream must not block graceful shutdown.
    let _open = Sse::open(&s, "").await;
    s.stop().await;
}
