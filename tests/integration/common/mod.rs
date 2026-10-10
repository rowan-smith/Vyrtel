//! Test harness: runs a real Vyrtel server on an ephemeral port.

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};
use server::App;
use server::config::Config;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

pub struct TestServer {
    pub url: String,
    pub config: Config,
    pub client: reqwest::Client,
    dir: Option<tempfile::TempDir>,
    stop: Option<oneshot::Sender<()>>,
    handle: Option<JoinHandle<()>>,
    state: Option<server::state::SharedState>,
}

pub fn base_config(dir: &Path) -> Config {
    let mut c = Config::default();
    c.storage.path = dir.to_path_buf();
    c.server.bind = "127.0.0.1:0".into();
    c.log.level = "warn".into();
    c
}

impl TestServer {
    pub async fn start() -> TestServer {
        Self::start_with(|_| {}).await
    }

    pub async fn start_with(f: impl FnOnce(&mut Config)) -> TestServer {
        let dir = tempfile::tempdir().unwrap();
        let mut c = base_config(dir.path());
        f(&mut c);
        Self::launch(c, Some(dir)).await
    }

    pub async fn launch(config: Config, dir: Option<tempfile::TempDir>) -> TestServer {
        // On Unix a process spawned by a parallel test (crash.rs starts the
        // real binary) inherits our data-directory lock until it execs, so a
        // restart can briefly see the lock as held. Retry only that error,
        // and not for long: a lock that is really leaked still fails.
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let app = loop {
            match App::build(config.clone()) {
                Ok(app) => break app,
                Err(e) if format!("{e:#}").contains("in use by another Vyrtel process") => {
                    assert!(std::time::Instant::now() < deadline, "server starts: {e:#}");
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
                Err(e) => panic!("server starts: {e:#}"),
            }
        };
        let state = app.state.clone();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let (tx, rx) = oneshot::channel::<()>();
        let handle = tokio::spawn(async move {
            app.serve(listener, async move {
                let _ = rx.await;
            })
            .await
            .unwrap();
        });
        TestServer {
            url,
            config,
            client: reqwest::Client::new(),
            dir,
            stop: Some(tx),
            handle: Some(handle),
            state: Some(state),
        }
    }

    pub fn state(&self) -> &server::state::SharedState {
        self.state.as_ref().unwrap()
    }

    pub fn data_dir(&self) -> PathBuf {
        self.config.storage.path.clone()
    }

    /// Graceful shutdown (flush, release the data directory lock).
    pub async fn stop(mut self) -> (Config, Option<tempfile::TempDir>) {
        self.state.take();
        if let Some(tx) = self.stop.take() {
            let _ = tx.send(());
        }
        if let Some(h) = self.handle.take() {
            tokio::time::timeout(Duration::from_secs(30), h).await.expect("server stops in time").unwrap();
        }
        (self.config.clone(), self.dir.take())
    }

    pub async fn restart(self) -> TestServer {
        let (config, dir) = self.stop().await;
        Self::launch(config, dir).await
    }

    pub fn u(&self, path: &str) -> String {
        format!("{}{path}", self.url)
    }

    pub async fn post(&self, path: &str, body: Value) -> reqwest::Response {
        self.client.post(self.u(path)).json(&body).send().await.unwrap()
    }

    pub async fn post_ok(&self, path: &str, body: Value) -> Value {
        let r = self.post(path, body).await;
        let status = r.status();
        let text = r.text().await.unwrap();
        assert!(status.is_success(), "POST {path} → {status}: {text}");
        serde_json::from_str(&text).unwrap_or(Value::Null)
    }

    pub async fn get_ok(&self, path: &str) -> Value {
        let r = self.client.get(self.u(path)).send().await.unwrap();
        let status = r.status();
        let text = r.text().await.unwrap();
        assert!(status.is_success(), "GET {path} → {status}: {text}");
        serde_json::from_str(&text).unwrap()
    }

    pub async fn ingest(&self, events: Value) -> Value {
        self.post_ok("/api/v1/events", events).await
    }

    pub async fn ingest_ndjson(&self, body: String) -> reqwest::Response {
        self.client
            .post(self.u("/api/v1/events"))
            .header("content-type", "application/x-ndjson")
            .body(body)
            .send()
            .await
            .unwrap()
    }

    pub async fn otlp_json(&self, path: &str, body: String) -> reqwest::Response {
        self.client.post(self.u(path)).header("content-type", "application/json").body(body).send().await.unwrap()
    }

    pub async fn query(&self, q: &str) -> Value {
        self.post_ok("/api/v1/query/logs", json!({ "query": q, "limit": 1000 })).await
    }

    pub async fn messages(&self, q: &str) -> Vec<String> {
        let v = self.query(q).await;
        let mut m: Vec<String> =
            v["events"].as_array().unwrap().iter().map(|e| e["message"].as_str().unwrap().to_string()).collect();
        m.sort();
        m
    }

    /// Seal the active segment of every signal into segments now.
    pub fn rotate_all(&self) {
        for s in telemetry::Signal::ALL {
            self.state().storage.rotate_blocking(s).unwrap();
        }
    }
}

pub fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures")
}

