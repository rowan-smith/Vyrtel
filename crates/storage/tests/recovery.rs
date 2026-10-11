//! Crash-recovery tests.
//!
//! Committed telemetry must survive restarts; incomplete telemetry must not
//! appear as valid telemetry. Each test simulates a crash by manipulating
//! files the way an interrupted process would leave them.

use std::path::Path;
use std::sync::mpsc;
use std::time::Duration;

use storage::segment::{Segment, SegmentWriteOptions, segment_file_name, write_segment};
use storage::wal::{self, wal_file_name};
use storage::*;

use telemetry::*;

fn config(dir: &Path) -> StorageConfig {
    let mut c = StorageConfig::new(dir);
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
        environment: Some("production".into()),
        trace_id: None,
        span_id: None,
        resource: Fields::new(),
        attributes: [("i".to_string(), Value::Int(i))].into_iter().collect(),
        payload: TelemetryPayload::Log(LogEvent {
            level: Level::Information,
            message: msg.into(),
            message_template: None,
            exception: None,
        }),
    }
}

fn submit(s: &Storage, events: Vec<TelemetryEvent>) -> Committed {
    let (tx, rx) = mpsc::sync_channel(1);
    s.submit(
        Signal::Logs,
        events,
        Box::new(move |r| {
            tx.send(r).unwrap();
        }),
    )
    .unwrap();
    rx.recv_timeout(Duration::from_secs(10)).unwrap().unwrap()
}

/// All visible log events, sorted by id.
fn all_events(s: &Storage) -> Vec<TelemetryEvent> {
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

fn messages(s: &Storage) -> Vec<String> {
    all_events(s).iter().map(|e| e.message().to_string()).collect()
}

fn wal_dir(dir: &Path) -> std::path::PathBuf {
    dir.join("wal").join("logs")
}

fn seg_dir(dir: &Path) -> std::path::PathBuf {
    dir.join("segments").join("logs")
}

fn only_wal(dir: &Path) -> std::path::PathBuf {
    let mut w: Vec<_> = std::fs::read_dir(wal_dir(dir))
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "wal"))
        .collect();
    assert_eq!(w.len(), 1, "expected exactly one WAL: {w:?}");
    w.pop().unwrap()
}

#[test]
fn empty_wal_starts_clean() {
    let d = tempfile::tempdir().unwrap();
    {
        let s = Storage::open(config(d.path()), None).unwrap();
        assert!(all_events(&s).is_empty());
    }
    let s = Storage::open(config(d.path()), None).unwrap();
    assert!(all_events(&s).is_empty());
    assert_eq!(s.recovery_report(Signal::Logs).wal_files_replayed, 1);
}

#[test]
fn valid_wal_is_replayed_after_restart() {
    let d = tempfile::tempdir().unwrap();
    {
        let s = Storage::open(config(d.path()), None).unwrap();
        submit(&s, vec![log(1, "a"), log(2, "b")]);
        submit(&s, vec![log(3, "c")]);
    }
    let s = Storage::open(config(d.path()), None).unwrap();
    assert_eq!(messages(&s), ["a", "b", "c"]);
    // New events continue the id sequence without collisions.
    let c = submit(&s, vec![log(4, "d")]);
    assert_eq!(c.first_id, EventId(4));
}

#[test]
fn truncated_final_record_is_ignored() {
    let d = tempfile::tempdir().unwrap();
    {
        let s = Storage::open(config(d.path()), None).unwrap();
        submit(&s, vec![log(1, "committed")]);
        submit(&s, vec![log(2, "torn")]);
    }
    let path = only_wal(d.path());
    let len = std::fs::metadata(&path).unwrap().len();
    let f = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
    f.set_len(len - 3).unwrap();
    drop(f);

    let s = Storage::open(config(d.path()), None).unwrap();
    assert_eq!(messages(&s), ["committed"]);
    let r = s.recovery_report(Signal::Logs);
    assert!(r.wal_bytes_discarded > 0);
    // Appending after recovery must produce a clean WAL.
    submit(&s, vec![log(3, "after")]);
    drop(s);
    let s = Storage::open(config(d.path()), None).unwrap();
    assert_eq!(messages(&s), ["committed", "after"]);
    assert_eq!(s.recovery_report(Signal::Logs).wal_bytes_discarded, 0);
}

