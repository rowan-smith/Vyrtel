//! REL-03: oversized and malformed ingest bodies are rejected the same way
//! on every ingest path, and a rejected request stores nothing.
//!
//! Every server here runs with `ingest.max_request_size = 64 KB`.

use std::time::Duration;

use prost::Message;
use serde_json::{Value, json};

use crate::common::*;

const LIMIT: usize = 64 << 10;

async fn server() -> TestServer {
    TestServer::start_with(|c| c.ingest.max_request_size = server::config::ByteSize(LIMIT as u64)).await
}

async fn post(
    s: &TestServer,
    path: &str,
    content_type: &str,
    encoding: Option<&str>,
    body: Vec<u8>,
) -> reqwest::Response {
    let mut req = s.client.post(s.u(path)).header("content-type", content_type);
    if let Some(e) = encoding {
        req = req.header("content-encoding", e);
    }
    req.body(body).send().await.unwrap()
}

async fn ingest_ndjson_ok(s: &TestServer) -> u16 {
    s.ingest_ndjson(
        "{\"message\":\"after\"}
"
        .into(),
    )
    .await
    .status()
    .as_u16()
}

async fn count(s: &TestServer, signal: &str) -> u64 {
    s.post_ok(&format!("/api/v1/query/{signal}/count"), json!({ "query": "" })).await["count"].as_u64().unwrap()
}

/// `make(pad)` with `pad` chosen so the result is exactly `size` bytes.
fn sized(size: usize, make: impl Fn(usize) -> Vec<u8>) -> Vec<u8> {
    let mut pad = size - make(0).len();
    for _ in 0..8 {
        let body = make(pad);
        if body.len() == size {
            return body;
        }
        pad = (pad + size).checked_sub(body.len()).expect("fixture can be padded");
    }
    panic!("could not build a {size}-byte fixture");
}

/// One event per body, carrying `id` as its message and `pad` filler bytes.
struct Format {
    name: &'static str,
    path: &'static str,
    content_type: &'static str,
    make: fn(&str, usize) -> Vec<u8>,
}

const FORMATS: [Format; 4] = [
    Format {
        name: "native JSON",
        path: "/api/v1/events",
        content_type: "application/json",
        make: |id, pad| serde_json::to_vec(&json!([{ "message": id, "pad": "x".repeat(pad) }])).unwrap(),
    },
    Format {
        name: "native NDJSON",
        path: "/api/v1/events",
        content_type: "application/x-ndjson",
        make: |id, pad| format!("{{\"message\":\"{id}\",\"pad\":\"{}\"}}\n", "x".repeat(pad)).into_bytes(),
    },
    Format {
        name: "OTLP JSON",
        path: "/v1/logs",
        content_type: "application/json",
        make: |id, pad| {
            serde_json::to_vec(&json!({ "resourceLogs": [{ "scopeLogs": [{ "logRecords": [{
                "body": { "stringValue": id },
                "attributes": [{ "key": "pad", "value": { "stringValue": "x".repeat(pad) } }],
            }]}]}]}))
            .unwrap()
        },
    },
    Format {
        name: "OTLP protobuf",
        path: "/v1/logs",
        content_type: "application/x-protobuf",
        make: |id, pad| {
            use ingest::otlp::proto::*;
            let string = |s: String| Some(AnyValue { value: Some(any_value::Value::StringValue(s)) });
            ExportLogsServiceRequest {
                resource_logs: vec![ResourceLogs {
                    resource: None,
                    scope_logs: vec![ScopeLogs {
                        scope: None,
                        log_records: vec![LogRecord {
                            body: string(id.into()),
                            attributes: vec![KeyValue { key: "pad".into(), value: string("x".repeat(pad)) }],
                            ..Default::default()
                        }],
                    }],
                }],
            }
            .encode_to_vec()
        },
    },
];