/// Load a fixture, replacing `{{now-<n><unit>}}` placeholders with Unix
/// nanoseconds relative to the current time.
pub fn fixture(name: &str) -> String {
    let text = std::fs::read_to_string(fixtures_dir().join(name)).unwrap();
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos() as i64;
    let mut out = String::with_capacity(text.len());
    let mut rest = text.as_str();
    while let Some(start) = rest.find("{{now") {
        out.push_str(&rest[..start]);
        let end = rest[start..].find("}}").unwrap() + start;
        let expr = &rest[start + 5..end]; // e.g. "-900ms"
        let offset = if expr.is_empty() {
            0
        } else {
            let e = expr.trim_start_matches('-');
            let (num, mult) = if let Some(n) = e.strip_suffix("ms") {
                (n, 1_000_000)
            } else if let Some(n) = e.strip_suffix('s') {
                (n, 1_000_000_000)
            } else if let Some(n) = e.strip_suffix('m') {
                (n, 60_000_000_000)
            } else {
                panic!("bad placeholder {expr}")
            };
            num.parse::<i64>().unwrap() * mult
        };
        out.push_str(&(now - offset).to_string());
        rest = &rest[end + 2..];
    }
    out.push_str(rest);
    out
}

/// Ingest requests holding an admission permit right now.
pub fn ingest_in_flight(s: &TestServer) -> usize {
    s.state().ingest_max_concurrent - s.state().ingest_permits.available_permits()
}

/// Poll `cond` until it holds; panic after 10 s.
pub async fn wait_until(what: &str, cond: impl Fn() -> bool) {
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while !cond() {
        assert!(std::time::Instant::now() < deadline, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

/// NDJSON with `n` events of roughly `pad` bytes each.
pub fn ndjson_payload(n: usize, pad: usize) -> String {
    let filler = "x".repeat(pad);
    (0..n)
        .map(|i| {
            format!(
                "{{\"message\":\"event {i}\",\"service\":\"load\",\"properties\":{{\"pad\":\"{filler}\",\"n\":{i}}}}}\n"
            )
        })
        .collect()
}

pub fn gzip(data: &[u8]) -> Vec<u8> {
    use std::io::Write;
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    gz.write_all(data).unwrap();
    gz.finish().unwrap()
}

/// An HTTP/1.1 request over a raw socket, so a test controls exactly when
/// body bytes arrive and when the client goes away.
pub struct RawRequest {
    pub tcp: tokio::net::TcpStream,
}

impl RawRequest {
    /// Send the request line and headers for a body of `content_length`
    /// bytes, and no body yet.
    pub async fn open(s: &TestServer, path: &str, headers: &[(&str, &str)], content_length: usize) -> RawRequest {
        use tokio::io::AsyncWriteExt;
        let addr = s.url.trim_start_matches("http://");
        let mut tcp = tokio::net::TcpStream::connect(addr).await.unwrap();
        let mut head = format!("POST {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n");
        for (k, v) in headers {
            head.push_str(&format!("{k}: {v}\r\n"));
        }
        head.push_str(&format!("Content-Length: {content_length}\r\n\r\n"));
        tcp.write_all(head.as_bytes()).await.unwrap();
        RawRequest { tcp }
    }

    pub async fn send(&mut self, bytes: &[u8]) {
        use tokio::io::AsyncWriteExt;
        self.tcp.write_all(bytes).await.unwrap();
        self.tcp.flush().await.unwrap();
    }

    /// Read the whole response; returns the status code and the raw text.
    pub async fn response(mut self) -> (u16, String) {
        use tokio::io::AsyncReadExt;
        let mut buf = Vec::new();
        let _ = tokio::time::timeout(Duration::from_secs(30), self.tcp.read_to_end(&mut buf)).await;
        let text = String::from_utf8_lossy(&buf).to_string();
        let status = text.split_whitespace().nth(1).and_then(|c| c.parse().ok()).unwrap_or(0);
        (status, text)
    }
}

/// Park the writer thread of `signal` until the returned sender is dropped,
/// so submitted batches wait in the queue unacknowledged. Must be called
/// while the signal's active buffer is empty: rotation then has nothing to
/// seal and runs the callback on the writer thread itself.
pub fn stall_writer(s: &TestServer, signal: telemetry::Signal) -> std::sync::mpsc::Sender<()> {
    let (entered_tx, entered_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    s.state().storage.stream(signal).rotate(Box::new(move |_| {
        let _ = entered_tx.send(());
        let _ = release_rx.recv();
    }));
    entered_rx.recv_timeout(Duration::from_secs(10)).expect("writer stalled");
    release_tx
}

pub fn queue_depth(s: &TestServer, signal: telemetry::Signal) -> usize {
    s.state().storage.stats().get(signal).queue_depth
}
