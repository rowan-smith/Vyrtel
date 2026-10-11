//! REL-07: an upgrade reads data written by every supported older release,
//! refuses data from a newer one without changing a byte, and an upgrade
//! interrupted at any step resumes on the next start. The procedure is the
//! one in docs/configuration.md#upgrades-and-data-migrations.
//!
//! The older data comes from real releases: `tests/fixtures/datadir/v*/`
//! holds a data directory each release wrote, and the answers that release
//! gave to the verification queries before it was stopped (see
//! `make_datadir_fixture` below and tests/fixtures/datadir/README.md).
//! 0.1.0 was released as Observer, whose files Vyrtel does not read: its
//! fixture must be refused, not quarantined.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use server::App;
use server::config::Config;
use sha2::{Digest, Sha256};

use crate::common::*;

/// The admin password of every fixture (a test value).
const PASSWORD: &str = "fixture-password";

/// `tests/fixtures/datadir/v<version>/fixture.json`.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Fixture {
    /// `vyrtel --version` of the release that wrote it.
    written_by: String,
    /// An ingest key created by that release, and one it revoked.
    api_key: String,
    revoked_api_key: String,
    /// The verification answers that release gave before it was stopped.
    answers: Value,
    /// Every directory, and every file with its size and SHA-256, as that
    /// release left them: git keeps neither empty directories nor, without
    /// `.gitattributes`, binary bytes on every platform.
    layout: Layout,
}

#[derive(Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
struct Layout {
    dirs: Vec<String>,
    files: BTreeMap<String, FileSum>,
}

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
struct FileSum {
    bytes: u64,
    sha256: String,
}

fn rel(root: &Path, p: &Path) -> String {
    p.strip_prefix(root).unwrap().to_string_lossy().replace('\\', "/")
}

fn layout_of(root: &Path) -> Layout {
    fn walk(root: &Path, d: &Path, out: &mut Layout) {
        let mut entries: Vec<_> = std::fs::read_dir(d).unwrap().flatten().map(|e| e.path()).collect();
        entries.sort();
        for p in entries {
            if p.is_dir() {
                out.dirs.push(rel(root, &p));
                walk(root, &p, out);
            } else {
                let bytes = std::fs::read(&p).unwrap();
                let sha256 = Sha256::digest(&bytes).iter().map(|b| format!("{b:02x}")).collect();
                out.files.insert(rel(root, &p), FileSum { bytes: bytes.len() as u64, sha256 });
            }
        }
    }
    let mut out = Layout::default();
    walk(root, root, &mut out);
    out
}

fn fixtures_root() -> PathBuf {
    fixtures_dir().join("datadir")
}

/// Every committed fixture, oldest release first.
fn all_fixtures() -> Vec<(PathBuf, Fixture)> {
    let mut out: Vec<(PathBuf, Fixture)> = std::fs::read_dir(fixtures_root())
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.join("fixture.json").is_file())
        .map(|p| {
            let f = serde_json::from_slice(&std::fs::read(p.join("fixture.json")).unwrap()).unwrap();
            (p, f)
        })
        .collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    assert!(out.len() >= 2, "the fixtures for 0.1.0 and 0.1.1 are committed");
    out
}

/// Data from Observer 0.1.0, Vyrtel's first release under its earlier name.
fn is_observer(f: &Fixture) -> bool {
    f.written_by.starts_with("observer ")
}

/// The fixtures of releases this build must upgrade.
fn fixtures() -> Vec<(PathBuf, Fixture)> {
    let out: Vec<_> = all_fixtures().into_iter().filter(|(_, f)| !is_observer(f)).collect();
    assert!(!out.is_empty(), "the fixture for 0.1.1 is committed");
    out
}

/// Copy a fixture's data directory to a new temporary directory, exactly as
/// its release left it.
fn restore(fixture: &Path, f: &Fixture) -> tempfile::TempDir {
    let host = tempfile::tempdir().unwrap();
    let dir = host.path();
    for d in &f.layout.dirs {
        std::fs::create_dir_all(dir.join(d)).unwrap();
    }
    for name in f.layout.files.keys() {
        std::fs::copy(fixture.join("data").join(name), dir.join(name)).unwrap();
    }
    assert_eq!(
        layout_of(dir),
        f.layout,
        "{} restores byte for byte (is tests/fixtures/datadir marked binary in .gitattributes?)",
        fixture.display()
    );
    host
}