#[test]
fn invalid_checksum_stops_replay() {
    let d = tempfile::tempdir().unwrap();
    {
        let s = Storage::open(config(d.path()), None).unwrap();
        submit(&s, vec![log(1, "good")]);
        submit(&s, vec![log(2, "corrupted")]);
        submit(&s, vec![log(3, "after-corruption")]);
    }
    let path = only_wal(d.path());
    let mut bytes = std::fs::read(&path).unwrap();
    // Find the second record and flip a payload byte.
    let mut offset = wal::HEADER_LEN as usize;
    let first_len = u32::from_le_bytes(bytes[offset + 1..offset + 5].try_into().unwrap()) as usize;
    offset += 9 + first_len;
    bytes[offset + 10] ^= 0x55;
    std::fs::write(&path, &bytes).unwrap();

    let s = Storage::open(config(d.path()), None).unwrap();
    // Never fabricate or partially interpret corrupt records, and do not
    // resynchronise past them either.
    assert_eq!(messages(&s), ["good"]);
    let q = d.path().join("quarantine").join("logs");
    assert!(std::fs::read_dir(q).unwrap().count() >= 1, "tail preserved for inspection");
}

#[test]
fn rotation_commits_segment_and_survives_restart() {
    let d = tempfile::tempdir().unwrap();
    {
        let s = Storage::open(config(d.path()), None).unwrap();
        submit(&s, (0..100).map(|i| log(i, &format!("m{i}"))).collect());
        s.rotate_blocking(Signal::Logs).unwrap();
        submit(&s, vec![log(200, "active")]);
        let snap = s.snapshot(Signal::Logs);
        assert_eq!(snap.segments.len(), 1);
        assert_eq!(snap.batches.len(), 1);
    }
    let s = Storage::open(config(d.path()), None).unwrap();
    let msgs = messages(&s);
    assert_eq!(msgs.len(), 101);
    assert_eq!(msgs.last().unwrap(), "active");
    assert_eq!(s.snapshot(Signal::Logs).segments.len(), 1);
}

#[test]
fn restart_after_segment_commit_before_wal_delete() {
    // Simulate: segment renamed into place, crash before WAL deletion.
    let d = tempfile::tempdir().unwrap();
    {
        let s = Storage::open(config(d.path()), None).unwrap();
        submit(&s, vec![log(1, "x"), log(2, "y")]);
    }
    let wal_path = only_wal(d.path());
    let id = wal::parse_wal_file_name(wal_path.file_name().unwrap().to_str().unwrap()).unwrap();
    // Build the segment that sealing would have produced.
    let mut events = Vec::new();
    wal::replay(&wal_path, |p| {
        events.extend(storage::codec::decode_batch(p).unwrap());
        Ok(())
    })
    .unwrap();
    let refs: Vec<&TelemetryEvent> = events.iter().collect();
    std::fs::create_dir_all(d.path().join("tmp")).unwrap();
    write_segment(
        &seg_dir(d.path()),
        &d.path().join("tmp"),
        id,
        Signal::Logs,
        &refs,
        vec![],
        &SegmentWriteOptions::default(),
    )
    .unwrap();

    let s = Storage::open(config(d.path()), None).unwrap();
    assert_eq!(messages(&s), ["x", "y"], "no duplicates from WAL + segment");
    assert_eq!(s.recovery_report(Signal::Logs).stale_wals_removed, 1);
    assert!(!wal_path.exists());
}

#[test]
fn temporary_segment_file_is_discarded() {
    let d = tempfile::tempdir().unwrap();
    {
        let s = Storage::open(config(d.path()), None).unwrap();
        submit(&s, vec![log(1, "kept")]);
    }
    // A half-written segment from an interrupted seal.
    let tmp = d.path().join("tmp");
    std::fs::write(tmp.join("logs.000000000099.seg.tmp"), b"partial garbage").unwrap();
    let s = Storage::open(config(d.path()), None).unwrap();
    assert_eq!(messages(&s), ["kept"]);
    assert_eq!(std::fs::read_dir(&tmp).unwrap().count(), 0);
}

#[test]
fn partial_segment_in_segment_dir_is_quarantined_and_rebuilt_from_wal() {
    // A segment that somehow exists but is damaged while its WAL survives:
    // the WAL is authoritative and the events come back exactly once.
    let d = tempfile::tempdir().unwrap();
    {
        let s = Storage::open(config(d.path()), None).unwrap();
        submit(&s, vec![log(1, "one"), log(2, "two")]);
    }
    let wal_path = only_wal(d.path());
    let id = wal::parse_wal_file_name(wal_path.file_name().unwrap().to_str().unwrap()).unwrap();
    std::fs::write(seg_dir(d.path()).join(segment_file_name(id)), b"VYRTSEG1 truncated").unwrap();

    let s = Storage::open(config(d.path()), None).unwrap();
    assert_eq!(messages(&s), ["one", "two"]);
    assert_eq!(s.recovery_report(Signal::Logs).segments_quarantined, 1);
}

