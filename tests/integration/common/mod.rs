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
    /// Sends `api_key` with every request.
    pub client: reqwest::Client,
    /// An admin-scoped key, created at launch.
    pub api_key: String,
    dir: Option<tempfile::TempDir>,
    stop: Option<oneshot::Sender<()>>,
    force: Option<oneshot::Sender<()>>,
    handle: Option<JoinHandle<anyhow::Result<()>>>,
    state: Option<server::state::SharedState>,
}

/// A client that sends `header: value` with every request.
pub fn authorized_client(header: &'static str, value: &str) -> reqwest::Client {
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(header, reqwest::header::HeaderValue::from_str(value).unwrap());
    reqwest::Client::builder().default_headers(headers).build().unwrap()
}

pub fn base_config(dir: &Path) -> Config {
    let mut c = Config::default();
    c.storage.path = dir.to_path_buf();
    c.server.bind = "127.0.0.1:0".into();
    c.log.level = "warn".into();
    // As long as `stop` waits, so a loaded machine does not fail unrelated
    // tests; the shutdown tests set their own.
    c.server.shutdown_timeout = Duration::from_secs(30);
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
        Self::launch_at(config, dir, "127.0.0.1:0").await
    }

    /// Like [`TestServer::launch`], listening on `addr`.
    pub async fn launch_at(config: Config, dir: Option<tempfile::TempDir>, addr: &str) -> TestServer {
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
        let (api_key, prefix, hash) = server::auth::new_api_key();
        state.metadata.create_api_key(HARNESS_KEY, &prefix, &hash, "admin").unwrap();
        let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let (tx, rx) = oneshot::channel::<()>();
        let (force_tx, force_rx) = oneshot::channel::<()>();
        // A dropped sender must not count as a request.
        let requested = |rx: oneshot::Receiver<()>| async move {
            if rx.await.is_err() {
                std::future::pending::<()>().await;
            }
        };
        let handle = tokio::spawn(app.serve_until(listener, requested(rx), requested(force_rx)));
        TestServer {
            url,
            config,
            client: authorized_client("authorization", &format!("Bearer {api_key}")),
            api_key,
            dir,
            stop: Some(tx),
            force: Some(force_tx),
            handle: Some(handle),
            state: Some(state),
        }
    }

    /// Send the stop request (as SIGTERM does) without waiting.
    pub fn begin_stop(&mut self) {
        if let Some(tx) = self.stop.take() {
            let _ = tx.send(());
        }
    }

    /// Send a second stop request.
    pub fn force_stop(&mut self) {
        if let Some(tx) = self.force.take() {
            let _ = tx.send(());
        }
    }

    /// Wait for the server to finish serving; returns how it ended, and
    /// what [`TestServer::launch`] needs to start it again.
    pub async fn stopped(mut self) -> (anyhow::Result<()>, Config, Option<tempfile::TempDir>) {
        self.state.take();
        self.begin_stop();
        let h = self.handle.take().unwrap();
        let r = tokio::time::timeout(Duration::from_secs(30), h).await.expect("server stops in time").unwrap();
        (r, self.config.clone(), self.dir.take())
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
        self.begin_stop();
        if let Some(h) = self.handle.take() {
            tokio::time::timeout(Duration::from_secs(30), h).await.expect("server stops in time").unwrap().unwrap();
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

/// Make a cleanly stopped data directory look as a crash leaves it: without
/// the inventory a clean shutdown records in `vyrtel.json`. For tests that
/// damage files to simulate a crash after stopping the server normally.
pub fn as_after_crash(dir: &Path) {
    let path = dir.join("vyrtel.json");
    let mut m: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    m.as_object_mut().unwrap().remove("inventory");
    std::fs::write(path, serde_json::to_vec(&m).unwrap()).unwrap();
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
    wait_until_within(what, Duration::from_secs(10), cond).await
}

/// Poll `cond` until it holds; panic after `timeout`.
pub async fn wait_until_within(what: &str, timeout: Duration, cond: impl Fn() -> bool) {
    let deadline = std::time::Instant::now() + timeout;
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
        let mut head = format!(
            "POST {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\nAuthorization: Bearer {}\r\n",
            s.api_key
        );
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

    /// The response status, if a status line arrives within `wait`.
    pub async fn status_line(&mut self, wait: Duration) -> Option<u16> {
        use tokio::io::AsyncReadExt;
        let mut buf = [0u8; 64];
        let n = tokio::time::timeout(wait, self.tcp.read(&mut buf)).await.ok()?.ok()?;
        let text = String::from_utf8_lossy(&buf[..n]).to_string();
        text.strip_prefix("HTTP/1.1 ")?.get(..3)?.parse().ok()
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

/// The real `vyrtel` binary on its own port and data directory, for tests of
/// what only a process can show: kills, signals, exit codes.
pub struct Process {
    pub child: std::process::Child,
    pub url: String,
    pub client: reqwest::Client,
    /// The admin key `client` sends (empty from [`Process::start_binary`]).
    pub api_key: String,
    stderr: tempfile::NamedTempFile,
}

/// Name of the admin keys the harness creates; [`verification`] leaves
/// them out, since every start adds one.
pub const HARNESS_KEY: &str = "tests";

/// The admin password [`Process::start`] gives the server.
pub const PROCESS_PASSWORD: &str = "process admin";

impl Process {
    /// Start with `env`, wait until it is healthy, and log in as admin:
    /// `client` then sends the session cookie with every request.
    pub async fn start(dir: &Path, env: &[(&str, &str)]) -> Process {
        let env: Vec<(&str, &str)> =
            [("VYRTEL_AUTH_ADMIN_PASSWORD", PROCESS_PASSWORD)].into_iter().chain(env.iter().copied()).collect();
        let mut p = Process::start_binary(Path::new(env!("CARGO_BIN_EXE_vyrtel")), dir, &env).await;
        let a = Admin::login_at(&p.client, &p.url, PROCESS_PASSWORD).await;
        let key = a.post("/api/v1/api-keys", json!({"name": HARNESS_KEY, "scope": "admin"})).await;
        p.api_key = key["key"].as_str().unwrap().to_string();
        p.client = authorized_client("authorization", &format!("Bearer {}", p.api_key));
        p
    }

    /// [`Process::start`] with another Vyrtel binary, such as an older release.
    pub async fn start_binary(binary: &Path, dir: &Path, env: &[(&str, &str)]) -> Process {
        use std::process::{Command, Stdio};
        // Reserve a free port, then hand it to the server.
        let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let stderr = tempfile::NamedTempFile::new().unwrap();
        let child = Command::new(binary)
            .args(["--data-dir", dir.to_str().unwrap(), "--bind", &format!("127.0.0.1:{port}"), "--log-level", "warn"])
            .envs(env.iter().copied())
            .stdout(Stdio::null())
            .stderr(stderr.reopen().unwrap())
            .spawn()
            .unwrap();
        let p = Process {
            child,
            url: format!("http://127.0.0.1:{port}"),
            client: reqwest::Client::new(),
            api_key: String::new(),
            stderr,
        };
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        loop {
            if let Ok(r) = p.client.get(format!("{}/health", p.url)).send().await
                && r.status().is_success()
            {
                return p;
            }
            assert!(std::time::Instant::now() < deadline, "vyrtel did not become healthy: {}", p.stderr());
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    pub async fn get(&self, path: &str) -> Value {
        self.client.get(format!("{}{path}", self.url)).send().await.unwrap().json().await.unwrap()
    }

    /// Everything the process wrote to stderr so far.
    pub fn stderr(&self) -> String {
        std::fs::read_to_string(self.stderr.path()).unwrap_or_default()
    }

    /// `kill -9` (TerminateProcess on Windows): no shutdown, no final fsync.
    pub fn kill(mut self) {
        self.child.kill().unwrap();
        self.child.wait().unwrap();
    }

    /// Send SIGTERM, as `docker stop`, systemd and Kubernetes do.
    #[cfg(unix)]
    pub fn terminate(&self) {
        let ok = std::process::Command::new("kill")
            .args(["-TERM", &self.child.id().to_string()])
            .status()
            .unwrap()
            .success();
        assert!(ok, "kill -TERM failed");
    }

    /// The exit status, once the process has exited; `None` while it runs.
    pub fn exited(&mut self) -> Option<std::process::ExitStatus> {
        self.child.try_wait().unwrap()
    }

    /// Wait up to `limit` for the process to exit.
    pub async fn wait_exit(&mut self, limit: Duration) -> std::process::ExitStatus {
        let deadline = std::time::Instant::now() + limit;
        loop {
            if let Some(status) = self.exited() {
                return status;
            }
            assert!(std::time::Instant::now() < deadline, "vyrtel did not exit within {limit:?}: {}", self.stderr());
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Minimal SSE reader over reqwest's chunked body.
pub struct Sse {
    resp: reqwest::Response,
    buf: String,
    /// The last `retry:` field received, in milliseconds.
    pub retry: Option<u64>,
}

impl Sse {
    /// Subscribe to the logs live tail and wait for `ready`.
    pub async fn open(s: &TestServer, query: &str) -> Sse {
        let resp = s.client.get(s.u("/api/v1/live/logs")).query(&[("query", query)]).send().await.unwrap();
        assert_eq!(resp.status(), 200);
        assert!(resp.headers()["content-type"].to_str().unwrap().starts_with("text/event-stream"));
        let mut sse = Sse { resp, buf: String::new(), retry: None };
        let (kind, _) = sse.next().await;
        assert_eq!(kind, "ready");
        sse
    }

    /// Next (event, data), skipping keep-alive comments.
    pub async fn next(&mut self) -> (String, String) {
        self.next_or_end().await.expect("stream still open")
    }

    /// Next (event, data), or `None` once the server ended the response
    /// cleanly (a reset or truncated body fails the test).
    pub async fn next_or_end(&mut self) -> Option<(String, String)> {
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
                    } else if let Some(r) = line.strip_prefix("retry:") {
                        self.retry = r.trim().parse().ok();
                    }
                }
                if data.is_empty() && kind == "message" {
                    continue;
                }
                return Some((kind, data));
            }
            let chunk = tokio::time::timeout(Duration::from_secs(10), self.resp.chunk())
                .await
                .expect("SSE data within timeout")
                .expect("SSE body ends cleanly");
            match chunk {
                Some(chunk) => self.buf.push_str(&String::from_utf8_lossy(&chunk)),
                None => {
                    assert!(self.buf.trim().is_empty(), "partial frame at end of stream: {:?}", self.buf);
                    return None;
                }
            }
        }
    }
}

/// The trace id every shared fixture uses (tests/fixtures/README.md).
pub const FIXTURE_TRACE: &str = "4bf92f3577b34da6a3ce929d0e0e4736";

/// A logged-in admin session against any Vyrtel: an in-process
/// [`TestServer`] or a [`Process`], including older releases.
pub struct Admin {
    pub client: reqwest::Client,
    pub url: String,
    pub cookie: String,
}

impl Admin {
    pub async fn login(s: &TestServer, password: &str) -> Admin {
        Admin::login_at(&s.client, &s.url, password).await
    }

    pub async fn login_at(client: &reqwest::Client, url: &str, password: &str) -> Admin {
        let r = client
            .post(format!("{url}/api/v1/auth/login"))
            .json(&json!({"username": "admin", "password": password}))
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 200, "admin login");
        let cookie = r.headers()["set-cookie"].to_str().unwrap().split(';').next().unwrap().to_string();
        Admin { client: client.clone(), url: url.to_string(), cookie }
    }

    pub async fn get(&self, path: &str) -> Value {
        let r = self.client.get(format!("{}{path}", self.url)).header("cookie", &self.cookie).send().await.unwrap();
        assert_eq!(r.status(), 200, "GET {path}");
        r.json().await.unwrap()
    }

    pub async fn post(&self, path: &str, body: Value) -> Value {
        let r = self
            .client
            .post(format!("{}{path}", self.url))
            .header("cookie", &self.cookie)
            .json(&body)
            .send()
            .await
            .unwrap();
        assert!(r.status().is_success(), "POST {path} → {}", r.status());
        r.json().await.unwrap_or(Value::Null)
    }

    pub async fn delete(&self, path: &str) -> u16 {
        let r = self.client.delete(format!("{}{path}", self.url)).header("cookie", &self.cookie).send().await.unwrap();
        r.status().as_u16()
    }
}

/// The documented verification queries (docs/configuration.md#verification-queries),
/// as one comparable value.
pub async fn verification(a: &Admin) -> Value {
    let mut counts = json!({});
    for signal in ["logs", "traces", "metrics"] {
        counts[signal] = a.post(&format!("/api/v1/query/{signal}/count"), json!({"query": ""})).await["count"].clone();
    }
    let trace = a.get(&format!("/api/v1/traces/{FIXTURE_TRACE}")).await;
    let mut span_names: Vec<String> =
        trace["spans"].as_array().unwrap().iter().map(|s| s["name"].as_str().unwrap().to_string()).collect();
    span_names.sort();
    json!({
        "counts": counts,
        "errorLogs": a.post("/api/v1/query/logs", json!({"query": "level = Error", "limit": 100})).await["events"]
            .as_array().unwrap().iter().map(|e| e["message"].clone()).collect::<Vec<_>>(),
        "traceSpans": span_names,
        "metricNames": a.get("/api/v1/metrics").await["metrics"],
        "dashboards": a.get("/api/v1/dashboards").await,
        "alerts": a.get("/api/v1/alerts").await["alerts"].as_array().unwrap().iter()
            .map(|al| json!({"id": al["id"], "name": al["name"], "query": al["query"], "op": al["op"],
                             "threshold": al["threshold"], "enabled": al["enabled"]}))
            .collect::<Vec<_>>(),
        "savedQueries": a.get("/api/v1/saved-queries").await,
        "apiKeys": a.get("/api/v1/api-keys").await["apiKeys"].as_array().unwrap().iter()
            .filter(|k| k["name"] != HARNESS_KEY)
            .map(|k| json!({"id": k["id"], "name": k["name"], "prefix": k["prefix"], "scope": k["scope"]}))
            .collect::<Vec<_>>(),
    })
}