/// The stored admin user signs in: no `admin_password`. Nothing expires.
fn config(dir: &Path) -> Config {
    let mut c = base_config(dir);
    c.auth.enabled = true;
    c.auth.admin_password = None;
    c.retention.logs = None;
    c.retention.traces = None;
    c.retention.metrics = None;
    c
}

/// `vyrtel check-data-dir` on `dir`: exit code, stdout, stderr.
fn check_data_dir(dir: &Path) -> (Option<i32>, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_vyrtel"))
        .args(["check-data-dir", "--data-dir", dir.to_str().unwrap()])
        .output()
        .unwrap();
    (out.status.code(), String::from_utf8_lossy(&out.stdout).into(), String::from_utf8_lossy(&out.stderr).into())
}

/// Every file and directory with its bytes (the `.lock` file reads as empty).
fn snapshot(dir: &Path) -> Layout {
    layout_of(dir)
}

fn manifest(dir: &Path) -> Value {
    serde_json::from_slice(&std::fs::read(dir.join("vyrtel.json")).unwrap()).unwrap()
}

/// Every verification query answers as the release that wrote the data
/// did, keys and sign-in behave as they did, and nothing was quarantined.
async fn assert_reads_like_its_release(s: &TestServer, f: &Fixture, release: &str) {
    let a = Admin::login(s, PASSWORD).await;
    assert_eq!(verification(&a).await, f.answers, "{release}: verification answers");
    let storage = a.get("/api/v1/system/storage").await;
    for signal in ["logs", "traces", "metrics"] {
        assert_eq!(storage["signals"][signal]["recovery"]["quarantined"], json!([]), "{release}: {signal}");
    }
    let ingest = |key: &str| {
        s.client
            .post(s.u("/api/v1/events"))
            .bearer_auth(key)
            .header("content-type", "application/json")
            .body(r#"{"message":"after upgrade"}"#)
            .send()
    };
    assert_eq!(ingest(&f.revoked_api_key).await.unwrap().status(), 401, "{release}: revoked key");
    assert_eq!(s.client.get(s.u("/api/v1/dashboards")).send().await.unwrap().status(), 401, "{release}: anonymous");
}

/// AC1, TC-01, TC-04: the documented upgrade (check, then start) on data
/// from each supported older release.
#[tokio::test(flavor = "multi_thread")]
async fn data_from_every_supported_release_is_readable_after_the_upgrade() {
    let this = env!("CARGO_PKG_VERSION");
    for (path, f) in fixtures() {
        let release = &f.written_by;
        let host = restore(&path, &f);
        let dir = host.path().to_path_buf();

        // 1. Check first: read-only, and it says what will change.
        let before = snapshot(&dir);
        let (code, stdout, stderr) = check_data_dir(&dir);
        assert_eq!(code, Some(0), "{release}: {stderr}");
        assert!(stdout.contains("  vyrtel.json: none (written before 0.1.2"), "{release}: {stdout}");
        assert!(stdout.contains("  metadata/metadata.db: schema version 1\n"), "{release}: {stdout}");
        assert!(
            stdout.ends_with(
                "Starting it will:\n  - record data-directory format 1 in vyrtel.json (it has none: written \
                 before 0.1.2)\nAfterwards every release that reads it now (Vyrtel 0.1.1 and later) can still read \
                 it.\n"
            ),
            "{release}: {stdout}"
        );
        assert_eq!(snapshot(&dir), before, "{release}: check-data-dir changed nothing");

        // 2. Start: the data reads as it did, and the format is recorded.
        let s = TestServer::launch(config(&dir), Some(host)).await;
        assert_reads_like_its_release(&s, &f, release).await;
        let r = s
            .client
            .post(s.u("/api/v1/events"))
            .bearer_auth(&f.api_key)
            .header("content-type", "application/json")
            .body(r#"{"message":"after upgrade"}"#)
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 200, "{release}: its ingest key still works");
        assert_eq!(manifest(&dir), json!({"format": 1, "writtenBy": this}), "{release}");

        // 3. Later starts migrate nothing and still read everything.
        let s = s.restart().await;
        let a = Admin::login(&s, PASSWORD).await;
        let counts = verification(&a).await["counts"].clone();
        let logs = f.answers["counts"]["logs"].as_u64().unwrap();
        assert_eq!(counts["logs"], logs + 1, "{release}");
        assert_eq!(counts["traces"], f.answers["counts"]["traces"], "{release}");
        let (_, dir_host) = s.stop().await;
        let (code, stdout, _) = check_data_dir(dir_host.as_ref().unwrap().path());
        assert_eq!(code, Some(0));
        assert!(stdout.contains("Starting it migrates nothing.\n"), "{stdout}");
    }
}

/// AC2, TC-03: a directory a newer Vyrtel wrote is refused by
/// `check-data-dir` and by a start, both naming the versions this build
/// reads, with every byte left as it was.
#[tokio::test(flavor = "multi_thread")]
async fn future_formats_are_refused_read_only_naming_the_supported_versions() {
    let (path, f) = fixtures().pop().unwrap();
    let host = restore(&path, &f);
    let dir = host.path();
    // Upgraded once, as a newer release's backup would have been by it.
    let s = TestServer::launch(config(dir), None).await;
    s.stop().await;

    let refused = |expected: &str| {
        let before = snapshot(dir);
        let (code, stdout, stderr) = check_data_dir(dir);
        assert_eq!(code, Some(1), "{stdout}{stderr}");
        assert!(stderr.starts_with("vyrtel: data directory "), "{stderr}");
        assert!(stderr.contains(expected), "{stderr}");
        assert!(stderr.contains("Vyrtel did not start and changed nothing in it"), "{stderr}");
        assert_eq!(snapshot(dir), before, "check-data-dir changed nothing");
        let err = format!("{:#}", App::build(config(dir)).err().expect("the start is refused"));
        assert!(err.contains(expected), "{err}");
        assert_eq!(snapshot(dir), before, "the refused start changed nothing");
    };

    let original = std::fs::read(dir.join("vyrtel.json")).unwrap();
    std::fs::write(dir.join("vyrtel.json"), r#"{"format": 2, "writtenBy": "0.3.0"}"#).unwrap();
    refused("has format 2 (written by Vyrtel 0.3.0), but Vyrtel");
    refused("reads only format 1");
    std::fs::write(dir.join("vyrtel.json"), &original).unwrap();

    let db = dir.join("metadata").join("metadata.db");
    let schema = |v: i64| {
        let c = rusqlite::Connection::open(&db).unwrap();
        if v > 1 {
            c.execute("INSERT INTO schema_migrations (version, applied_at) VALUES (?1, 0)", [v]).unwrap();
        } else {
            c.execute("DELETE FROM schema_migrations WHERE version > 1", []).unwrap();
        }
    };
    schema(2);
    refused("metadata/metadata.db has schema version 2, and this version knows schema versions up to 1");
    schema(1);

    // A checksummed WAL record in a newer format, in the last signal
    // recovered, while the first signal's WAL has a torn tail that recovery
    // would truncate: before REL-07 that recovery ran before the newer record
    // was found. Now the read-only check finds it first.
    let wal_of = |signal: &str| {
        let mut w: Vec<PathBuf> = std::fs::read_dir(dir.join("wal").join(signal))
            .unwrap()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|e| e == "wal"))
            .collect();
        w.sort();
        w.pop().expect("a WAL")
    };
    let metrics = wal_of("metrics");
    let logs = wal_of("logs");
    let (metrics_bytes, logs_bytes) = (std::fs::read(&metrics).unwrap(), std::fs::read(&logs).unwrap());
    let mut record = vec![2u8, 2, 0, 0, 0, 1, 0];
    let crc = crc32fast::hash(&record);
    record.extend_from_slice(&crc.to_le_bytes());
    std::fs::write(&metrics, [metrics_bytes.as_slice(), &record].concat()).unwrap();
    std::fs::write(&logs, [logs_bytes.as_slice(), &[1, 9, 9]].concat()).unwrap();
    refused(&format!("{} uses WAL record format version 2, and this version reads only 1", rel(dir, &metrics)));
    std::fs::write(&metrics, &metrics_bytes).unwrap();
    std::fs::write(&logs, &logs_bytes).unwrap();

    // Back to the original bytes, it starts and reads like its release.
    let s = TestServer::launch(config(dir), None).await;
    assert_reads_like_its_release(&s, &f, &f.written_by).await;
    s.stop().await;
}