#[test]
fn older_wals_are_sealed_on_startup() {
    let d = tempfile::tempdir().unwrap();
    {
        let s = Storage::open(config(d.path()), None).unwrap();
        submit(&s, vec![log(1, "first")]);
    }
    // Pretend a rotation happened (new WAL created) but sealing never ran.
    let first = only_wal(d.path());
    let id = wal::parse_wal_file_name(first.file_name().unwrap().to_str().unwrap()).unwrap();
    {
        let mut w = wal::WalWriter::create(
            &wal_dir(d.path()),
            &d.path().join("tmp"),
            wal::WalHeader { signal: Signal::Logs, wal_id: id + 1 },
        )
        .unwrap();
        let mut e = log(2, "second");
        e.id = EventId(2);
        w.append(&storage::codec::encode_batch(&[e])).unwrap();
        w.sync().unwrap();
    }
    let s = Storage::open(config(d.path()), None).unwrap();
    assert_eq!(messages(&s), ["first", "second"]);
    assert_eq!(s.recovery_report(Signal::Logs).segments_sealed, 1);
    let snap = s.snapshot(Signal::Logs);
    assert_eq!(snap.segments.len(), 1);
    assert_eq!(snap.batches.len(), 1);
    assert!(d.path().join("wal/logs").join(wal_file_name(id + 1)).exists());
}

#[test]
fn restart_during_compaction_does_not_duplicate() {
    let d = tempfile::tempdir().unwrap();
    {
        let s = Storage::open(config(d.path()), None).unwrap();
        for i in 0..4 {
            submit(&s, vec![log(i, &format!("e{i}"))]);
            s.rotate_blocking(Signal::Logs).unwrap();
        }
    }
    // Simulate compaction committing its output but crashing before the
    // inputs were deleted.
    let segs: Vec<Segment> = {
        let mut v: Vec<_> = std::fs::read_dir(seg_dir(d.path()))
            .unwrap()
            .flatten()
            .map(|e| Segment::open(&e.path()).unwrap())
            .collect();
        v.sort_by_key(|s| s.summary.id);
        v
    };
    assert_eq!(segs.len(), 4);
    let mut events: Vec<TelemetryEvent> = segs.iter().flat_map(|s| s.read_all().unwrap()).collect();
    events.sort_by_key(|e| (e.timestamp, e.id));
    let refs: Vec<&TelemetryEvent> = events.iter().collect();
    let replaces: Vec<u64> = segs.iter().map(|s| s.summary.id).collect();
    drop(segs);
    write_segment(
        &seg_dir(d.path()),
        &d.path().join("tmp"),
        1000,
        Signal::Logs,
        &refs,
        replaces,
        &SegmentWriteOptions::default(),
    )
    .unwrap();

    let s = Storage::open(config(d.path()), None).unwrap();
    assert_eq!(messages(&s), ["e0", "e1", "e2", "e3"]);
    assert_eq!(s.recovery_report(Signal::Logs).replaced_segments_removed, 4);
    assert_eq!(s.snapshot(Signal::Logs).segments.len(), 1);
    // New ids/files never collide with the compaction output.
    submit(&s, vec![log(9, "new")]);
    s.rotate_blocking(Signal::Logs).unwrap();
    assert_eq!(s.snapshot(Signal::Logs).segments.len(), 2);
}

#[test]
fn compaction_merges_small_segments() {
    let d = tempfile::tempdir().unwrap();
    let mut c = config(d.path());
    c.compaction.enabled = true;
    let s = Storage::open(c, None).unwrap();
    for i in 0..5 {
        submit(&s, vec![log(i, &format!("e{i}"))]);
        s.rotate_blocking(Signal::Logs).unwrap();
    }
    assert_eq!(s.snapshot(Signal::Logs).segments.len(), 5);
    s.maintain_blocking(Signal::Logs).unwrap();
    assert_eq!(s.snapshot(Signal::Logs).segments.len(), 1);
    assert_eq!(messages(&s), ["e0", "e1", "e2", "e3", "e4"]);
    drop(s);
    let s = Storage::open(config(d.path()), None).unwrap();
    assert_eq!(messages(&s), ["e0", "e1", "e2", "e3", "e4"]);
}

