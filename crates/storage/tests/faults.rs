//! Storage write failures (REL-04).
//!
//! Faults are injected at named points (`storage::fault::inject`, feature
//! `fault-injection`) for one temp data directory at a time. Each test
//! checks the acknowledgement contract (a failed write is never reported
//! as stored), the fault diagnostics, and that a restart afterwards brings
//! back exactly the acknowledged data.
//!
//! A process restart does not lose unsynced filesystem metadata the way a
//! power loss can, so the rollback tests also check *that* the WAL was
//! fsynced after a rollback, using a recorder of the fault points reached.

use std::path::Path;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use storage::fault::inject::{Injected, inject, inject_times, record};
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

fn write_uncertain(r: Result<Committed>) -> StorageFault {
    match r {
        Err(StorageError::WriteUncertain(f)) => f,
        other => panic!("expected WriteUncertain, got {other:?}"),
    }
}

fn only_wal_len(dir: &Path) -> u64 {
    let wals: Vec<_> = std::fs::read_dir(dir.join("wal").join("logs"))
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "wal"))
        .collect();
    assert_eq!(wals.len(), 1, "{wals:?}");
    std::fs::metadata(&wals[0]).unwrap().len()
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
        Injected::Stall(_) => unreachable!("a stall does not fail"),
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
fn failed_strict_fsync_is_rolled_back_durably_before_the_error() {
    for fault in [Injected::NoSpace, Injected::PermissionDenied] {
        let d = tempfile::tempdir().unwrap();
        let s = Storage::open(config(d.path(), Durability::Strict), None).unwrap();
        submit_ok(&s, "before");
        let acked_len = only_wal_len(d.path());

        // The commit fsync fails once; the rollback's own fsync works.
        let _fault = inject_times(d.path(), "wal.sync", fault, 1);
        let rec = record(d.path());
        let f = write_failed(try_submit(&s, "not synced"));
        assert_eq!((f.op, f.reason), (FaultOp::WalSync, expected_reason(fault)));
        // Append, failed fsync, then the fsync that makes the cut durable,
        // all before the client hears "not stored".
        assert_eq!(rec.points(), ["wal.append", "wal.sync", "wal.sync"]);
        assert_eq!(only_wal_len(d.path()), acked_len, "records cut from the WAL");
        assert_eq!(messages(&s), ["before"]);

        submit_ok(&s, "after");
        assert!(faults(&s).is_empty());
        drop(s);
        // A client that retried the rejected batch does not get two copies.
        restart_and_check(d.path(), Durability::Strict, &["before", "after"]);
    }
}

#[test]
fn rollback_that_cannot_be_synced_reports_unknown_and_is_synced_later() {
    let d = tempfile::tempdir().unwrap();
    let s = Storage::open(config(d.path(), Durability::Strict), None).unwrap();
    submit_ok(&s, "before");
    let acked_len = only_wal_len(d.path());

    let guard = inject(d.path(), "wal.sync", Injected::NoSpace);
    let rec = record(d.path());
    // The commit fsync and the rollback's fsync both fail: the cut is made
    // but may not survive a power loss, so the outcome is unknown.
    let f = write_uncertain(try_submit(&s, "outcome unknown"));
    assert_eq!((f.op, f.reason), (FaultOp::WalSync, FaultReason::NoSpace));
    assert_eq!(rec.points(), ["wal.append", "wal.sync", "wal.sync"]);
    assert_eq!(only_wal_len(d.path()), acked_len);
    assert_eq!(messages(&s), ["before"]);
    drop(rec);

    // Storage works again. Without any further request, the idle writer
    // fsyncs the WAL, which persists the pending cut, and the fault clears.
    let rec = record(d.path());
    drop(guard);
    wait_for("idle recovery", || faults(&s).is_empty());
    // The first thing reached is the fsync; no record was written (the WAL
    // length is unchanged; the idle probe file lives beside the WAL).
    let points = rec.points();
    assert_eq!(points.first().map(String::as_str), Some("wal.sync"), "{points:?}");
    assert_eq!(only_wal_len(d.path()), acked_len);
    drop(s);
    restart_and_check(d.path(), Durability::Strict, &["before"]);
}