/// AC2: data from Observer 0.1.0 is in a format no Vyrtel reads. It is
/// refused with what to do, and nothing is quarantined: 0.1.1 moved every
/// such file to `quarantine/` as damage, hiding all of its telemetry.
#[tokio::test(flavor = "multi_thread")]
async fn observer_data_is_refused_unchanged() {
    let observer: Vec<_> = all_fixtures().into_iter().filter(|(_, f)| is_observer(f)).collect();
    assert_eq!(observer.len(), 1, "the Observer 0.1.0 fixture is committed");
    for (path, f) in observer {
        let host = restore(&path, &f);
        let dir = host.path();
        let before = snapshot(dir);
        let (code, stdout, stderr) = check_data_dir(dir);
        assert_eq!(code, Some(1), "{stdout}{stderr}");
        assert!(stderr.contains(" was written by Observer 0.1.0, Vyrtel's first release under its earlier name ("));
        assert!(stderr.contains("Vyrtel did not start and changed nothing in it. Keep running Observer 0.1.0 on it"));
        assert_eq!(snapshot(dir), before, "check-data-dir changed nothing");
        let err = format!("{:#}", App::build(config(dir)).err().expect("the start is refused"));
        assert!(err.contains("was written by Observer 0.1.0"), "{err}");
        assert_eq!(snapshot(dir), before, "nothing quarantined, locked or created");
    }
}

