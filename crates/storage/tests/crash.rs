//! Process-kill crash tests (REL-01).
//!
//! Unlike `recovery.rs`, which fabricates the files a crash would leave,
//! these tests run the real write path in a child process and kill it: at a
//! named crash point (`VYRTEL_CRASH_AT`, `abort()` ≈ `kill -9`) or after a
//! random delay. The child keeps a durable client-side **ledger**: one
//! fsynced line per acknowledged batch. After the kill the parent recovers the
//! data directory and checks, for both durability modes:
//!
//! * every acknowledged batch is visible exactly once, with the ids it was
//!   acknowledged with;
//! * nothing else is visible except, at most, the single batch per producer
//!   that was in flight at the kill — and only whole;
//! * a second recovery gives the identical result and new ids never collide.
//!
//! Needs `--features crash-points`:
//!
//! ```text
//! cargo test -p storage --features crash-points --test crash
//! ```
//!
//! The child is this test binary re-invoked to run [`child_entry`] only.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use storage::*;
use telemetry::*;

const BATCH_EVENTS: usize = 5;
/// Tag of the probe event `verify` appends after recovery.
const POST_RECOVERY: &str = "post-recovery";

// ---------------------------------------------------------------- child --

fn env(k: &str) -> Option<String> {
    std::env::var(k).ok()
}

fn durability(name: &str) -> Durability {
    match name {
        "strict" => Durability::Strict,
        "normal" => Durability::Normal,
        other => panic!("unknown durability {other}"),
    }
}

fn config(dir: &Path, d: Durability) -> StorageConfig {
    let mut c = StorageConfig::new(dir);
    c.durability = d;
    // Maintenance only when a scenario asks for it, so crash points fire at
    // predictable moments.
    c.maintenance_interval = Duration::from_secs(3600);
    c.compaction.enabled = true;
    c.compaction.min_inputs = 2;
    c
}

fn event(tag: &str, k: usize) -> TelemetryEvent {
    let now = Timestamp::now();
    TelemetryEvent {
        id: EventId(0),
        timestamp: now,
        observed_timestamp: now,
        service: Some("crash-test".into()),
        environment: None,
        trace_id: None,
        span_id: None,
        resource: Fields::new(),
        attributes: Fields::new(),
        payload: TelemetryPayload::Log(LogEvent {
            level: Level::Information,
            message: format!("{tag}-e{k}"),
            message_template: None,
            exception: None,
        }),
    }
}

fn submit(s: &Storage, events: Vec<TelemetryEvent>) -> Result<Committed> {
    let (tx, rx) = mpsc::sync_channel(1);
    s.submit(
        Signal::Logs,
        events,
        Box::new(move |r| {
            let _ = tx.send(r);
        }),
    )?;
    rx.recv_timeout(Duration::from_secs(30)).expect("ack")
}

/// Durable client-side record of acknowledged batches.
struct Ledger(Mutex<File>);

impl Ledger {
    fn record(&self, run: u32, producer: usize, batch: u64, c: Committed) {
        // One write per line: the kill may land mid-record, and a line
        // without its newline is ignored by the reader (never recorded).
        let line = format!("{run} {producer} {batch} {} {}\n", c.first_id.0, c.count);
        let mut f = self.0.lock().unwrap();
        f.write_all(line.as_bytes()).unwrap();
        f.sync_data().unwrap();
    }
}

/// Entry point of the child process; a no-op in a normal test run.
#[test]
fn child_entry() {
    let Some(scenario) = env("VYRTEL_CRASH_SCENARIO") else {
        return;
    };
    let dir = PathBuf::from(env("VYRTEL_CRASH_DIR").unwrap());
    let d = durability(&env("VYRTEL_CRASH_DURABILITY").unwrap());
    let s = Arc::new(Storage::open(config(&dir, d), None).unwrap());
    if scenario == "open" {
        // Recovery itself is the subject; nothing else to do.
        std::process::exit(0);
    }
    let ledger = Arc::new(Ledger(Mutex::new(
        OpenOptions::new().create(true).append(true).open(env("VYRTEL_CRASH_LEDGER").unwrap()).unwrap(),
    )));
    let run: u32 = env("VYRTEL_CRASH_RUN").unwrap().parse().unwrap();
    let producers: usize = env("VYRTEL_CRASH_PRODUCERS").unwrap().parse().unwrap();
    let batches: u64 = env("VYRTEL_CRASH_BATCHES").unwrap().parse().unwrap();

    let threads: Vec<_> = (0..producers)
        .map(|p| {
            let (s, ledger, scenario) = (s.clone(), ledger.clone(), scenario.clone());
            std::thread::spawn(move || {
                for b in 0..batches {
                    let tag = format!("r{run}-p{p}-b{b}");
                    let events = (0..BATCH_EVENTS).map(|k| event(&tag, k)).collect();
                    match submit(&s, events) {
                        Ok(c) => ledger.record(run, p, b, c),
                        Err(e) => panic!("{e}"),
                    }
                    match scenario.as_str() {
                        "rotate" if b % 5 == 4 => s.rotate_blocking(Signal::Logs).unwrap(),
                        "compact" => {
                            s.rotate_blocking(Signal::Logs).unwrap();
                            if b % 4 == 3 {
                                s.maintain_blocking(Signal::Logs).unwrap();
                            }
                        }
                        _ => {}
                    }
                }
            })
        })
        .collect();
    for t in threads {
        t.join().unwrap();
    }
    if env("VYRTEL_CRASH_AT").is_some() {
        eprintln!("crash point was never reached");
        std::process::exit(3);
    }
    std::process::exit(0);
}