#[test]
fn retention_deletes_whole_expired_segments_and_survives_restart() {
    let d = tempfile::tempdir().unwrap();
    let mut c = config(d.path());
    c.logs.retention = Some(Duration::from_secs(3600));
    let now = Timestamp::now().nanos();
    let mut old = log(0, "old");
    old.timestamp = Timestamp(now - 2 * 3600 * 1_000_000_000);
    let mut fresh = log(0, "fresh");
    fresh.timestamp = Timestamp(now);
    {
        let s = Storage::open(c.clone(), None).unwrap();
        submit(&s, vec![old]);
        s.rotate_blocking(Signal::Logs).unwrap();
        submit(&s, vec![fresh]);
        s.rotate_blocking(Signal::Logs).unwrap();
        assert_eq!(s.snapshot(Signal::Logs).segments.len(), 2);
        s.maintain_blocking(Signal::Logs).unwrap();
        assert_eq!(messages(&s), ["fresh"]);
    }
    let s = Storage::open(c, None).unwrap();
    assert_eq!(messages(&s), ["fresh"]);
    assert_eq!(std::fs::read_dir(seg_dir(d.path())).unwrap().count(), 1);
}

#[test]
fn queue_full_applies_backpressure() {
    let d = tempfile::tempdir().unwrap();
    let mut c = config(d.path());
    c.logs.queue_events = 10;
    let s = Storage::open(c, None).unwrap();
    assert!(matches!(
        s.submit(Signal::Logs, (0..11).map(|i| log(i, "x")).collect(), Box::new(|_| {})),
        Err(StorageError::BatchTooLarge(11))
    ));
    // Fill the queue faster than the writer can drain it; at some point a
    // submit must be rejected rather than buffered without bound.
    let mut rejected = false;
    for _ in 0..10_000 {
        match s.submit(Signal::Logs, (0..10).map(|i| log(i, "x")).collect(), Box::new(|_| {})) {
            Err(StorageError::QueueFull) => {
                rejected = true;
                break;
            }
            Err(e) => panic!("{e}"),
            Ok(()) => {}
        }
    }
    assert!(rejected);
}

#[test]
fn data_dir_is_locked() {
    let d = tempfile::tempdir().unwrap();
    let _s = Storage::open(config(d.path()), None).unwrap();
    assert!(matches!(Storage::open(config(d.path()), None), Err(StorageError::Unavailable(_))));
}

#[test]
fn strict_durability_round_trip() {
    let d = tempfile::tempdir().unwrap();
    let mut c = config(d.path());
    c.durability = Durability::Strict;
    {
        let s = Storage::open(c.clone(), None).unwrap();
        submit(&s, vec![log(1, "durable")]);
    }
    let s = Storage::open(c, None).unwrap();
    assert_eq!(messages(&s), ["durable"]);
}

#[test]
fn commit_hook_sees_batches() {
    let d = tempfile::tempdir().unwrap();
    let (tx, rx) = mpsc::channel();
    let tx = std::sync::Mutex::new(tx);
    let hook: CommitHook = std::sync::Arc::new(move |sig, b| {
        tx.lock().unwrap().send((sig, b.events.len())).unwrap();
    });
    let s = Storage::open(config(d.path()), Some(hook)).unwrap();
    submit(&s, vec![log(1, "a"), log(2, "b")]);
    assert_eq!(rx.recv_timeout(Duration::from_secs(5)).unwrap(), (Signal::Logs, 2));
}

// ---- REL-01: quarantine preserves evidence and names the signal ----------

fn quarantine_dir(dir: &Path, signal: Signal) -> std::path::PathBuf {
    dir.join("quarantine").join(signal.as_str())
}

#[test]
fn torn_final_record_is_preserved_byte_for_byte() {
    let d = tempfile::tempdir().unwrap();
    {
        let s = Storage::open(config(d.path()), None).unwrap();
        submit(&s, vec![log(1, "committed")]);
        submit(&s, vec![log(2, "torn")]);
    }
    let path = only_wal(d.path());
    let original = std::fs::read(&path).unwrap();
    let cut = original.len() - 3;
    let f = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
    f.set_len(cut as u64).unwrap();
    drop(f);
    // Where the torn record starts: header + first record.
    let first_len = u32::from_le_bytes(original[25..29].try_into().unwrap()) as usize;
    let valid = wal::HEADER_LEN as usize + 9 + first_len;

    let s = Storage::open(config(d.path()), None).unwrap();
    assert_eq!(messages(&s), ["committed"]);
    let r = s.recovery_report(Signal::Logs).clone();
    assert_eq!(r.quarantined.len(), 1);
    let q = &r.quarantined[0];
    assert_eq!(q.signal, Signal::Logs);
    assert_eq!(q.kind, QuarantineKind::WalTail);
    assert_eq!(q.source, path.file_name().unwrap().to_str().unwrap());
    assert!(q.reason.contains(&format!("incomplete record at offset {valid}")), "{}", q.reason);
    assert!(q.path.starts_with(quarantine_dir(d.path(), Signal::Logs)));
    assert_eq!(std::fs::read(&q.path).unwrap(), &original[valid..cut], "evidence is the exact discarded bytes");
    assert_eq!(q.bytes, (cut - valid) as u64);
    // Other signals are untouched and say so.
    assert!(s.recovery_report(Signal::Traces).quarantined.is_empty());
    assert_eq!(s.stats().logs.recovery.quarantined, r.quarantined);
    drop(s);

    // Restarting again neither re-quarantines nor duplicates, and the
    // evidence from the first recovery is still there.
    let s = Storage::open(config(d.path()), None).unwrap();
    assert_eq!(messages(&s), ["committed"]);
    assert!(s.recovery_report(Signal::Logs).quarantined.is_empty());
    assert!(q.path.exists());
}

