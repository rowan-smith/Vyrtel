//! Live stream (SSE): new matching events arrive; filters apply.

use std::time::Duration;

use serde_json::{Value, json};

use crate::common::*;

/// Minimal SSE reader over reqwest's chunked body.
struct Sse {
    resp: reqwest::Response,
    buf: String,
}

impl Sse {
    async fn open(s: &TestServer, query: &str) -> Sse {
        let resp = s.client.get(s.u("/api/v1/live/logs")).query(&[("query", query)]).send().await.unwrap();
        assert_eq!(resp.status(), 200);
        assert!(resp.headers()["content-type"].to_str().unwrap().starts_with("text/event-stream"));
        let mut sse = Sse { resp, buf: String::new() };
        let (kind, _) = sse.next().await;
        assert_eq!(kind, "ready");
        sse
    }

    /// Next (event, data), skipping keep-alive comments.
    async fn next(&mut self) -> (String, String) {
        loop {
            if let Some(end) = self.buf.find("\n\n") {
                let frame: String = self.buf.drain(..end + 2).collect();
                let mut kind = String::from("message");
                let mut data = String::new();
                for line in frame.lines() {
                    if let Some(k) = line.strip_prefix("event:") {
                        kind = k.trim().to_string();
                    } else if let Some(d) = line.strip_prefix("data:") {
                        data.push_str(d.trim_start());
                    }
                }
                if data.is_empty() && kind == "message" {
                    continue;
                }
                return (kind, data);
            }
            let chunk = tokio::time::timeout(Duration::from_secs(10), self.resp.chunk())
                .await
                .expect("SSE data within timeout")
                .unwrap()
                .expect("stream still open");
            self.buf.push_str(&String::from_utf8_lossy(&chunk));
        }
    }
}

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