// --------------------------------------------------------------- parent --

struct Child<'a> {
    dir: &'a Path,
    durability: &'a str,
    scenario: &'a str,
    run: u32,
    producers: usize,
    batches: u64,
    crash_at: Option<&'a str>,
}

impl Child<'_> {
    fn ledger(&self) -> PathBuf {
        self.dir.with_extension(format!("ledger-{}", self.run))
    }

    fn command(&self) -> Command {
        let mut c = Command::new(std::env::current_exe().unwrap());
        c.args(["child_entry", "--exact", "--nocapture", "--test-threads=1"])
            .env("VYRTEL_CRASH_SCENARIO", self.scenario)
            .env("VYRTEL_CRASH_DIR", self.dir)
            .env("VYRTEL_CRASH_DURABILITY", self.durability)
            .env("VYRTEL_CRASH_LEDGER", self.ledger())
            .env("VYRTEL_CRASH_RUN", self.run.to_string())
            .env("VYRTEL_CRASH_PRODUCERS", self.producers.to_string())
            .env("VYRTEL_CRASH_BATCHES", self.batches.to_string())
            .env_remove("VYRTEL_CRASH_AT")
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        if let Some(p) = self.crash_at {
            c.env("VYRTEL_CRASH_AT", p);
        }
        c
    }

    /// Run until the crash point aborts the process.
    fn crash(&self) {
        let out = self.command().output().unwrap();
        let stderr = String::from_utf8_lossy(&out.stderr);
        let point = self.crash_at.unwrap().split(':').next().unwrap();
        assert!(!out.status.success(), "child exited cleanly; expected a crash at {point}\n{stderr}");
        assert!(stderr.contains(&format!("crash point {point} reached")), "child did not crash at {point}: {stderr}");
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct BatchKey {
    run: u32,
    producer: usize,
    batch: u64,
}

fn parse_message(m: &str) -> (BatchKey, usize) {
    let parts: Vec<&str> = m.split('-').collect();
    let field = |i: usize, p: char| -> u64 {
        parts
            .get(i)
            .and_then(|s| s.strip_prefix(p))
            .and_then(|s| s.parse().ok())
            .unwrap_or_else(|| panic!("fabricated or foreign event: {m:?}"))
    };
    assert_eq!(parts.len(), 4, "fabricated or foreign event: {m:?}");
    let key = BatchKey { run: field(0, 'r') as u32, producer: field(1, 'p') as usize, batch: field(2, 'b') };
    (key, field(3, 'e') as usize)
}

fn read_ledgers(dir: &Path, runs: u32) -> BTreeMap<BatchKey, Committed> {
    let mut out = BTreeMap::new();
    for run in 0..runs {
        let path = dir.with_extension(format!("ledger-{run}"));
        let Ok(text) = std::fs::read_to_string(&path) else { continue };
        // Only newline-terminated lines were fully recorded.
        let complete = text.rfind('\n').map_or("", |i| &text[..=i]);
        for line in complete.lines() {
            let f: Vec<u64> = line.split(' ').map(|x| x.parse().unwrap()).collect();
            let key = BatchKey { run: f[0] as u32, producer: f[1] as usize, batch: f[2] };
            out.insert(key, Committed { first_id: EventId(f[3]), count: f[4] as usize });
        }
    }
    out
}

fn visible(s: &Storage) -> Vec<TelemetryEvent> {
    let snap = s.snapshot(Signal::Logs);
    let mut out = Vec::new();
    for seg in &snap.segments {
        out.extend(seg.read_all().unwrap());
    }
    for b in &snap.batches {
        out.extend(b.events.iter().cloned());
    }
    out.sort_by_key(|e| e.id);
    out
}

/// What a check found, for the scenario's own assertions.
struct Outcome {
    acknowledged: usize,
    visible_batches: usize,
    report: RecoveryReport,
}

/// Recover `dir` and check the acknowledgement contract against the ledgers.
fn verify(dir: &Path, durability: Durability, runs: u32) -> Outcome {
    let ledger = read_ledgers(dir, runs);
    let mut c = config(dir, durability);
    c.compaction.enabled = false;
    let s = Storage::open(c.clone(), None).unwrap();
    let report = s.recovery_report(Signal::Logs).clone();
    let events = visible(&s);

    // Exactly once: ids and messages are unique.
    let ids: BTreeSet<u64> = events.iter().map(|e| e.id.0).collect();
    assert_eq!(ids.len(), events.len(), "duplicate event ids after recovery");
    let mut batches: BTreeMap<BatchKey, Vec<(usize, EventId)>> = BTreeMap::new();
    for e in events.iter().filter(|e| !e.message().starts_with(POST_RECOVERY)) {
        let (key, k) = parse_message(e.message());
        batches.entry(key).or_default().push((k, e.id));
    }

    // Batches are all-or-nothing and keep the ids they were assigned.
    for (key, members) in &mut batches {
        members.sort();
        let ks: Vec<usize> = members.iter().map(|m| m.0).collect();
        assert_eq!(ks, (0..BATCH_EVENTS).collect::<Vec<_>>(), "{key:?} is partial or duplicated: {ks:?}");
        let first = members[0].1.0;
        for (k, id) in members.iter() {
            assert_eq!(id.0, first + *k as u64, "{key:?} ids are not contiguous");
        }
    }

    // Every acknowledged batch is visible with the ids it was acknowledged with.
    for (key, c) in &ledger {
        let got = batches.get(key).unwrap_or_else(|| panic!("acknowledged batch {key:?} lost ({c:?})"));
        assert_eq!(got[0].1, c.first_id, "{key:?} visible with different ids than acknowledged");
        assert_eq!(got.len(), c.count);
    }

    // Per producer, visible batches are contiguous and exceed the ledger by
    // at most the one batch in flight at the kill.
    let mut per_producer: BTreeMap<(u32, usize), Vec<u64>> = BTreeMap::new();
    for key in batches.keys() {
        per_producer.entry((key.run, key.producer)).or_default().push(key.batch);
    }
    for ((run, p), nums) in &per_producer {
        assert_eq!(*nums, (0..nums.len() as u64).collect::<Vec<_>>(), "r{run}-p{p}: gap in visible batches");
        let acked = ledger.keys().filter(|k| k.run == *run && k.producer == *p).count() as u64;
        assert!(
            nums.len() as u64 <= acked + 1,
            "r{run}-p{p}: {} visible batches but only {acked} acknowledged",
            nums.len()
        );
    }

    // New writes continue the id sequence.
    let max_id = ids.iter().max().copied().unwrap_or(0);
    drop(s);

    // Recovery is idempotent: a second restart sees the identical data.
    let s = Storage::open(c, None).unwrap();
    let again = visible(&s);
    assert_eq!(again.len(), events.len(), "second recovery changed the visible event count");
    assert!(again.iter().zip(&events).all(|(a, b)| a.id == b.id && a.message() == b.message()));
    assert!(s.recovery_report(Signal::Logs).quarantined.is_empty(), "second recovery quarantined something");
    let next = submit(&s, vec![event(POST_RECOVERY, 0)]).unwrap();
    assert!(next.first_id.0 > max_id, "new id {} collides with recovered ids (max {max_id})", next.first_id.0);
    // Leave the directory as recovery left it for the next round.
    drop(s);

    Outcome { acknowledged: ledger.len(), visible_batches: batches.len(), report }
}

/// Crash at `point` while running `scenario`, then verify. Returns the outcome.
fn crash_and_verify(scenario: &str, durability_name: &str, point: &str, producers: usize, batches: u64) -> Outcome {
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("data");
    Child { dir: &dir, durability: durability_name, scenario, run: 0, producers, batches, crash_at: Some(point) }
        .crash();
    let out = verify(&dir, durability(durability_name), 1);
    assert!(
        out.report.quarantined.is_empty(),
        "a process crash at {point} must not damage files: {:?}",
        out.report.quarantined
    );
    assert!(out.acknowledged > 0, "crash at {point} came before any acknowledgement; test proves nothing");
    out
}

const MODES: [&str; 2] = ["strict", "normal"];

// TC-02: kill before and after the WAL sync and around the acknowledgement.

#[test]
fn kill_before_wal_sync() {
    for mode in MODES {
        let o = crash_and_verify("ingest", mode, "wal.before_sync:40", 3, 1000);
        // The batch written but not fsynced/acknowledged may surface (the
        // OS still had it) — whole, and at most one per producer.
        assert!(o.visible_batches <= o.acknowledged + 3, "{mode}");
    }
}

#[test]
fn kill_after_wal_sync_before_ack() {
    for mode in MODES {
        crash_and_verify("ingest", mode, "wal.before_ack:40", 3, 1000);
    }
}

#[test]
fn kill_after_ack() {
    for mode in MODES {
        crash_and_verify("ingest", mode, "wal.after_ack:40", 3, 1000);
    }
}

// TC-02 / AC3: kill during sealing — around the segment rename.

#[test]
fn kill_during_rotation_and_seal() {
    for mode in MODES {
        for point in [
            "rotate.after_wal_create:2",
            "segment.before_rename:2",
            "segment.after_rename:2",
            "seal.before_wal_delete:2",
        ] {
            let o = crash_and_verify("rotate", mode, point, 1, 1000);
            // Sealing runs between requests: every visible batch was
            // acknowledged, so visible counts equal the committed dataset.
            assert_eq!(o.visible_batches, o.acknowledged, "{mode} {point}");
        }
    }
}

// AC3: kill during compaction.

#[test]
fn kill_during_compaction() {
    for mode in MODES {
        for point in ["compact.before_input_delete", "compact.mid_input_delete", "segment.before_rename:7"] {
            let o = crash_and_verify("compact", mode, point, 1, 1000);
            assert_eq!(o.visible_batches, o.acknowledged, "{mode} {point}");
        }
    }
}

// AC3: kill while recovery is itself sealing WALs left by an earlier crash.

#[test]
fn kill_during_recovery_seal() {
    for mode in MODES {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("data");
        // First crash leaves an unsealed WAL plus a fresh one.
        let first = Child {
            dir: &dir,
            durability: mode,
            scenario: "rotate",
            run: 0,
            producers: 1,
            batches: 1000,
            crash_at: Some("rotate.after_wal_create"),
        };
        first.crash();
        // Second crash happens inside recovery, after sealing the old WAL
        // into a segment but before deleting the WAL.
        Child { scenario: "open", crash_at: Some("recovery.before_wal_delete"), ..first }.crash();
        let o = verify(&dir, durability(mode), 1);
        assert_eq!(o.visible_batches, o.acknowledged, "{mode}");
        assert_eq!(o.report.stale_wals_removed, 1, "{mode}: recovery must treat the sealed WAL as stale");
    }
}

// TC-02: random kills (SIGKILL / TerminateProcess) under concurrent load,
// accumulated over several restarts of the same data directory.

/// Small deterministic generator so a failing seed can be replayed.
fn lcg(state: &mut u64) -> u64 {
    *state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
    *state >> 33
}

#[test]
fn random_kills_under_load() {
    let seed: u64 = env("VYRTEL_CRASH_SEED").and_then(|s| s.parse().ok()).unwrap_or(0x5eed_0001);
    eprintln!("random_kills_under_load seed = {seed} (set VYRTEL_CRASH_SEED to replay)");
    for mode in MODES {
        let mut rng = seed;
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("data");
        let runs = 4;
        for run in 0..runs {
            let child = Child {
                dir: &dir,
                durability: mode,
                scenario: if run % 2 == 0 { "ingest" } else { "rotate" },
                run,
                producers: 4,
                batches: u64::MAX,
                crash_at: None,
            };
            let mut proc = child.command().spawn().unwrap();
            // Wait for some acknowledgements, then kill at a random moment.
            let deadline = Instant::now() + Duration::from_secs(60);
            while std::fs::read_to_string(child.ledger()).map_or(0, |t| t.matches('\n').count()) < 20 {
                assert!(Instant::now() < deadline, "child made no progress");
                assert!(proc.try_wait().unwrap().is_none(), "child exited early");
                std::thread::sleep(Duration::from_millis(5));
            }
            std::thread::sleep(Duration::from_millis(lcg(&mut rng) % 150));
            proc.kill().unwrap();
            proc.wait().unwrap();
            let o = verify(&dir, durability(mode), run + 1);
            assert!(o.acknowledged >= 20, "{mode} run {run}");
        }
    }
}