#[test]
fn corrupt_final_record_names_the_reason() {
    let d = tempfile::tempdir().unwrap();
    {
        let s = Storage::open(config(d.path()), None).unwrap();
        submit(&s, vec![log(1, "good")]);
        submit(&s, vec![log(2, "flipped")]);
    }
    let path = only_wal(d.path());
    let mut bytes = std::fs::read(&path).unwrap();
    let n = bytes.len();
    bytes[n - 6] ^= 0x40; // inside the last record's payload
    std::fs::write(&path, &bytes).unwrap();

    let s = Storage::open(config(d.path()), None).unwrap();
    assert_eq!(messages(&s), ["good"]);
    let q = &s.recovery_report(Signal::Logs).quarantined[0];
    assert_eq!(q.kind, QuarantineKind::WalTail);
    assert!(q.reason.contains("checksum mismatch"), "{}", q.reason);
    let tail = std::fs::read(&q.path).unwrap();
    assert!(bytes.ends_with(&tail), "the corrupt record is preserved as found");
}

#[test]
fn damaged_segment_is_quarantined_whole_under_its_signal() {
    let d = tempfile::tempdir().unwrap();
    drop(Storage::open(config(d.path()), None).unwrap());
    // A traces segment with no WAL behind it: its events cannot be rebuilt,
    // so the evidence must be kept and the signal named.
    let garbage = b"VYRTSEG1 this is not a complete segment file at all".to_vec();
    let seg = d.path().join("segments").join("traces").join(segment_file_name(7));
    std::fs::write(&seg, &garbage).unwrap();

    let s = Storage::open(config(d.path()), None).unwrap();
    let r = s.recovery_report(Signal::Traces);
    assert_eq!(r.segments_quarantined, 1);
    let q = &r.quarantined[0];
    assert_eq!((q.signal, q.kind), (Signal::Traces, QuarantineKind::Segment));
    assert_eq!(q.source, segment_file_name(7));
    assert!(q.path.starts_with(quarantine_dir(d.path(), Signal::Traces)));
    assert_eq!(std::fs::read(&q.path).unwrap(), garbage);
    assert!(!seg.exists());
    // Reasons are shown over the API, so they never contain host paths.
    assert!(!q.reason.contains(d.path().to_str().unwrap()), "{}", q.reason);
    assert!(s.recovery_report(Signal::Logs).quarantined.is_empty());
}

#[test]
fn wal_with_unreadable_header_is_quarantined_whole() {
    let d = tempfile::tempdir().unwrap();
    {
        let s = Storage::open(config(d.path()), None).unwrap();
        submit(&s, vec![log(1, "kept")]);
    }
    let first = only_wal(d.path());
    let id = wal::parse_wal_file_name(first.file_name().unwrap().to_str().unwrap()).unwrap();
    // An older WAL whose header was destroyed.
    let damaged = wal_dir(d.path()).join(wal_file_name(id - 1));
    std::fs::write(&damaged, b"definitely not a WAL header").unwrap();

    let s = Storage::open(config(d.path()), None).unwrap();
    assert_eq!(messages(&s), ["kept"]);
    let r = s.recovery_report(Signal::Logs);
    assert_eq!(r.wal_files_quarantined, 1);
    let q = &r.quarantined[0];
    assert_eq!(q.kind, QuarantineKind::Wal);
    assert_eq!(std::fs::read(&q.path).unwrap(), b"definitely not a WAL header");
}

#[test]
fn wal_header_for_another_signal_is_quarantined() {
    let d = tempfile::tempdir().unwrap();
    drop(Storage::open(config(d.path()), None).unwrap());
    // A metrics WAL dropped into the logs directory (e.g. a botched restore).
    let stray = wal_dir(d.path()).join(wal_file_name(0));
    {
        let w = wal::WalWriter::create(
            &d.path().join("wal").join("metrics"),
            &d.path().join("tmp"),
            wal::WalHeader { signal: Signal::Metrics, wal_id: 0 },
        )
        .unwrap();
        std::fs::copy(w.path(), &stray).unwrap();
    }
    let s = Storage::open(config(d.path()), None).unwrap();
    let q = &s.recovery_report(Signal::Logs).quarantined[0];
    assert_eq!((q.signal, q.kind), (Signal::Logs, QuarantineKind::Wal));
    assert!(q.reason.contains("metrics"), "{}", q.reason);
}