#[test]
fn rollback_is_made_durable_before_any_later_acknowledgement() {
    let d = tempfile::tempdir().unwrap();
    let s = Storage::open(config(d.path(), Durability::Strict), None).unwrap();
    submit_ok(&s, "before");

    // Commit fsync and rollback fsync fail, then storage recovers before the
    // idle check runs. The next acknowledgement must include the fsync that
    // persists the cut, so the cut records can never outlive a later ack.
    let fault = inject_times(d.path(), "wal.sync", Injected::NoSpace, 2);
    write_uncertain(try_submit(&s, "outcome unknown"));
    drop(fault);
    let rec = record(d.path());
    submit_ok(&s, "after");
    let points = rec.points();
    let append = points.iter().position(|p| p == "wal.append").expect("append recorded");
    assert!(points[append..].iter().any(|p| p == "wal.sync"), "ack waited for an fsync: {points:?}");
    drop(s);
    restart_and_check(d.path(), Durability::Strict, &["before", "after"]);
}

#[test]
fn wal_fault_clears_without_new_requests_once_storage_works() {
    let d = tempfile::tempdir().unwrap();
    let s = Storage::open(config(d.path(), Durability::Normal), None).unwrap();
    let guard = inject(d.path(), "wal.append", Injected::NoSpace);
    write_failed(try_submit(&s, "rejected"));
    // Still failing: the idle check writes into the WAL directory too.
    std::thread::sleep(Duration::from_millis(1600));
    assert!(s.stats().logs.ingest_fault().is_some(), "fault stays while writes fail");
    drop(guard);
    // No request arrives, but readiness must not stay degraded forever.
    wait_for("idle recovery", || faults(&s).is_empty());
    assert!(!d.path().join("wal").join("logs").join(".write-probe").exists());
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

    // From now on acknowledgements wait for an fsync, which fails (and so
    // does the fsync of the rollback, hence "unknown").
    let f = write_uncertain(try_submit(&s, "rejected"));
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
fn batches_queued_as_a_failing_seal_blocks_rotation_fail_fast_too() {
    let d = tempfile::tempdir().unwrap();
    let mut c = config(d.path(), Durability::Strict);
    c.logs.segment_max_events = 1;
    let s = Storage::open(c, None).unwrap();
    let guard = inject(d.path(), "segment.write", Injected::NoSpace);
    submit_ok(&s, "first");
    wait_for("seal fault", || faults(&s).iter().any(|f| f.op == FaultOp::SegmentWrite));

    // A burst, not waiting in between: some batches are queued after the
    // writer took its last group and before it marked rotation stalled, so
    // they wait behind a seal that keeps failing. They must be answered
    // like a new submission would be, not after their acknowledgement
    // timeout (the seal is retried every 5 s and fails every time).
    //
    // The writer appends everything queued as one group, so when it wakes
    // late a whole burst can be accepted before it next tries to rotate.
    // That rotation cannot succeed, so a following burst always reaches
    // the stalled writer: repeat until one does.
    let mut accepted = vec!["first".to_string()];
    let (mut failed_fast, mut failed_queued) = (0, 0);
    let mut i = 0;
    for _burst in 0..20 {
        let mut waiting = Vec::new();
        for _ in 0..200 {
            let msg = format!("burst {i}");
            let (tx, rx) = mpsc::sync_channel(1);
            match s.submit(Signal::Logs, vec![log(i, &msg)], Box::new(move |r| drop(tx.send(r)))) {
                Ok(()) => waiting.push((msg, rx)),
                Err(StorageError::WriteFailed(_)) => failed_fast += 1,
                Err(e) => panic!("unexpected error: {e}"),
            }
            i += 1;
        }
        for (msg, rx) in waiting {
            match rx.recv_timeout(Duration::from_secs(2)) {
                Ok(Ok(_)) => accepted.push(msg),
                Ok(Err(StorageError::WriteFailed(f))) => {
                    assert_eq!((f.op, f.reason), (FaultOp::SegmentWrite, FaultReason::NoSpace));
                    failed_queued += 1;
                }
                Ok(Err(e)) => panic!("{msg}: unexpected error: {e}"),
                Err(_) => panic!("{msg} was queued behind the failing seal and never answered"),
            }
        }
        if failed_fast + failed_queued > 0 {
            break;
        }
    }
    assert!(failed_fast + failed_queued > 0, "a burst ran into the stalled writer");
    assert_eq!(s.stats().logs.queue_depth, 0, "failed batches release their queue reservation");
    assert_eq!(messages(&s), accepted);

    drop(s);
    drop(guard);
    let s = Storage::open(config(d.path(), Durability::Strict), None).unwrap();
    assert_eq!(messages(&s), accepted, "exactly the acknowledged batches are back");
}

/// Submit `msg` with an acknowledgement that reports on the returned
/// receiver and then runs `then`. Acknowledgements run on the writer thread
/// right after the commit, before the writer looks at rotation, so `then`
/// acts at a fixed point instead of racing the writer.
fn submit_then(
    s: &Storage,
    msg: &str,
    then: impl FnOnce() + Send + 'static,
) -> Result<mpsc::Receiver<Result<Committed>>> {
    let (tx, rx) = mpsc::sync_channel(1);
    s.submit(
        Signal::Logs,
        vec![log(0, msg)],
        Box::new(move |r| {
            let _ = tx.send(r);
            then();
        }),
    )?;
    Ok(rx)
}

/// Queue `msg` without waiting; the receiver gets the acknowledgement.
fn submit_queued(s: &Storage, msg: &str) -> Result<mpsc::Receiver<Result<Committed>>> {
    submit_then(s, msg, || {})
}

fn rotate_queued(s: &Storage) -> mpsc::Receiver<Result<()>> {
    let (tx, rx) = mpsc::sync_channel(1);
    s.stream(Signal::Logs).rotate(Box::new(move |r| {
        let _ = tx.send(r);
    }));
    rx
}

/// Within a few seconds, far below any `ack_timeout`; the seal is retried
/// only every 5 s, so an answer that waited for it would miss this.
const PROMPTLY: Duration = Duration::from_secs(2);

/// REL-17: a batch submitted right after the acknowledgement of the batch
/// that needs a rotation, before the writer finds that the previous seal is
/// still failing, passes the fail-fast check in `submit` and is queued. It
/// must still fail fast with the seal's fault instead of waiting for it.
#[test]
fn batch_queued_just_before_rotation_stalls_fails_fast() {
    for durability in [Durability::Normal, Durability::Strict] {
        for (point, fault) in [("segment.write", Injected::NoSpace), ("segment.sync", Injected::ReadOnly)] {
            let d = tempfile::tempdir().unwrap();
            let mut c = config(d.path(), durability);
            // Every committed batch asks for a rotation.
            c.logs.segment_max_events = 1;
            let s = std::sync::Arc::new(Storage::open(c, None).unwrap());
            let guard = inject(d.path(), point, fault);
            // Frozen for sealing; the seal fails and keeps failing.
            submit_ok(&s, "first");
            wait_for("seal fault", || faults(&s).iter().any(|f| f.op == FaultOp::SegmentWrite));

            // "second" is stored, then needs a rotation, which has to wait
            // for the failing seal. "third" is submitted in between.
            let (third_tx, third_rx) = mpsc::sync_channel(1);
            let storage = s.clone();
            let second = submit_then(&s, "second", move || {
                let _ = third_tx.send(submit_queued(&storage, "third"));
            })
            .unwrap();
            assert!(second.recv_timeout(PROMPTLY).unwrap().is_ok(), "{durability:?} {point}: second is stored");
            let third = third_rx
                .recv_timeout(PROMPTLY)
                .unwrap()
                .expect("third passes the check in submit: rotation is not stalled yet");
            let f = match third.recv_timeout(PROMPTLY) {
                Ok(r) => write_failed(r),
                Err(_) => panic!("{durability:?} {point}: third waited for the failing seal"),
            };
            assert_eq!((f.op, f.reason), (FaultOp::SegmentWrite, expected_reason(fault)), "{durability:?} {point}");
            assert_eq!(s.stats().logs.queue_depth, 0, "the failed batch releases its queue reservation");
            assert_eq!(messages(&s), ["first", "second"]);

            drop(s);
            drop(guard);
            let s = restart_and_check(d.path(), durability, &["first", "second"]);
            assert!(faults(&s).is_empty(), "{:?}", faults(&s));
        }
    }
}

/// REL-17: the writer takes everything queued as one group. Appends behind
/// a `Rotate` in that group must not wait for the seal the rotation waits
/// for. The rotation itself still waits, and finishes once sealing works.
#[test]
fn batches_behind_a_stalled_rotation_fail_fast_and_recover() {
    for durability in [Durability::Normal, Durability::Strict] {
        let d = tempfile::tempdir().unwrap();
        let s = std::sync::Arc::new(Storage::open(config(d.path(), durability), None).unwrap());
        let guard = inject(d.path(), "segment.write", Injected::NoSpace);
        submit_ok(&s, "first");
        let first_rotation = rotate_queued(&s);
        wait_for("seal fault", || faults(&s).iter().any(|f| f.op == FaultOp::SegmentWrite));

        // From "second"'s acknowledgement, queue a rotation and a batch:
        // the writer next takes both as one group, rotation first.
        let (queued_tx, queued_rx) = mpsc::sync_channel(1);
        let storage = s.clone();
        let second = submit_then(&s, "second", move || {
            let rotation = rotate_queued(&storage);
            let _ = queued_tx.send((rotation, submit_queued(&storage, "third")));
        })
        .unwrap();
        assert!(second.recv_timeout(PROMPTLY).unwrap().is_ok(), "{durability:?}: second is stored");
        let (second_rotation, third) = queued_rx.recv_timeout(PROMPTLY).unwrap();
        let third = third.expect("third passes the check in submit: rotation is not stalled yet");
        let f = match third.recv_timeout(PROMPTLY) {
            Ok(r) => write_failed(r),
            Err(_) => panic!("{durability:?}: third waited behind the stalled rotation"),
        };
        assert_eq!((f.op, f.reason), (FaultOp::SegmentWrite, FaultReason::NoSpace));
        // While the seal fails, new batches fail fast and the rotations wait.
        write_failed(try_submit(&s, "rejected"));
        assert!(first_rotation.try_recv().is_err() && second_rotation.try_recv().is_err());
        assert_eq!(s.stats().logs.queue_depth, 0);

        // Sealing works again (retried every 5 s): both rotations finish,
        // the faults clear and ingest resumes.
        drop(guard);
        let rotated = Duration::from_secs(15);
        assert!(first_rotation.recv_timeout(rotated).unwrap().is_ok(), "{durability:?}");
        assert!(second_rotation.recv_timeout(rotated).unwrap().is_ok(), "{durability:?}");
        submit_ok(&s, "after");
        assert!(faults(&s).is_empty(), "{:?}", faults(&s));
        assert_eq!(messages(&s), ["first", "second", "after"]);
        assert_eq!(s.snapshot(Signal::Logs).segments.len(), 2, "both rotations were sealed");

        drop(s);
        restart_and_check(d.path(), durability, &["first", "second", "after"]);
    }
}

/// REL-17: rotations queued while the writer waits for a failing seal are
/// kept and answered, and shutdown is not held up by the seal.
#[test]
fn shutdown_during_a_stalled_rotation_is_bounded_and_answers_rotations() {
    let d = tempfile::tempdir().unwrap();
    let mut c = config(d.path(), Durability::Strict);
    c.logs.segment_max_events = 1;
    let s = Storage::open(c, None).unwrap();
    let guard = inject(d.path(), "segment.write", Injected::NoSpace);
    submit_ok(&s, "first");
    wait_for("seal fault", || faults(&s).iter().any(|f| f.op == FaultOp::SegmentWrite));
    // The next batch needs a rotation, which waits for the failing seal.
    let mut accepted = vec!["first".to_string()];
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let msg = format!("accepted {}", accepted.len());
        match try_submit(&s, &msg) {
            Ok(_) => accepted.push(msg),
            Err(StorageError::WriteFailed(_)) => break,
            Err(e) => panic!("unexpected error: {e}"),
        }
        assert!(Instant::now() < deadline, "writer never stalled");
    }
    let rotation = rotate_queued(&s);

    let started = Instant::now();
    drop(s);
    assert!(started.elapsed() < Duration::from_secs(5), "shutdown took {:?}", started.elapsed());
    match rotation.recv_timeout(PROMPTLY) {
        Ok(Err(StorageError::Unavailable(why))) => assert_eq!(why, "shutting down"),
        other => panic!("expected the rotation to be refused by shutdown, got {other:?}"),
    }

    drop(guard);
    let accepted: Vec<&str> = accepted.iter().map(String::as_str).collect();
    restart_and_check(d.path(), Durability::Strict, &accepted);
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
