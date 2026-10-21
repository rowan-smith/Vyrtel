//! COMPAT-GATE-0.1.2 (FR-AC3, FR-AC4): before 1.0 a release reads only data
//! directories of its own layout ([`telemetry::LAYOUT`]). Data written by
//! another release, including every release before 0.1.2 (which wrote no
//! layout identity, although its files say version 1 like this build's), is
//! refused by `check-data-dir` and by a start, with nothing in it changed:
//! never migrated, guessed at, quarantined or reset. The documented way
//! forward is a new, empty data directory; the old one stays as it was. See
//! docs/configuration.md#upgrading-between-pre-10-releases.
//!
//! The older data comes from real releases: `tests/fixtures/datadir/v*/`
//! holds a data directory each release wrote, and the answers that release
//! gave to the verification queries before it was stopped (see
//! `make_datadir_fixture` below and tests/fixtures/datadir/README.md). A
//! fixture of this build's layout must read as its release answered; every
//! other fixture must be refused unchanged.

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

/// The layout identity a fixture's manifest records; `None` without one
/// (every release before 0.1.2).
fn layout_identity(fixture: &Path) -> Option<String> {
    let bytes = std::fs::read(fixture.join("data").join("vyrtel.json")).ok()?;
    let m: Value = serde_json::from_slice(&bytes).ok()?;
    m["layout"].as_str().map(str::to_string)
}