// ---- REL-01: unreadable is not damaged ----------------------------------

/// Keeps a file unreadable to Vyrtel, without touching its bytes, until
/// dropped: mode 000 on Unix, an exclusive share lock on Windows.
struct Unreadable {
    #[cfg(unix)]
    path: std::path::PathBuf,
    #[cfg(windows)]
    _lock: std::fs::File,
}

impl Unreadable {
    /// `None` when the platform cannot deny the read (e.g. tests run as root).
    fn new(path: &Path) -> Option<Unreadable> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o000)).unwrap();
            let guard = Unreadable { path: path.to_path_buf() };
            if std::fs::File::open(path).is_ok() {
                eprintln!("skipping: running with privileges that bypass file permissions");
                return None;
            }
            Some(guard)
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            let lock = std::fs::OpenOptions::new().read(true).share_mode(0).open(path).unwrap();
            assert!(std::fs::File::open(path).is_err(), "share lock must deny reads");
            Some(Unreadable { _lock: lock })
        }
    }
}

#[cfg(unix)]
impl Drop for Unreadable {
    fn drop(&mut self) {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&self.path, std::fs::Permissions::from_mode(0o644));
    }
}

/// Startup must fail with a non-damage I/O error, leave `path` byte-for-byte
/// in place and quarantine nothing; once readable again, recovery succeeds.
fn assert_unreadable_file_stops_startup(dir: &Path, path: &Path) {
    let before = std::fs::read(path).unwrap();
    {
        let Some(_guard) = Unreadable::new(path) else { return };
        match Storage::open(config(dir), None) {
            Err(StorageError::Io { path: failed, source }) => {
                assert_eq!(failed, path, "the error names the unreadable file");
                assert_ne!(source.kind(), std::io::ErrorKind::UnexpectedEof);
            }
            Err(e) => panic!("expected an I/O error, got {e}"),
            Ok(_) => panic!("startup must not continue past an unreadable file"),
        }
    }
    assert_eq!(std::fs::read(path).unwrap(), before, "the original file is untouched");
    let q = dir.join("quarantine");
    assert!(
        !q.exists()
            || std::fs::read_dir(&q).unwrap().flatten().all(|s| std::fs::read_dir(s.path()).unwrap().count() == 0),
        "nothing was quarantined"
    );
}

#[test]
fn unreadable_wal_stops_startup_and_keeps_the_wal() {
    let d = tempfile::tempdir().unwrap();
    {
        let mut c = config(d.path());
        c.durability = Durability::Strict;
        let s = Storage::open(c, None).unwrap();
        submit(&s, vec![log(1, "acknowledged")]);
    }
    let wal = only_wal(d.path());
    assert_unreadable_file_stops_startup(d.path(), &wal);
    // The acknowledged event was hidden from nobody: it is back once readable.
    let s = Storage::open(config(d.path()), None).unwrap();
    assert_eq!(messages(&s), ["acknowledged"]);
    assert!(s.recovery_report(Signal::Logs).quarantined.is_empty());
}

#[test]
fn unreadable_segment_stops_startup_and_keeps_the_segment() {
    let d = tempfile::tempdir().unwrap();
    {
        let s = Storage::open(config(d.path()), None).unwrap();
        submit(&s, vec![log(1, "sealed")]);
        s.rotate_blocking(Signal::Logs).unwrap();
    }
    let seg = std::fs::read_dir(seg_dir(d.path())).unwrap().flatten().map(|e| e.path()).next().unwrap();
    assert_unreadable_file_stops_startup(d.path(), &seg);
    let s = Storage::open(config(d.path()), None).unwrap();
    assert_eq!(messages(&s), ["sealed"]);
    assert_eq!(s.recovery_report(Signal::Logs).segments_quarantined, 0);
}

// ---- REL-06: a newer format is not damage --------------------------------

/// Every file under `dir` (except the lock file) with its bytes.
fn snapshot(dir: &Path) -> std::collections::BTreeMap<std::path::PathBuf, Vec<u8>> {
    fn walk(root: &Path, d: &Path, out: &mut std::collections::BTreeMap<std::path::PathBuf, Vec<u8>>) {
        for e in std::fs::read_dir(d).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(root, &p, out);
            } else if p.file_name().is_some_and(|n| n != ".lock") {
                out.insert(p.strip_prefix(root).unwrap().to_path_buf(), std::fs::read(&p).unwrap());
            }
        }
    }
    let mut out = std::collections::BTreeMap::new();
    walk(dir, dir, &mut out);
    out
}