async fn assert_too_large(r: reqwest::Response, what: &str) {
    // The rest of the body may go unread, so the connection is not reused.
    assert_eq!(r.headers()["connection"], "close", "{what}");
    let (status, e) = error_of(r).await;
    assert_eq!(status, 413, "{what}");
    assert_eq!(e["code"], "payload_too_large", "{what}");
    assert_eq!(e["limitBytes"], LIMIT, "{what}");
    assert!(e["message"].as_str().unwrap().contains("after gzip decoding"), "{what}: {e}");
}

async fn error_of(r: reqwest::Response) -> (u16, Value) {
    let status = r.status().as_u16();
    let body: Value = r.json().await.unwrap();
    (status, body["error"].clone())
}

// AC2 / TC-02: limit−1 and limit decoded bytes are accepted, limit+1 is
// rejected, for native JSON, NDJSON, OTLP JSON and OTLP protobuf, sent
// plain and gzip-compressed. Only the accepted bodies are stored.
#[tokio::test(flavor = "multi_thread")]
async fn decoded_size_limit_boundaries_on_every_ingest_path() {
    let s = server().await;
    let mut stored = Vec::new();
    for f in &FORMATS {
        for gz in [false, true] {
            for size in [LIMIT - 1, LIMIT, LIMIT + 1] {
                let id = format!("{} {} {size}", f.name, if gz { "gzip" } else { "plain" });
                let body = sized(size, |pad| (f.make)(&id, pad));
                let (body, encoding) = if gz { (gzip(&body), Some("gzip")) } else { (body, None) };
                // A gzip body is far below the limit on the wire either way.
                assert!(!gz || body.len() < LIMIT / 10);
                let r = post(&s, f.path, f.content_type, encoding, body).await;
                if size <= LIMIT {
                    assert_eq!(r.status(), 200, "{id}: {}", r.text().await.unwrap());
                    stored.push(id);
                } else {
                    assert_too_large(r, &id).await;
                }
            }
        }
    }
    stored.sort();
    assert_eq!(s.messages("").await, stored);

    // The traces and metrics endpoints share the same reader.
    for path in ["/v1/traces", "/v1/metrics"] {
        for ct in ["application/json", "application/x-protobuf"] {
            let over = vec![b' '; LIMIT + 1];
            assert_too_large(post(&s, path, ct, None, over.clone()).await, path).await;
            assert_too_large(post(&s, path, ct, Some("gzip"), gzip(&over)).await, path).await;
        }
    }
    assert_eq!(count(&s, "traces").await, 0);
    assert_eq!(count(&s, "metrics").await, 0);
    s.stop().await;
}

// AC1: a gzip bomb is rejected with 413 as soon as its decoded size passes
// the limit — here while the client is still uploading it, so the server
// cannot have read (or decoded) the rest.
#[tokio::test(flavor = "multi_thread")]
async fn gzip_bomb_is_rejected_before_the_upload_finishes() {
    let s = server().await;
    let bomb = gzip(&vec![b' '; 64 << 20]);
    let sent = 8 << 10; // decodes to megabytes, far over the limit
    let mut r = RawRequest::open(
        &s,
        "/api/v1/events",
        &[("Content-Type", "application/x-ndjson"), ("Content-Encoding", "gzip")],
        bomb.len(),
    )
    .await;
    r.send(&bomb[..sent]).await;
    assert_eq!(r.status_line(Duration::from_secs(10)).await, Some(413));
    assert_eq!(s.state().received_bytes.load(std::sync::atomic::Ordering::Relaxed), 0);
    wait_until("slot freed", || ingest_in_flight(&s) == 0).await;
    s.stop().await;
}