/// The fixtures of this build's layout, or of any other.
fn fixtures(same_layout: bool) -> Vec<(PathBuf, Fixture)> {
    all_fixtures()
        .into_iter()
        .filter(|(p, _)| (layout_identity(p).as_deref() == Some(telemetry::LAYOUT)) == same_layout)
        .collect()
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

/// `check-data-dir`, a start and a start with `--accept-incomplete-restore`
/// all refuse `dir` with `expected` in the reason, and leave every byte of
/// it as it was: every acknowledged event of the release that wrote it is
/// still where that release put it.
fn assert_refused_unchanged(dir: &Path, expected: &str) {
    let before = snapshot(dir);
    let (code, stdout, stderr) = check_data_dir(dir);
    assert_eq!(code, Some(1), "{stdout}{stderr}");
    assert!(stderr.starts_with("vyrtel: data directory "), "{stderr}");
    assert!(stderr.contains(expected), "{stderr}");
    assert!(stderr.contains("Vyrtel did not start and changed nothing in it"), "{stderr}");
    assert_eq!(snapshot(dir), before, "check-data-dir changed nothing");
    let err = format!("{:#}", App::build(config(dir)).err().expect("the start is refused"));
    assert!(err.contains(expected), "{err}");
    let options = server::app::StartOptions { accept_incomplete_restore: true };
    let err = format!("{:#}", App::build_with(config(dir), options).err().expect("never accepted"));
    assert!(err.contains(expected), "{err}");
    assert_eq!(snapshot(dir), before, "nothing locked, created, migrated, quarantined or truncated");
}

/// Ingest the shared fixtures of every signal (authentication off).
async fn ingest_fixtures(s: &TestServer) {
    assert_eq!(s.ingest_ndjson(fixture("logs.ndjson")).await.status(), 200);
    assert_eq!(s.otlp_json("/v1/logs", fixture("otlp-logs.json")).await.status(), 200);
    assert_eq!(s.otlp_json("/v1/traces", fixture("otlp-traces.json")).await.status(), 200);
    assert_eq!(s.otlp_json("/v1/metrics", fixture("otlp-metrics.json")).await.status(), 200);
}

/// Event counts per signal (authentication off).
async fn counts(s: &TestServer) -> Value {
    let mut out = json!({});
    for signal in ["logs", "traces", "metrics"] {
        out[signal] = s.post_ok(&format!("/api/v1/query/{signal}/count"), json!({"query": ""})).await["count"].clone();
    }
    out
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
            .body(r#"{"message":"after the switch"}"#)
            .send()
    };
    assert_eq!(ingest(&f.revoked_api_key).await.unwrap().status(), 401, "{release}: revoked key");
    assert_eq!(ingest(&f.api_key).await.unwrap().status(), 200, "{release}: its ingest key works");
    assert_eq!(s.client.get(s.u("/api/v1/dashboards")).send().await.unwrap().status(), 401, "{release}: anonymous");
}

/// FR-AC2, FR-AC4, TC3: data from every release of another layout (0.1.0,
/// released as Observer, and 0.1.1) is refused before anything changes,
/// with what to do. The documented way forward, a new data directory next
/// to the old one, starts and works, and the old one is still refused
/// byte for byte as it was.
#[tokio::test(flavor = "multi_thread")]
async fn releases_of_another_layout_are_refused_unchanged() {
    let this = env!("CARGO_PKG_VERSION");
    let other = fixtures(false);
    assert!(other.len() >= 2, "0.1.0 and 0.1.1 have no layout identity");
    for (path, f) in other {
        let release = &f.written_by;
        let host = restore(&path, &f);
        let dir = host.path();
        assert_refused_unchanged(
            dir,
            &format!(
                "is not in the layout Vyrtel {this} reads: there is no vyrtel.json, and metadata/metadata.db has no layout identity"
            ),
        );
        let (_, _, stderr) = check_data_dir(dir);
        assert!(
            stderr.contains(&format!(
                "Keep running the Vyrtel version that wrote it, or start Vyrtel {this} on a new, empty data directory \
                 (see docs/configuration.md#upgrading-between-pre-10-releases)"
            )),
            "{release}: {stderr}"
        );

        // The documented procedure, on this disposable copy: leave the old
        // directory as it is, start on a new one beside it.
        let before = snapshot(dir);
        let fresh = tempfile::tempdir().unwrap();
        let (code, stdout, _) = check_data_dir(fresh.path());
        assert_eq!(code, Some(0), "{release}: {stdout}");
        assert!(stdout.ends_with("Starting it will create a new data directory in this layout.\n"), "{stdout}");
        let s = TestServer::launch(base_config(fresh.path()), Some(fresh)).await;
        ingest_fixtures(&s).await;
        let s = s.restart().await;
        assert_eq!(counts(&s).await, f.answers["counts"], "{release}: the same telemetry, sent again");
        s.stop().await;
        assert_eq!(snapshot(dir), before, "{release}: the old directory is untouched");
    }
}

/// A fixture of this build's layout (one per release that kept it) reads
/// as its release answered. None is committed yet: 0.1.2 introduces the
/// layout, and its fixture is added after its release
/// (tests/fixtures/datadir/README.md).
#[tokio::test(flavor = "multi_thread")]
async fn releases_of_this_layout_read_like_their_release() {
    for (path, f) in fixtures(true) {
        let release = &f.written_by;
        let host = restore(&path, &f);
        let dir = host.path().to_path_buf();
        let before = snapshot(&dir);
        let (code, stdout, stderr) = check_data_dir(&dir);
        assert_eq!(code, Some(0), "{release}: {stderr}");
        assert!(stdout.ends_with("Starting it changes no file formats.\n"), "{release}: {stdout}");
        assert_eq!(snapshot(&dir), before, "{release}: check-data-dir changed nothing");
        let s = TestServer::launch(config(&dir), Some(host)).await;
        assert_reads_like_its_release(&s, &f, release).await;
        s.stop().await;
    }
}

/// FR-AC3, TC3: files of an earlier release mixed into a directory of this
/// layout (a restore that combined two backups, a file copied in by hand)
/// are refused before any change, named, even though their version numbers
/// match this build's. Taken out again, the directory starts and has every
/// event it acknowledged.
#[tokio::test(flavor = "multi_thread")]
async fn mixed_directories_are_refused_before_any_change() {
    let this = env!("CARGO_PKG_VERSION");
    let (old_path, _) = all_fixtures().into_iter().find(|(_, f)| f.written_by == "vyrtel 0.1.1").unwrap();
    let old = old_path.join("data");

    let host = tempfile::tempdir().unwrap();
    let dir = host.path().to_path_buf();
    let c = config(&dir);
    let s = TestServer::launch(c.clone(), None).await;
    ingest_fixtures(&s).await;
    let acknowledged = counts(&s).await;
    s.stop().await;

    for (from, to, what) in [
        ("wal/logs/000000000002.wal", "wal/logs/000000000090.wal", "a WAL file with no layout identity"),
        (
            "segments/traces/000000000001.seg",
            "segments/traces/000000000091.seg",
            "a segment file with no layout identity",
        ),
    ] {
        std::fs::copy(old.join(from), dir.join(to)).unwrap();
        assert_refused_unchanged(&dir, &format!("is not in the layout Vyrtel {this} reads: {to} is {what}"));
        std::fs::remove_file(dir.join(to)).unwrap();
    }

    // The metadata database of 0.1.1 in place of this one's.
    let db = dir.join("metadata").join("metadata.db");
    let current = std::fs::read(&db).unwrap();
    std::fs::copy(old.join("metadata/metadata.db"), &db).unwrap();
    assert_refused_unchanged(&dir, "metadata/metadata.db has no layout identity (it has schema_migrations");
    std::fs::write(&db, current).unwrap();

    let s = TestServer::launch(c, Some(host)).await;
    assert_eq!(counts(&s).await, acknowledged, "every acknowledged event is there");
    s.stop().await;
}

/// TC1, FR-AC4: this layout's own directories restart, back up and
/// restore as they are, and `check-data-dir` reports nothing to change.
#[tokio::test(flavor = "multi_thread")]
async fn this_layout_restarts_and_restores_as_it_is() {
    let this = env!("CARGO_PKG_VERSION");
    let s = TestServer::start().await;
    ingest_fixtures(&s).await;
    let acknowledged = counts(&s).await;
    let s = s.restart().await;
    assert_eq!(counts(&s).await, acknowledged);
    let (_, host) = s.stop().await;
    let host = host.unwrap();

    // A backup of the stopped directory, restored elsewhere.
    let restored = tempfile::tempdir().unwrap();
    copy_dir(host.path(), restored.path());
    let (code, stdout, stderr) = check_data_dir(restored.path());
    assert_eq!(code, Some(0), "{stderr}");
    assert!(
        stdout.contains(&format!(
            "  vyrtel.json: layout {}, format 1, last written by Vyrtel {this}\n",
            telemetry::LAYOUT
        )),
        "{stdout}"
    );
    assert!(stdout.ends_with("Starting it changes no file formats.\n"), "{stdout}");
    let s = TestServer::launch(base_config(restored.path()), Some(restored)).await;
    assert_eq!(counts(&s).await, acknowledged);
    let m: Value = serde_json::from_slice(&std::fs::read(s.data_dir().join("vyrtel.json")).unwrap()).unwrap();
    assert_eq!(m, json!({"format": 1, "layout": telemetry::LAYOUT, "writtenBy": this}));
    s.stop().await;
}

fn copy_dir(from: &Path, to: &Path) {
    for entry in std::fs::read_dir(from).unwrap().flatten() {
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            std::fs::create_dir_all(&target).unwrap();
            copy_dir(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), target).unwrap();
        }
    }
}