/// Startup must fail with `UnsupportedFormat` naming `path` before any file
/// in the data directory changes: same files, same bytes, nothing
/// quarantined, no WAL truncated or sealed.
fn assert_newer_format_stops_startup(dir: &Path, path: &Path, what: &str, found: u16) {
    let before = snapshot(dir);
    match Storage::open(config(dir), None) {
        Err(StorageError::UnsupportedFormat { path: p, what: w, found: f, supported }) => {
            assert_eq!((p.as_path(), w, f, supported), (path, what, found, 1));
            let text = StorageError::UnsupportedFormat { path: p, what: w, found: f, supported }.to_string();
            assert!(text.ends_with(&format!("uses {what} version {found}, but this Vyrtel reads only version 1")));
        }
        Err(e) => panic!("expected UnsupportedFormat, got {e}"),
        Ok(_) => panic!("startup must not continue past a file in a newer format"),
    }
    assert_eq!(snapshot(dir), before, "nothing in the data directory changed");
}

/// Rewrite a segment's format version as a newer writer would: in the
/// header and, with `footer_too`, in the footer with its checksum redone.
fn set_segment_format(path: &Path, v: u16, footer_too: bool) {
    let mut b = std::fs::read(path).unwrap();
    b[8..10].copy_from_slice(&v.to_le_bytes());
    if footer_too {
        let f = b.len() - storage::segment::FOOTER_LEN;
        b[f + 40..f + 42].copy_from_slice(&v.to_le_bytes());
        let crc = crc32fast::hash(&b[f..f + 44]);
        b[f + 44..f + 48].copy_from_slice(&crc.to_le_bytes());
    }
    std::fs::write(path, b).unwrap();
}

fn sealed_segment(dir: &Path) -> std::path::PathBuf {
    {
        let s = Storage::open(config(dir), None).unwrap();
        submit(&s, vec![log(1, "sealed")]);
        s.rotate_blocking(Signal::Logs).unwrap();
    }
    std::fs::read_dir(seg_dir(dir)).unwrap().flatten().map(|e| e.path()).next().unwrap()
}

#[test]
fn segment_in_a_newer_format_stops_startup_and_is_kept() {
    let d = tempfile::tempdir().unwrap();
    let seg = sealed_segment(d.path());
    let original = std::fs::read(&seg).unwrap();
    set_segment_format(&seg, 2, true);

    // The read-only pre-start check finds it as well.
    assert!(matches!(check_formats(d.path()), Err(StorageError::UnsupportedFormat { found: 2, .. })));
    assert_newer_format_stops_startup(d.path(), &seg, "segment format", 2);

    // Given a build that reads it (here: the original bytes), nothing was lost.
    std::fs::write(&seg, original).unwrap();
    check_formats(d.path()).unwrap();
    let s = Storage::open(config(d.path()), None).unwrap();
    assert_eq!(messages(&s), ["sealed"]);
    assert!(s.recovery_report(Signal::Logs).quarantined.is_empty());
}

#[test]
fn segment_header_version_contradicting_its_footer_is_damage() {
    let d = tempfile::tempdir().unwrap();
    let seg = sealed_segment(d.path());
    // Only the unchecksummed header changed; the footer proves version 1.
    set_segment_format(&seg, 2, false);
    check_formats(d.path()).unwrap();
    let s = Storage::open(config(d.path()), None).unwrap();
    let q = &s.recovery_report(Signal::Logs).quarantined[0];
    assert_eq!(q.kind, QuarantineKind::Segment);
    assert!(q.reason.contains("header version disagrees with footer"), "{}", q.reason);
}

#[test]
fn wal_in_a_newer_format_stops_startup_and_is_kept() {
    let d = tempfile::tempdir().unwrap();
    {
        let s = Storage::open(config(d.path()), None).unwrap();
        submit(&s, vec![log(1, "acknowledged")]);
    }
    let wal = only_wal(d.path());
    let mut b = std::fs::read(&wal).unwrap();
    b[8..10].copy_from_slice(&3u16.to_le_bytes());
    std::fs::write(&wal, &b).unwrap();

    assert!(matches!(check_formats(d.path()), Err(StorageError::UnsupportedFormat { found: 3, .. })));
    assert_newer_format_stops_startup(d.path(), &wal, "WAL file format", 3);
}