// An uncompressed body that announces more than the limit is rejected from
// its headers, before any of it is read.
#[tokio::test(flavor = "multi_thread")]
async fn oversized_content_length_is_rejected_before_reading() {
    let s = server().await;
    let mut r = RawRequest::open(&s, "/v1/logs", &[("Content-Type", "application/x-protobuf")], 100 << 20).await;
    assert_eq!(r.status_line(Duration::from_secs(10)).await, Some(413));
    wait_until("slot freed", || ingest_in_flight(&s) == 0).await;
    // A client that sends the whole oversized body reads the 413 too, and
    // its next request (on a new connection) succeeds.
    let big = vec![b' '; 8 << 20];
    assert_too_large(post(&s, "/api/v1/events", "application/x-ndjson", None, big).await, "8 MB upload").await;
    assert_eq!(ingest_ndjson_ok(&s).await, 200);
    s.stop().await;
}

// The compressed bytes count against the limit too: empty gzip members
// decode to nothing, yet a stream of them is not read forever.
#[tokio::test(flavor = "multi_thread")]
async fn compressed_bytes_on_the_wire_are_limited_too() {
    let s = server().await;
    let empty = gzip(b"");
    let body = empty.repeat(LIMIT / empty.len() + 1);
    assert!(body.len() > LIMIT);
    assert_too_large(post(&s, "/api/v1/events", "application/x-ndjson", Some("gzip"), body).await, "empty members")
        .await;
    s.stop().await;
}

// AC4: malformed compressed bodies get a 400 that says what is wrong with
// the encoding, unsupported encodings a structured 415, and valid but less
// common gzip forms are accepted. Nothing malformed is stored.
#[tokio::test(flavor = "multi_thread")]
async fn malformed_and_unsupported_encodings_get_actionable_errors() {
    let s = server().await;
    let ndjson = |msgs: &[&str]| msgs.iter().map(|m| format!("{{\"message\":\"{m}\"}}\n")).collect::<String>();
    let gz = gzip(ndjson(&["rejected"]).as_bytes());
    let corrupt_crc = {
        let mut g = gz.clone();
        let n = g.len();
        g[n - 8] ^= 0xff;
        g
    };
    let malformed: [(&str, Vec<u8>, &str); 5] = [
        ("truncated", gz[..gz.len() - 10].to_vec(), "unexpected end of file"),
        ("bad CRC", corrupt_crc, "CRC"),
        ("trailing bytes after the last member", [gz.clone(), b"trailing".to_vec()].concat(), "gzip header"),
        ("not gzip at all", ndjson(&["rejected"]).into_bytes(), "gzip header"),
        ("empty body", Vec::new(), "unexpected end of file"),
    ];
    for (path, ct) in [("/api/v1/events", "application/x-ndjson"), ("/v1/logs", "application/x-protobuf")] {
        for (name, body, cause) in &malformed {
            let (status, e) = error_of(post(&s, path, ct, Some("gzip"), body.clone()).await).await;
            assert_eq!(status, 400, "{path} {name}");
            assert_eq!(e["code"], "invalid_encoding", "{path} {name}");
            let message = e["message"].as_str().unwrap();
            assert!(message.contains("gzip body could not be decoded") && message.contains(cause), "{name}: {message}");
        }
        for encoding in ["br", "deflate", "zstd", "gzip, br"] {
            let (status, e) = error_of(post(&s, path, ct, Some(encoding), gz.clone()).await).await;
            assert_eq!(status, 415, "{path} {encoding}");
            assert_eq!(e["code"], "unsupported_media_type");
            assert!(e["message"].as_str().unwrap().contains("Content-Encoding: gzip"), "{e}");
        }
    }
    assert!(s.messages("").await.is_empty());

    // Concatenated members (`cat a.gz b.gz`) are one gzip stream: every
    // member is stored, not just the first. `x-gzip` and any case work.
    let members = [gzip(ndjson(&["member 1"]).as_bytes()), gzip(ndjson(&["member 2", "member 3"]).as_bytes())].concat();
    let r = post(&s, "/api/v1/events", "application/x-ndjson", Some("gzip"), members).await;
    assert_eq!(r.json::<Value>().await.unwrap(), json!({ "accepted": 3 }));
    for (i, encoding) in ["x-gzip", "GZip", "identity"].into_iter().enumerate() {
        let body = ndjson(&[&format!("variant {i}")]).into_bytes();
        let body = if encoding == "identity" { body } else { gzip(&body) };
        let r = post(&s, "/api/v1/events", "application/x-ndjson", Some(encoding), body).await;
        assert_eq!(r.status(), 200, "{encoding}");
    }
    assert_eq!(s.messages("").await, ["member 1", "member 2", "member 3", "variant 0", "variant 1", "variant 2"]);
    s.stop().await;
}