/// AC4, TC-02: interrupt the upgrade at every point where the start writes
/// to the data directory, then start again: the upgrade completes and every
/// event and setting reads as the release that wrote them answered.
#[cfg(feature = "fault-injection")]
#[tokio::test(flavor = "multi_thread")]
async fn an_upgrade_interrupted_at_any_step_resumes_on_the_next_start() {
    use storage::fault::inject::{Injected, inject_times, record};

    let this = env!("CARGO_PKG_VERSION");
    for (path, f) in fixtures() {
        let release = &f.written_by;
        // Every write the upgrade start makes, in order.
        let points: Vec<String> = {
            let host = restore(&path, &f);
            let recorder = record(host.path());
            let app = App::build(config(host.path())).unwrap();
            let mut points = recorder.points();
            drop(recorder);
            let _ = app.shutdown().await;
            points.dedup();
            points
        };
        assert!(points.contains(&"manifest.write".to_string()), "{release}: {points:?}");

        let mut failed_starts = 0;
        for point in &points {
            let host = restore(&path, &f);
            let dir = host.path();
            let telemetry_before: BTreeMap<_, _> = layout_of(dir)
                .files
                .into_iter()
                .filter(|(name, _)| name.starts_with("wal/") || name.starts_with("segments/"))
                .collect();
            {
                let _fault = inject_times(dir, point, Injected::NoSpace, 1);
                match App::build(config(dir)) {
                    Err(e) => {
                        failed_starts += 1;
                        let text = format!("{e:#}");
                        assert!(text.contains("(injected)"), "{release} {point}: {text}");
                        if point == "manifest.write" {
                            // The format is recorded before storage recovery
                            // touches telemetry: none of it changed.
                            assert!(!dir.join("vyrtel.json").exists(), "{release}");
                            let telemetry_after: BTreeMap<_, _> = layout_of(dir)
                                .files
                                .into_iter()
                                .filter(|(name, _)| name.starts_with("wal/") || name.starts_with("segments/"))
                                .collect();
                            assert_eq!(telemetry_after, telemetry_before, "{release}: telemetry untouched");
                            let (code, stdout, _) = check_data_dir(dir);
                            assert_eq!(code, Some(0), "{release}");
                            assert!(stdout.contains("  - record data-directory format 1"), "{stdout}");
                        }
                    }
                    // A write a background task made after the start.
                    Ok(app) => {
                        let _ = app.shutdown().await;
                    }
                }
            }
            let s = TestServer::launch(config(dir), Some(host)).await;
            assert_reads_like_its_release(&s, &f, &format!("{release} after {point} failed")).await;
            assert_eq!(manifest(&s.data_dir())["format"], 1);
            assert_eq!(manifest(&s.data_dir())["writtenBy"], this);
            s.stop().await;
        }
        assert!(failed_starts >= 1, "{release}: some injected failure interrupted the upgrade: {points:?}");
    }
}