/// FR-AC4, AC5: a first start interrupted at every point where it writes to
/// the data directory leaves a directory the next start finishes: the
/// metadata schema is one transaction, and the manifest is written
/// atomically before the first telemetry file.
#[cfg(feature = "fault-injection")]
#[tokio::test(flavor = "multi_thread")]
async fn a_first_start_interrupted_at_any_step_completes_on_the_next_start() {
    use storage::fault::inject::{Injected, inject_times, record};

    let points: Vec<String> = {
        let host = tempfile::tempdir().unwrap();
        let recorder = record(host.path());
        let app = App::build(base_config(host.path())).unwrap();
        let mut points = recorder.points();
        drop(recorder);
        let _ = app.shutdown().await;
        points.dedup();
        points
    };
    assert!(points.contains(&"manifest.write".to_string()), "{points:?}");

    let mut failed_starts = 0;
    for point in &points {
        let host = tempfile::tempdir().unwrap();
        let dir = host.path();
        {
            let _fault = inject_times(dir, point, Injected::NoSpace, 1);
            match App::build(base_config(dir)) {
                Err(e) => {
                    failed_starts += 1;
                    assert!(format!("{e:#}").contains("(injected)"), "{point}: {e:#}");
                    if point == "manifest.write" {
                        // Recorded before any telemetry file exists.
                        assert!(!dir.join("vyrtel.json").exists());
                        let wal_files = std::fs::read_dir(dir.join("wal").join("logs")).unwrap().count();
                        assert_eq!(wal_files, 0, "no telemetry file before the manifest");
                        let (code, stdout, _) = check_data_dir(dir);
                        assert_eq!(code, Some(0));
                        assert!(stdout.ends_with("record the layout in vyrtel.json.\n"), "{stdout}");
                    }
                }
                // A write a background task made after the start.
                Ok(app) => {
                    let _ = app.shutdown().await;
                }
            }
        }
        let s = TestServer::launch(base_config(dir), Some(host)).await;
        ingest_fixtures(&s).await;
        assert!(counts(&s).await["logs"].as_u64().unwrap() > 0, "{point}");
        let m: Value = serde_json::from_slice(&std::fs::read(s.data_dir().join("vyrtel.json")).unwrap()).unwrap();
        assert_eq!(m["layout"], telemetry::LAYOUT, "{point}");
        s.stop().await;
    }
    assert!(failed_starts >= 1, "some injected failure interrupted the first start: {points:?}");
}

/// Writes a fixture with a released binary. Not part of the suite: run it
/// once per release, on Linux or macOS (it stops the server with SIGTERM),
/// as tests/fixtures/datadir/README.md describes.
///
/// ```text
/// VYRTEL_FIXTURE_BINARY=/path/to/vyrtel-0.1.2 VYRTEL_FIXTURE_OUT=tests/fixtures/datadir/v0.1.2 \
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
    // Releases before 0.1.2 have authentication off unless enabled; later
    // ones always require a login and refuse the setting.
    let legacy_auth = ["observer 0.1.0", "vyrtel 0.1.1"].iter().any(|v| written_by.starts_with(v));
    let auth_enabled = if legacy_auth { ("AUTH_ENABLED", "true") } else { ("AUTH_INGEST_API_KEY", "true") };
    let settings = [
        auth_enabled,
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