// AC3 / TC-03: a rejected native batch leaves nothing behind, however far
// into the body the problem is: a malformed final record, a gzip stream cut
// short after complete records, or a body that passes the limit after
// valid records.
#[tokio::test(flavor = "multi_thread")]
async fn rejected_native_batches_store_nothing() {
    let s = server().await;
    let valid = ndjson_payload(20, 10);
    let bad_last = format!("{valid}{{\"message\":\"cut");
    let gz = gzip(valid.as_bytes());
    let oversized = ndjson_payload(1000, 100);
    assert!(oversized.len() > LIMIT);

    // (name, Content-Encoding, body, status, code)
    type Case<'a> = (&'a str, Option<&'a str>, Vec<u8>, u16, &'a str);
    let cases: [Case; 6] = [
        ("malformed final record", None, bad_last.clone().into_bytes(), 400, "invalid_payload"),
        ("malformed final record, gzip", Some("gzip"), gzip(bad_last.as_bytes()), 400, "invalid_payload"),
        ("final record not an object", None, format!("{valid}5\n").into_bytes(), 400, "invalid_event"),
        ("gzip truncated after complete records", Some("gzip"), gz[..gz.len() - 8].to_vec(), 400, "invalid_encoding"),
        ("over the limit", None, oversized.clone().into_bytes(), 413, "payload_too_large"),
        ("over the limit, gzip", Some("gzip"), gzip(oversized.as_bytes()), 413, "payload_too_large"),
    ];
    for (name, encoding, body, status, code) in cases {
        let (got, e) = error_of(post(&s, "/api/v1/events", "application/x-ndjson", encoding, body).await).await;
        assert_eq!((got, e["code"].as_str().unwrap()), (status, code), "{name}: {e}");
        assert!(s.messages("").await.is_empty(), "{name} stored events");
    }
    // Only the three bodies that were read whole reached the parser.
    assert_eq!(s.state().received_requests.load(std::sync::atomic::Ordering::Relaxed), 3);

    // The same batch, intact, is stored whole.
    let r = post(&s, "/api/v1/events", "application/x-ndjson", Some("gzip"), gz).await;
    assert_eq!(r.json::<Value>().await.unwrap(), json!({ "accepted": 20 }));
    assert_eq!(s.messages("").await.len(), 20);
    s.stop().await;
}

// The documented exception to all-or-nothing: OTLP records that cannot be
// represented are dropped and reported through `partialSuccess`, and the
// rest of the request is stored.
#[tokio::test(flavor = "multi_thread")]
async fn otlp_partial_success_is_reported() {
    let s = server().await;
    let now = telemetry::Timestamp::now().0;
    let body = json!({ "resourceSpans": [{ "scopeSpans": [{ "spans": [
        { "traceId": "5b8efff798038103d269b633813fc60c", "spanId": "eee19b7ec3c1b174", "name": "kept",
          "startTimeUnixNano": now.to_string(), "endTimeUnixNano": (now + 1_000_000).to_string() },
        { "name": "no ids", "startTimeUnixNano": now.to_string() },
    ]}]}]});
    let r = s.otlp_json("/v1/traces", body.to_string()).await;
    assert_eq!(r.status(), 200);
    let v: Value = r.json().await.unwrap();
    assert_eq!(v["partialSuccess"]["rejected"], "1");
    assert!(v["partialSuccess"]["errorMessage"].as_str().unwrap().contains("without trace id"));
    assert_eq!(count(&s, "traces").await, 1);
    s.stop().await;
}
