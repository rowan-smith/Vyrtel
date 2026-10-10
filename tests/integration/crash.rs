//! REL-01 end to end: kill the real `vyrtel` binary after HTTP
//! acknowledgements, restart it on the same data directory and check through
//! the public API that every acknowledged event is back exactly once.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::common::*;

struct Process {
    child: Child,
    url: String,
    client: reqwest::Client,
}

impl Process {
    async fn start(dir: &Path, durability: &str) -> Process {
        // Reserve a free port, then hand it to the server.
        let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let child = Command::new(env!("CARGO_BIN_EXE_vyrtel"))
            .args(["--data-dir", dir.to_str().unwrap(), "--bind", &format!("127.0.0.1:{port}"), "--log-level", "warn"])
            .env("VYRTEL_STORAGE_DURABILITY", durability)
            .env("VYRTEL_AUTH_ENABLED", "false")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let p = Process { child, url: format!("http://127.0.0.1:{port}"), client: reqwest::Client::new() };
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            if let Ok(r) = p.client.get(format!("{}/health", p.url)).send().await
                && r.status().is_success()
            {
                return p;
            }
            assert!(Instant::now() < deadline, "vyrtel did not become healthy");
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    async fn get(&self, path: &str) -> Value {
        self.client.get(format!("{}{path}", self.url)).send().await.unwrap().json().await.unwrap()
    }

    /// `kill -9` (TerminateProcess on Windows): no shutdown, no final fsync.
    fn kill(mut self) {
        self.child.kill().unwrap();
        self.child.wait().unwrap();
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn killed_server_keeps_acknowledged_events() {
    for durability in ["strict", "normal"] {
        let dir = tempfile::tempdir().unwrap();
        let p = Process::start(dir.path(), durability).await;

        // A client-side ledger of what the server acknowledged.
        let mut acked: Vec<String> = Vec::new();
        for b in 0..25 {
            let batch: Vec<Value> =
                (0..4).map(|e| json!({"message": format!("{durability}-b{b}-e{e}"), "service": "crash"})).collect();
            let r = p.client.post(format!("{}/api/v1/events", p.url)).json(&batch).send().await.unwrap();
            assert!(r.status().is_success(), "{}", r.status());
            assert_eq!(r.json::<Value>().await.unwrap()["accepted"], 4);
            acked.extend(batch.iter().map(|e| e["message"].as_str().unwrap().to_string()));
        }
        p.kill();

        let p = Process::start(dir.path(), durability).await;
        let v = p
            .client
            .post(format!("{}/api/v1/query/logs", p.url))
            .json(&json!({"query": "service = crash", "limit": 1000}))
            .send()
            .await
            .unwrap()
            .json::<Value>()
            .await
            .unwrap();
        let mut seen: BTreeMap<String, usize> = BTreeMap::new();
        for e in v["events"].as_array().unwrap() {
            *seen.entry(e["message"].as_str().unwrap().to_string()).or_default() += 1;
        }
        for m in &acked {
            assert_eq!(seen.get(m), Some(&1), "{durability}: acknowledged event {m} must be visible exactly once");
        }
        assert_eq!(seen.len(), acked.len(), "{durability}: nothing unacknowledged appears");

        let storage = p.get("/api/v1/system/storage").await;
        let rec = &storage["signals"]["logs"]["recovery"];
        assert_eq!(rec["walEventsReplayed"], acked.len(), "{durability}: {rec}");
        assert_eq!(rec["quarantined"], json!([]), "{durability}: a process kill damages nothing");
        p.kill();
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn storage_api_reports_quarantined_wal_tail() {
    let s = TestServer::start().await;
    s.ingest(json!({"message": "committed"})).await;
    s.ingest(json!({"message": "will be torn"})).await;
    let (config, dir) = s.stop().await;

    // Tear the final WAL record, as a crash mid-write would.
    let wal_dir = config.storage.path.join("wal").join("logs");
    let wal = std::fs::read_dir(&wal_dir).unwrap().flatten().map(|e| e.path()).next().unwrap();
    let len = std::fs::metadata(&wal).unwrap().len();
    std::fs::OpenOptions::new().write(true).open(&wal).unwrap().set_len(len - 5).unwrap();

    let s = TestServer::launch(config, dir).await;
    assert_eq!(s.messages("").await, ["committed"]);
    let storage = s.get_ok("/api/v1/system/storage").await;
    let q = &storage["signals"]["logs"]["recovery"]["quarantined"];
    assert_eq!(q.as_array().unwrap().len(), 1, "{storage}");
    assert_eq!(q[0]["signal"], "logs");
    assert_eq!(q[0]["kind"], "walTail");
    assert_eq!(q[0]["source"], wal.file_name().unwrap().to_str().unwrap());
    assert!(q[0]["reason"].as_str().unwrap().starts_with("incomplete record"), "{}", q[0]);
    let file = q[0]["file"].as_str().unwrap();
    assert!(file.starts_with("quarantine/logs/"), "{file}");
    assert!(s.data_dir().join(file).exists());
    assert_eq!(storage["signals"]["traces"]["recovery"]["quarantined"], json!([]));
    s.stop().await;
}