/// Append a record with a valid checksum and the given version byte.
fn append_record(wal: &Path, record_version: u8, payload: &[u8]) {
    use std::io::Write;
    let mut rec = vec![record_version];
    rec.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    rec.extend_from_slice(payload);
    let crc = crc32fast::hash(&rec);
    rec.extend_from_slice(&crc.to_le_bytes());
    std::fs::OpenOptions::new().append(true).open(wal).unwrap().write_all(&rec).unwrap();
}

#[test]
fn checksummed_wal_record_in_a_newer_format_stops_startup() {
    // An unknown record version, then a known record whose batch uses an
    // unknown codec version (the batch's first byte).
    for (record_version, payload, what, found) in
        [(2u8, &b"\x01\x00"[..], "WAL record format", 2u16), (1, &b"\x07\x00"[..], "event codec", 7)]
    {
        let d = tempfile::tempdir().unwrap();
        {
            let s = Storage::open(config(d.path()), None).unwrap();
            submit(&s, vec![log(1, "acknowledged")]);
        }
        let wal = only_wal(d.path());
        append_record(&wal, record_version, payload);
        // File headers are fine, but the read-only pre-start check replays
        // the WAL and finds the record; so does recovery, which stops before
        // truncating the WAL or quarantining anything.
        match check_formats(d.path()) {
            Err(StorageError::UnsupportedFormat { path, what: w, found: f, .. }) => {
                assert_eq!((path.as_path(), w, f), (wal.as_path(), what, found));
            }
            other => panic!("expected UnsupportedFormat, got {other:?}"),
        }
        assert_newer_format_stops_startup(d.path(), &wal, what, found);
    }
}

#[test]
fn unknown_record_version_with_a_bad_checksum_is_still_a_corrupt_tail() {
    let d = tempfile::tempdir().unwrap();
    {
        let s = Storage::open(config(d.path()), None).unwrap();
        submit(&s, vec![log(1, "acknowledged")]);
    }
    let wal = only_wal(d.path());
    {
        use std::io::Write;
        // Torn bytes after a crash can start with any value.
        let mut f = std::fs::OpenOptions::new().append(true).open(&wal).unwrap();
        f.write_all(&[9, 3, 0, 0, 0, 1, 2, 3, 0, 0, 0, 0]).unwrap();
    }
    // Damage, not a newer format: the pre-start check lets recovery judge it.
    check_formats(d.path()).unwrap();
    let s = Storage::open(config(d.path()), None).unwrap();
    assert_eq!(messages(&s), ["acknowledged"]);
    let q = &s.recovery_report(Signal::Logs).quarantined[0];
    assert_eq!(q.kind, QuarantineKind::WalTail);
    assert!(q.reason.contains("unknown record version"), "{}", q.reason);
}

/// Observer 0.1.0 (Vyrtel's first release, under its earlier name) wrote the
/// same layout with other magic bytes. Its files are not damage: startup
/// stops, names the file, and moves or truncates nothing.
#[test]
fn observer_files_stop_startup_and_are_kept() {
    // Rewrite a Vyrtel file's magic as Observer 0.1.0 wrote it.
    let as_observer = |path: &Path, magic: &[u8; 8]| {
        let mut b = std::fs::read(path).unwrap();
        b[..8].copy_from_slice(magic);
        std::fs::write(path, b).unwrap();
    };

    let d = tempfile::tempdir().unwrap();
    {
        let s = Storage::open(config(d.path()), None).unwrap();
        submit(&s, vec![log(1, "acknowledged")]);
    }
    let wal = only_wal(d.path());
    as_observer(&wal, wal::OBSERVER_WAL_MAGIC);
    let before = snapshot(d.path());
    for result in [check_formats(d.path()).map(|_| ()), Storage::open(config(d.path()), None).map(|_| ())] {
        match result {
            Err(e @ StorageError::ObserverFormat { .. }) => {
                assert!(e.to_string().ends_with("was written by Observer 0.1.0 (Vyrtel's first release, under its earlier name), which this Vyrtel cannot read"), "{e}");
            }
            other => panic!("expected ObserverFormat, got {other:?}"),
        }
    }
    assert_eq!(snapshot(d.path()), before, "nothing quarantined or truncated");

    let d = tempfile::tempdir().unwrap();
    let seg = sealed_segment(d.path());
    as_observer(&seg, storage::segment::OBSERVER_SEGMENT_MAGIC);
    let before = snapshot(d.path());
    assert!(matches!(check_formats(d.path()), Err(StorageError::ObserverFormat { path }) if path == seg));
    assert!(matches!(Storage::open(config(d.path()), None), Err(StorageError::ObserverFormat { path }) if path == seg));
    assert_eq!(snapshot(d.path()), before, "nothing quarantined");
}
