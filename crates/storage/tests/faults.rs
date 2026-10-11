//! Storage write failures (REL-04).
//!
//! Faults are injected at named points (`storage::fault::inject`, feature
//! `fault-injection`) for one temp data directory at a time. Each test
//! checks the acknowledgement contract (a failed write is never reported
//! as stored), the fault diagnostics, and that a restart afterwards brings
//! back exactly the acknowledged data.

use std::path::Path;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use storage::fault::inject::{Injected, inject};
use storage::*;
use telemetry::*;

fn config(dir: &Path, durability: Durability) -> StorageConfig {
    let mut c = StorageConfig::new(dir);
    c.durability = durability;
    c.maintenance_interval = Duration::from_secs(3600);
    c.compaction.enabled = false;
    c
}

fn log(i: i64, msg: &str) -> TelemetryEvent {
    TelemetryEvent {
        id: EventId(0),
        timestamp: Timestamp(1_700_000_000_000_000_000 + i * 1_000_000),
        observed_timestamp: Timestamp(1_700_000_000_000_000_000 + i * 1_000_000),
        service: Some("payments".into()),
        environment: None,
        trace_id: None,
        span_id: None,
        resource: Fields::new(),
        attributes: Fields::new(),
        payload: TelemetryPayload::Log(LogEvent {
            level: Level::Information,
            message: msg.into(),
            message_template: None,
            exception: None,
        }),
    }
}

/// Submit one event and wait for the outcome, whether the batch was turned
/// away at submission or failed in the writer.
fn try_submit(s: &Storage, msg: &str) -> Result<Committed> {
    let (tx, rx) = mpsc::sync_channel(1);
    s.submit(
        Signal::Logs,
        vec![log(0, msg)],
        Box::new(move |r| {
            let _ = tx.send(r);
        }),
    )?;
    rx.recv_timeout(Duration::from_secs(10)).expect("acknowledgement arrives")
}

fn submit_ok(s: &Storage, msg: &str) {
    try_submit(s, msg).unwrap_or_else(|e| panic!("{msg} is stored: {e}"));
}

fn write_failed(r: Result<Committed>) -> StorageFault {
    match r {
        Err(StorageError::WriteFailed(f)) => f,
        other => panic!("expected WriteFailed, got {other:?}"),
    }
}

fn messages(s: &Storage) -> Vec<String> {
    let snap = s.snapshot(Signal::Logs);
    let mut events: Vec<TelemetryEvent> = snap.segments.iter().flat_map(|seg| seg.read_all().unwrap()).collect();
    events.extend(snap.batches.iter().flat_map(|b| b.events.iter().cloned()));
    events.sort_by_key(|e| e.id);
    events.iter().map(|e| e.message().to_string()).collect()
}

fn faults(s: &Storage) -> Vec<StorageFault> {
    s.stats().logs.faults
}