/// Writes a fixture with a released binary. Not part of the suite: run it
/// once per release, on Linux or macOS (it stops the server with SIGTERM),
/// as tests/fixtures/datadir/README.md describes.
///
/// ```text
/// VYRTEL_FIXTURE_BINARY=/path/to/vyrtel-0.1.1 VYRTEL_FIXTURE_OUT=tests/fixtures/datadir/v0.1.1 \
///   cargo test -p server --test integration -- --ignored make_datadir_fixture
/// ```
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
#[ignore = "writes a fixture; run by hand with VYRTEL_FIXTURE_BINARY and VYRTEL_FIXTURE_OUT"]
async fn make_datadir_fixture() {
    let binary = PathBuf::from(std::env::var("VYRTEL_FIXTURE_BINARY").expect("VYRTEL_FIXTURE_BINARY"));
    let out = PathBuf::from(std::env::var("VYRTEL_FIXTURE_OUT").expect("VYRTEL_FIXTURE_OUT"));
    assert!(!out.exists(), "{} exists; remove it first", out.display());
    let version = Command::new(&binary).arg("--version").output().unwrap();
    let written_by = String::from_utf8(version.stdout).unwrap().trim().to_string();
    let data = out.join("data");
    std::fs::create_dir_all(&data).unwrap();

    // Observer 0.1.0 reads OBSERVER_* variables.
    let prefix = if written_by.starts_with("observer ") { "OBSERVER" } else { "VYRTEL" };
    let settings = [
        ("AUTH_ENABLED", "true"),
        ("AUTH_ADMIN_PASSWORD", PASSWORD),
        // Seal the first batches into segments; later ones stay in the WAL.
        ("STORAGE_SEGMENT_MAX_AGE", "1s"),
        ("RETENTION_LOGS", "forever"),
        ("RETENTION_TRACES", "forever"),
        ("RETENTION_METRICS", "forever"),
    ]
    .map(|(k, v)| (format!("{prefix}_{k}"), v));
    let env: Vec<(&str, &str)> = settings.iter().map(|(k, v)| (k.as_str(), *v)).collect();
    let mut p = Process::start_binary(&binary, &data, &env).await;
    let a = Admin::login_at(&p.client, &p.url, PASSWORD).await;
    let api_key = a.post("/api/v1/api-keys", json!({"name": "producers"})).await["key"].as_str().unwrap().to_string();
    let retired = a.post("/api/v1/api-keys", json!({"name": "retired"})).await;
    assert_eq!(a.delete(&format!("/api/v1/api-keys/{}", retired["id"])).await, 204);
    let ingest = |path: &'static str, content_type: &'static str, body: String| {
        p.client
            .post(format!("{}{path}", p.url))
            .bearer_auth(&api_key)
            .header("content-type", content_type)
            .body(body)
            .send()
    };
    assert_eq!(ingest("/api/v1/events", "application/x-ndjson", fixture("logs.ndjson")).await.unwrap().status(), 200);
    assert_eq!(ingest("/v1/traces", "application/json", fixture("otlp-traces.json")).await.unwrap().status(), 200);
    let sealed = |signal: &str| {
        std::fs::read_dir(data.join("segments").join(signal))
            .map(|d| d.flatten().any(|e| e.path().extension().is_some_and(|x| x == "seg")))
            .unwrap_or(false)
    };
    wait_until("logs and traces are sealed", || sealed("logs") && sealed("traces")).await;
    assert_eq!(ingest("/v1/logs", "application/json", fixture("otlp-logs.json")).await.unwrap().status(), 200);
    assert_eq!(ingest("/v1/metrics", "application/json", fixture("otlp-metrics.json")).await.unwrap().status(), 200);

    let d = a.post("/api/v1/dashboards", json!({"name": "Checkout", "description": "upgrade fixture"})).await;
    a.post(
        &format!("/api/v1/dashboards/{}/panels", d["id"]),
        json!({"title": "Errors", "kind": "log_count", "config": {"query": "level = Error"}}),
    )
    .await;
    a.post(
        "/api/v1/alerts",
        json!({"name": "Error spike", "query": "level = Error", "op": ">", "threshold": 5.0,
               "windowSecs": 300, "intervalSecs": 60, "enabled": false}),
    )
    .await;
    a.post(
        "/api/v1/saved-queries",
        json!({"name": "Inventory warnings", "signal": "logs", "query": "level = Warning"}),
    )
    .await;
    let answers = verification(&a).await;
    for signal in ["logs", "traces", "metrics"] {
        assert!(answers["counts"][signal].as_u64().unwrap() > 0, "{signal}: {answers}");
    }

    p.terminate();
    let status = p.wait_exit(std::time::Duration::from_secs(30)).await;
    assert!(status.success(), "{written_by} stopped cleanly: {}", p.stderr());
    let f = Fixture {
        written_by,
        api_key,
        revoked_api_key: retired["key"].as_str().unwrap().to_string(),
        answers,
        layout: layout_of(&data),
    };
    let mut text = serde_json::to_string_pretty(&f).unwrap();
    text.push('\n');
    std::fs::write(out.join("fixture.json"), text).unwrap();
}