fn wait_for(what: &str, mut cond: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !cond() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn expected_reason(f: Injected) -> FaultReason {
    match f {
        Injected::NoSpace => FaultReason::NoSpace,
        Injected::PermissionDenied => FaultReason::PermissionDenied,
        Injected::ReadOnly => FaultReason::ReadOnly,
        Injected::ShortWrite => FaultReason::ShortWrite,
    }
}

/// Restart and check that exactly `expected` is back and recovery found
/// nothing torn (a failed write left no partial record behind).
fn restart_and_check(dir: &Path, durability: Durability, expected: &[&str]) -> Storage {
    let s = Storage::open(config(dir, durability), None).unwrap();
    assert_eq!(messages(&s), expected);
    let r = s.recovery_report(Signal::Logs);
    assert_eq!(r.wal_bytes_discarded, 0, "no torn WAL tail after restart: {r:?}");
    assert!(r.quarantined.is_empty(), "nothing quarantined: {r:?}");
    s
}

#[test]
fn failed_wal_appends_are_never_acknowledged() {
    for durability in [Durability::Normal, Durability::Strict] {
        for fault in [Injected::NoSpace, Injected::PermissionDenied, Injected::ReadOnly, Injected::ShortWrite] {
            let d = tempfile::tempdir().unwrap();
            let s = Storage::open(config(d.path(), durability), None).unwrap();
            submit_ok(&s, "before");

            let guard = inject(d.path(), "wal.append", fault);
            let f = write_failed(try_submit(&s, "rejected 1"));
            write_failed(try_submit(&s, "rejected 2"));
            assert_eq!(f.op, FaultOp::WalAppend);
            assert_eq!(f.reason, expected_reason(fault), "{fault:?}");
            assert!(f.path.starts_with("wal/logs/") && f.path.ends_with(".wal"), "{}", f.path);

            let stats = s.stats().logs;
            let reported = stats.ingest_fault().expect("fault is reported");
            assert_eq!(reported.occurrences, 2);
            let host = d.path().to_string_lossy().to_string();
            assert!(!format!("{reported:?}").contains(&host), "no host path in {reported:?}");
            assert_eq!(messages(&s), ["before"], "{durability:?} {fault:?}");

            // Storage works again: the next write goes through and the fault clears.
            drop(guard);
            submit_ok(&s, "after");
            assert!(faults(&s).is_empty(), "{:?}", faults(&s));
            drop(s);

            restart_and_check(d.path(), durability, &["before", "after"]);
        }
    }
}

#[test]
fn failed_strict_fsync_rejects_and_removes_the_batch() {
    for fault in [Injected::NoSpace, Injected::PermissionDenied] {
        let d = tempfile::tempdir().unwrap();
        let s = Storage::open(config(d.path(), Durability::Strict), None).unwrap();
        submit_ok(&s, "before");

        let guard = inject(d.path(), "wal.sync", fault);
        let f = write_failed(try_submit(&s, "not synced"));
        assert_eq!((f.op, f.reason), (FaultOp::WalSync, expected_reason(fault)));
        assert_eq!(messages(&s), ["before"]);
        drop(guard);

        submit_ok(&s, "after");
        assert!(faults(&s).is_empty());
        drop(s);
        // The batch whose fsync failed was cut from the WAL, so a client
        // that retried it does not end up with two copies.
        restart_and_check(d.path(), Durability::Strict, &["before", "after"]);
    }
}

#[test]
fn normal_mode_waits_for_fsync_after_a_failed_background_sync() {
    let d = tempfile::tempdir().unwrap();
    let mut c = config(d.path(), Durability::Normal);
    c.fsync_interval = Duration::from_millis(20);
    let s = Storage::open(c.clone(), None).unwrap();

    let guard = inject(d.path(), "wal.sync", Injected::NoSpace);
    // Acknowledged before the background fsync runs: the normal contract.
    submit_ok(&s, "acknowledged before the fault");
    wait_for("background fsync fault", || faults(&s).iter().any(|f| f.op == FaultOp::WalSync));

    // From now on acknowledgements wait for an fsync, which fails.
    let f = write_failed(try_submit(&s, "rejected"));
    assert_eq!(f.op, FaultOp::WalSync);
    drop(guard);

    submit_ok(&s, "after");
    assert!(faults(&s).is_empty());
    drop(s);
    let s = Storage::open(c, None).unwrap();
    assert_eq!(messages(&s), ["acknowledged before the fault", "after"]);
}

#[test]
fn failed_seal_fails_fast_and_keeps_data_for_restart() {
    for (point, fault) in [
        ("segment.write", Injected::NoSpace),
        ("segment.write", Injected::PermissionDenied),
        ("segment.write", Injected::ShortWrite),
        ("segment.sync", Injected::ReadOnly),
    ] {
        let d = tempfile::tempdir().unwrap();
        let mut c = config(d.path(), Durability::Strict);
        // Every committed batch asks for a rotation.
        c.logs.segment_max_events = 1;
        let s = Storage::open(c, None).unwrap();

        let guard = inject(d.path(), point, fault);
        // The first batch is frozen for sealing, and the seal fails.
        submit_ok(&s, "first");
        wait_for("seal fault", || faults(&s).iter().any(|f| f.op == FaultOp::SegmentWrite));
        // The writer waits for that seal before it can rotate again; once
        // it does, new batches fail fast instead of timing out in the queue.
        let mut accepted = vec!["first".to_string()];
        let deadline = Instant::now() + Duration::from_secs(10);
        let f = loop {
            let msg = format!("accepted {}", accepted.len());
            match try_submit(&s, &msg) {
                Ok(_) => accepted.push(msg),
                Err(StorageError::WriteFailed(f)) => break f,
                Err(e) => panic!("unexpected error: {e}"),
            }
            assert!(Instant::now() < deadline, "writer never stalled");
        };
        assert_eq!((f.op, f.reason), (FaultOp::SegmentWrite, expected_reason(fault)), "{point}");
        assert!(f.path.starts_with("tmp/logs."), "{}", f.path);
        assert!(s.stats().logs.ingest_fault().is_some());
        assert_eq!(messages(&s), accepted);

        drop(s);
        drop(guard);
        // Everything acknowledged is still in the WAL and is sealed on startup.
        let s = Storage::open(config(d.path(), Durability::Strict), None).unwrap();
        assert_eq!(messages(&s), accepted, "{point} {fault:?}");
        assert!(s.recovery_report(Signal::Logs).quarantined.is_empty());
    }
}

#[test]
fn failed_wal_creation_stalls_rotation_until_storage_recovers() {
    let d = tempfile::tempdir().unwrap();
    let mut c = config(d.path(), Durability::Normal);
    c.logs.segment_max_events = 1;
    let s = Storage::open(c.clone(), None).unwrap();

    let guard = inject(d.path(), "wal.create", Injected::NoSpace);
    submit_ok(&s, "first");
    wait_for("WAL create fault", || faults(&s).iter().any(|f| f.op == FaultOp::WalCreate));
    let f = write_failed(try_submit(&s, "rejected"));
    assert_eq!((f.op, f.reason), (FaultOp::WalCreate, FaultReason::NoSpace));
    // The temp file of the failed attempt does not linger.
    assert!(std::fs::read_dir(d.path().join("tmp")).unwrap().next().is_none());

    drop(guard);
    // The writer retries the rotation by itself and ingest resumes.
    wait_for("rotation to recover", || try_submit(&s, "after").is_ok());
    assert!(s.stats().logs.ingest_fault().is_none(), "{:?}", faults(&s));
    drop(s);

    let s = Storage::open(c, None).unwrap();
    assert_eq!(messages(&s), ["first", "after"]);
}

#[test]
fn write_probe_reports_unwritable_storage() {
    for (fault, reason) in [
        (Injected::NoSpace, FaultReason::NoSpace),
        (Injected::ReadOnly, FaultReason::ReadOnly),
        (Injected::PermissionDenied, FaultReason::PermissionDenied),
    ] {
        let d = tempfile::tempdir().unwrap();
        let s = Storage::open(config(d.path(), Durability::Normal), None).unwrap();
        assert!(s.probe_writable().is_ok());
        std::thread::sleep(Duration::from_millis(1100));

        let guard = inject(d.path(), "probe", fault);
        let f = s.probe_writable().unwrap_err();
        assert_eq!((f.op, f.reason, f.path.as_str()), (FaultOp::Probe, reason, "tmp/.write-probe"));
        drop(guard);
        // Cached briefly, then checked again.
        assert!(s.probe_writable().is_err());
        std::thread::sleep(Duration::from_millis(1100));
        assert!(s.probe_writable().is_ok());
        assert!(!d.path().join("tmp").join(".write-probe").exists());
    }
}

/// A real permission error, not an injected one. Skipped where the user
/// can write anyway (root, or Windows, which has no mode bits).
#[cfg(unix)]
#[test]
fn write_probe_sees_a_real_read_only_directory() {
    use std::os::unix::fs::PermissionsExt;
    let d = tempfile::tempdir().unwrap();
    let s = Storage::open(config(d.path(), Durability::Normal), None).unwrap();
    let tmp = d.path().join("tmp");
    std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o555)).unwrap();
    let restore = || std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755)).unwrap();
    if std::fs::write(tmp.join("can-i-write"), b"x").is_ok() {
        restore();
        eprintln!("skipped: permissions are not enforced for this user");
        return;
    }
    let r = s.probe_writable();
    restore();
    let f = r.unwrap_err();
    assert_eq!(f.reason, FaultReason::PermissionDenied);
    assert!(!f.message.contains(&*d.path().to_string_lossy()), "{}", f.message);
}
