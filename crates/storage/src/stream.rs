//! One signal stream (logs, traces or metrics): WAL writer thread, active
//! in-memory buffer, sealing into segments, compaction and retention.
//!
//! ```text
//!   submit() ──bounded queue──▶ writer thread ──▶ WAL append (+fsync)
//!                                    │
//!                                    ├─▶ active buffer (queryable)
//!                                    └─▶ rotate: freeze buffer, new WAL
//!                                              │
//!                         maintenance thread ◀─┘ seal → segment, delete WAL
//!                                              ├ retention (whole segments)
//!                                              └ compaction (small segments)
//! ```
//!
//! Memory is bounded by: queue capacity (events), the active buffer
//! (`segment_target_bytes`) and at most one frozen buffer awaiting sealing.
//! If sealing falls behind, the writer waits, the queue fills and callers
//! get [`StorageError::QueueFull`] — predictable backpressure instead of
//! unbounded growth.

use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, SyncSender, TrySendError};
use std::sync::{Arc, Condvar, Mutex, RwLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use telemetry::*;

use crate::cache::IndexCache;
use crate::codec;
use crate::config::{CompactionConfig, Durability, StreamConfig};
use crate::crash;
use crate::error::{Result, StorageError};
use crate::fsutil;
use crate::recovery::{Recovered, RecoveryReport, StreamDirs};
use crate::segment::{Segment, SegmentWriteOptions, write_segment};
use crate::wal::{WalHeader, WalWriter, wal_file_name};

/// A group of events committed together (one WAL record).
#[derive(Debug)]
pub struct Batch {
    pub events: Vec<TelemetryEvent>,
    pub min_ts: Timestamp,
    pub max_ts: Timestamp,
    pub approx_bytes: usize,
}

impl Batch {
    pub fn new(events: Vec<TelemetryEvent>) -> Self {
        let min_ts = events.iter().map(|e| e.timestamp).min().unwrap_or_default();
        let max_ts = events.iter().map(|e| e.timestamp).max().unwrap_or_default();
        let approx_bytes = events.iter().map(TelemetryEvent::approx_size).sum();
        Self { events, min_ts, max_ts, approx_bytes }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Committed {
    pub first_id: EventId,
    pub count: usize,
}

/// Completion callback. Storage is runtime-agnostic; the server bridges
/// this to an async oneshot.
pub type Ack = Box<dyn FnOnce(Result<Committed>) + Send>;
pub type Done = Box<dyn FnOnce(Result<()>) + Send>;
/// Called by the writer thread after a batch becomes queryable (live tail).
pub type CommitHook = Arc<dyn Fn(Signal, &Arc<Batch>) + Send + Sync>;

enum WriterMsg {
    Append { events: Vec<TelemetryEvent>, reserved: usize, ack: Ack },
    Rotate(Done),
    Shutdown(SyncSender<()>),
}

enum MaintMsg {
    Seal(Option<Done>),
    RunNow(Done),
    Shutdown,
}

#[derive(Clone)]
struct Frozen {
    wal_id: u64,
    batches: Vec<Arc<Batch>>,
}

struct State {
    active_wal: u64,
    active: Vec<Arc<Batch>>,
    active_bytes: usize,
    active_events: u64,
    active_since: Option<Instant>,
    frozen: Option<Frozen>,
    segments: Vec<Arc<Segment>>,
}

/// Point-in-time view used by queries. Holds `Arc`s, so it stays valid
/// while sealing, compaction or retention change the live catalog.
#[derive(Clone)]
pub struct StreamSnapshot {
    pub signal: Signal,
    pub segments: Vec<Arc<Segment>>,
    /// Unsealed batches (active + frozen), in commit order.
    pub batches: Vec<Arc<Batch>>,
}

#[derive(Debug, Clone, Default)]
pub struct StreamStats {
    pub segments: usize,
    pub segment_events: u64,
    pub segment_raw_bytes: u64,
    pub segment_data_bytes: u64,
    pub segment_index_bytes: u64,
    pub active_events: u64,
    pub active_bytes: u64,
    pub frozen_events: u64,
    pub wal_bytes: u64,
    pub queue_depth: usize,
    pub queue_capacity: usize,
    pub ingested_events: u64,
    pub rejected_batches: u64,
    pub oldest: Option<Timestamp>,
    pub newest: Option<Timestamp>,
    pub last_error: Option<String>,
    /// What startup recovery found, including anything quarantined.
    pub recovery: RecoveryReport,
}

struct Shared {
    signal: Signal,
    dirs: StreamDirs,
    config: StreamConfig,
    durability: Durability,
    fsync_interval: Duration,
    maintenance_interval: Duration,
    seg_opts: SegmentWriteOptions,
    compaction: CompactionConfig,
    cache: Arc<IndexCache>,
    hook: Option<CommitHook>,
    state: RwLock<State>,
    /// Signalled whenever the frozen buffer is sealed.
    sealed: (Mutex<()>, Condvar),
    next_file_id: AtomicU64,
    queued: AtomicUsize,
    ingested: AtomicU64,
    rejected: AtomicU64,
    shutting_down: AtomicBool,
    last_error: Mutex<Option<String>>,
}

impl Shared {
    fn set_error(&self, e: &StorageError) {
        tracing::error!(signal = self.signal.as_str(), error = %e, "storage error");
        *self.last_error.lock().unwrap() = Some(e.to_string());
    }

    fn clear_error(&self) {
        *self.last_error.lock().unwrap() = None;
    }
}

pub struct Stream {
    shared: Arc<Shared>,
    tx: SyncSender<WriterMsg>,
    maint_tx: Sender<MaintMsg>,
    threads: Mutex<Vec<JoinHandle<()>>>,
    pub recovery: RecoveryReport,
}

pub(crate) struct StreamParams {
    pub signal: Signal,
    pub dirs: StreamDirs,
    pub config: StreamConfig,
    pub durability: Durability,
    pub fsync_interval: Duration,
    pub maintenance_interval: Duration,
    pub seg_opts: SegmentWriteOptions,
    pub compaction: CompactionConfig,
    pub cache: Arc<IndexCache>,
    pub hook: Option<CommitHook>,
}

impl Stream {
    pub(crate) fn start(p: StreamParams, rec: Recovered) -> Result<Stream> {
        let active_bytes = rec.batches.iter().map(|b| b.approx_bytes).sum();
        let active_events = rec.batches.iter().map(|b| b.events.len() as u64).sum();
        let shared = Arc::new(Shared {
            signal: p.signal,
            dirs: p.dirs,
            durability: p.durability,
            fsync_interval: p.fsync_interval,
            maintenance_interval: p.maintenance_interval,
            seg_opts: p.seg_opts,
            compaction: p.compaction,
            cache: p.cache,
            hook: p.hook,
            state: RwLock::new(State {
                active_wal: rec.wal.id(),
                active_since: (!rec.batches.is_empty()).then(Instant::now),
                active: rec.batches,
                active_bytes,
                active_events,
                frozen: None,
                segments: rec.segments,
            }),
            sealed: (Mutex::new(()), Condvar::new()),
            next_file_id: AtomicU64::new(rec.next_file_id),
            queued: AtomicUsize::new(0),
            ingested: AtomicU64::new(0),
            rejected: AtomicU64::new(0),
            shutting_down: AtomicBool::new(false),
            last_error: Mutex::new(None),
            config: p.config,
        });

        let capacity = shared.config.queue_events.max(1);
        let (tx, rx) = mpsc::sync_channel(capacity);
        let (maint_tx, maint_rx) = mpsc::channel();
        let name = shared.signal.as_str();

        let writer = {
            let shared = shared.clone();
            let maint_tx = maint_tx.clone();
            let wal = rec.wal;
            let next_event = rec.next_event_id;
            std::thread::Builder::new()
                .name(format!("vyrtel-wal-{name}"))
                .spawn(move || Writer::new(shared, wal, next_event, maint_tx).run(rx))
                .map_err(|e| StorageError::Unavailable(format!("spawn writer: {e}")))?
        };
        let maintenance = {
            let shared = shared.clone();
            std::thread::Builder::new()
                .name(format!("vyrtel-maint-{name}"))
                .spawn(move || maintenance_loop(shared, maint_rx))
                .map_err(|e| StorageError::Unavailable(format!("spawn maintenance: {e}")))?
        };

        Ok(Stream { shared, tx, maint_tx, threads: Mutex::new(vec![writer, maintenance]), recovery: rec.report })
    }

    pub fn signal(&self) -> Signal {
        self.shared.signal
    }

    /// Queue events for writing. Returns immediately: `Err` means the batch
    /// was rejected (backpressure); otherwise `ack` runs once the batch is
    /// in the WAL (and fsynced in strict mode) and visible to queries.
    pub fn submit(&self, events: Vec<TelemetryEvent>, ack: Ack) -> Result<()> {
        let n = events.len();
        if n == 0 {
            ack(Ok(Committed { first_id: EventId(0), count: 0 }));
            return Ok(());
        }
        let capacity = self.shared.config.queue_events.max(1);
        if n > capacity {
            return Err(StorageError::BatchTooLarge(n));
        }
        if self.shared.shutting_down.load(Ordering::Acquire) {
            return Err(StorageError::Unavailable("shutting down".into()));
        }
        // Reserve queue capacity in events, not messages, so a few huge
        // batches cannot hide behind a small message count.
        let mut cur = self.shared.queued.load(Ordering::Acquire);
        loop {
            if cur + n > capacity {
                self.shared.rejected.fetch_add(1, Ordering::Relaxed);
                return Err(StorageError::QueueFull);
            }
            match self.shared.queued.compare_exchange_weak(cur, cur + n, Ordering::AcqRel, Ordering::Acquire) {
                Ok(_) => break,
                Err(actual) => cur = actual,
            }
        }
        match self.tx.try_send(WriterMsg::Append { events, reserved: n, ack }) {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(_)) => {
                self.shared.queued.fetch_sub(n, Ordering::AcqRel);
                self.shared.rejected.fetch_add(1, Ordering::Relaxed);
                Err(StorageError::QueueFull)
            }
            Err(TrySendError::Disconnected(_)) => {
                self.shared.queued.fetch_sub(n, Ordering::AcqRel);
                Err(StorageError::Unavailable("writer stopped".into()))
            }
        }
    }

    /// Seal the active buffer into a segment now; `done` runs once the
    /// segment is committed.
    pub fn rotate(&self, done: Done) {
        if let Err(TrySendError::Full(WriterMsg::Rotate(done)) | TrySendError::Disconnected(WriterMsg::Rotate(done))) =
            self.tx.try_send(WriterMsg::Rotate(done))
        {
            done(Err(StorageError::QueueFull));
        }
    }

    /// Run retention and compaction now; `done` runs when finished.
    pub fn run_maintenance(&self, done: Done) {
        if let Err(mpsc::SendError(MaintMsg::RunNow(done))) = self.maint_tx.send(MaintMsg::RunNow(done)) {
            done(Err(StorageError::Unavailable("maintenance stopped".into())));
        }
    }

    pub fn snapshot(&self) -> StreamSnapshot {
        let st = self.shared.state.read().unwrap();
        let mut batches = Vec::with_capacity(st.active.len());
        if let Some(f) = &st.frozen {
            batches.extend(f.batches.iter().cloned());
        }
        batches.extend(st.active.iter().cloned());
        StreamSnapshot { signal: self.shared.signal, segments: st.segments.clone(), batches }
    }

    pub fn cache(&self) -> &Arc<IndexCache> {
        &self.shared.cache
    }

    pub fn stats(&self) -> StreamStats {
        let st = self.shared.state.read().unwrap();
        let mut s = StreamStats {
            segments: st.segments.len(),
            active_events: st.active_events,
            active_bytes: st.active_bytes as u64,
            frozen_events: st.frozen.as_ref().map_or(0, |f| f.batches.iter().map(|b| b.events.len() as u64).sum()),
            queue_depth: self.shared.queued.load(Ordering::Relaxed),
            queue_capacity: self.shared.config.queue_events,
            ingested_events: self.shared.ingested.load(Ordering::Relaxed),
            rejected_batches: self.shared.rejected.load(Ordering::Relaxed),
            last_error: self.shared.last_error.lock().unwrap().clone(),
            recovery: self.recovery.clone(),
            ..Default::default()
        };
        let mut oldest: Option<Timestamp> = None;
        let mut newest: Option<Timestamp> = None;
        let mut see = |lo: Timestamp, hi: Timestamp| {
            oldest = Some(oldest.map_or(lo, |o| o.min(lo)));
            newest = Some(newest.map_or(hi, |n| n.max(hi)));
        };
        for seg in &st.segments {
            let m = &seg.summary;
            s.segment_events += m.event_count;
            s.segment_raw_bytes += m.raw_bytes;
            s.segment_data_bytes += m.data_bytes;
            s.segment_index_bytes += m.index_bytes;
            see(m.min_ts, m.max_ts);
        }
        for b in st.active.iter().chain(st.frozen.iter().flat_map(|f| f.batches.iter())) {
            see(b.min_ts, b.max_ts);
        }
        drop(st);
        s.oldest = oldest;
        s.newest = newest;
        s.wal_bytes = fsutil::dir_size(&self.shared.dirs.wal);
        s
    }

    pub(crate) fn begin_shutdown(&self) {
        self.shared.shutting_down.store(true, Ordering::Release);
        self.shared.sealed.1.notify_all();
    }

    /// Flush and stop the threads. Unsealed data stays in the WAL and is
    /// replayed on the next start, so shutdown is fast.
    pub(crate) fn shutdown(&self) {
        self.begin_shutdown();
        let (done_tx, done_rx) = mpsc::sync_channel(1);
        // A blocking send: the queue may be full of work that must finish.
        if self.tx.send(WriterMsg::Shutdown(done_tx)).is_ok() {
            let _ = done_rx.recv_timeout(Duration::from_secs(30));
        }
        let _ = self.maint_tx.send(MaintMsg::Shutdown);
        for t in self.threads.lock().unwrap().drain(..) {
            let _ = t.join();
        }
    }
}

struct Pending {
    batch: Arc<Batch>,
    reserved: usize,
    ack: Ack,
    committed: Committed,
}

struct Writer {
    shared: Arc<Shared>,
    wal: WalWriter,
    next_event: u64,
    maint: Sender<MaintMsg>,
    last_sync: Instant,
}

impl Writer {
    fn new(shared: Arc<Shared>, wal: WalWriter, next_event: u64, maint: Sender<MaintMsg>) -> Self {
        Self { shared, wal, next_event, maint, last_sync: Instant::now() }
    }

    fn run(mut self, rx: Receiver<WriterMsg>) {
        let tick = self.shared.fsync_interval.min(Duration::from_millis(500));
        loop {
            let first = match rx.recv_timeout(tick) {
                Ok(m) => Some(m),
                Err(RecvTimeoutError::Timeout) => None,
                Err(RecvTimeoutError::Disconnected) => break,
            };
            let mut msgs = Vec::new();
            if let Some(m) = first {
                msgs.push(m);
                // Group commit: drain whatever else is already queued so
                // one fsync covers many requests.
                while msgs.len() < 1024 {
                    match rx.try_recv() {
                        Ok(m) => msgs.push(m),
                        Err(_) => break,
                    }
                }
            }

            let mut pending: Vec<Pending> = Vec::new();
            for msg in msgs {
                match msg {
                    WriterMsg::Append { events, reserved, ack } => self.append(events, reserved, ack, &mut pending),
                    WriterMsg::Rotate(done) => {
                        self.commit(&mut pending);
                        self.rotate(Some(done));
                    }
                    WriterMsg::Shutdown(done) => {
                        self.commit(&mut pending);
                        if let Err(e) = self.wal.sync() {
                            self.shared.set_error(&e);
                        }
                        let _ = done.send(());
                        return;
                    }
                }
            }
            self.commit(&mut pending);

            if self.shared.durability == Durability::Normal && self.last_sync.elapsed() >= self.shared.fsync_interval {
                if let Err(e) = self.wal.sync() {
                    self.shared.set_error(&e);
                }
                self.last_sync = Instant::now();
            }

            if self.should_rotate() {
                self.rotate(None);
            }
        }
    }

    fn append(&mut self, mut events: Vec<TelemetryEvent>, reserved: usize, ack: Ack, pending: &mut Vec<Pending>) {
        let first = self.next_event;
        for e in events.iter_mut() {
            e.id = EventId(self.next_event);
            self.next_event += 1;
        }
        let bytes = codec::encode_batch(&events);
        match self.wal.append(&bytes) {
            Ok(()) => pending.push(Pending {
                committed: Committed { first_id: EventId(first), count: events.len() },
                batch: Arc::new(Batch::new(events)),
                reserved,
                ack,
            }),
            Err(e) => {
                // Ids are not reused: gaps are harmless and avoid any
                // chance of two events sharing an id after a retry.
                self.shared.set_error(&e);
                self.shared.queued.fetch_sub(reserved, Ordering::AcqRel);
                ack(Err(StorageError::Unavailable("write-ahead log write failed".into())));
            }
        }
    }

    /// Make pending batches durable (strict mode), visible, and acknowledge.
    fn commit(&mut self, pending: &mut Vec<Pending>) {
        if pending.is_empty() {
            return;
        }
        crash::point("wal.before_sync");
        if self.shared.durability == Durability::Strict {
            if let Err(e) = self.wal.sync() {
                self.shared.set_error(&e);
                for p in pending.drain(..) {
                    self.shared.queued.fetch_sub(p.reserved, Ordering::AcqRel);
                    (p.ack)(Err(StorageError::Unavailable("fsync failed".into())));
                }
                return;
            }
            self.last_sync = Instant::now();
        }
        crash::point("wal.before_ack");
        {
            let mut st = self.shared.state.write().unwrap();
            for p in pending.iter() {
                st.active_bytes += p.batch.approx_bytes;
                st.active_events += p.batch.events.len() as u64;
                st.active.push(p.batch.clone());
            }
            if st.active_since.is_none() {
                st.active_since = Some(Instant::now());
            }
        }
        for p in pending.drain(..) {
            self.shared.ingested.fetch_add(p.batch.events.len() as u64, Ordering::Relaxed);
            if let Some(hook) = &self.shared.hook {
                hook(self.shared.signal, &p.batch);
            }
            self.shared.queued.fetch_sub(p.reserved, Ordering::AcqRel);
            (p.ack)(Ok(p.committed));
        }
        crash::point("wal.after_ack");
    }

    fn should_rotate(&self) -> bool {
        let st = self.shared.state.read().unwrap();
        if st.active.is_empty() {
            return false;
        }
        let c = &self.shared.config;
        st.active_bytes as u64 >= c.segment_target_bytes
            || st.active_events >= c.segment_max_events
            || st.active_since.is_some_and(|t| t.elapsed() >= c.segment_max_age)
    }

    /// Freeze the active buffer, start a new WAL and ask the maintenance
    /// thread to seal the frozen one.
    fn rotate(&mut self, done: Option<Done>) {
        // Only one frozen buffer may exist: wait for the previous seal.
        // While we wait, the queue fills and producers see backpressure.
        {
            let (lock, cv) = &self.shared.sealed;
            let mut guard = lock.lock().unwrap();
            loop {
                if self.shared.state.read().unwrap().frozen.is_none() {
                    break;
                }
                if self.shared.shutting_down.load(Ordering::Acquire) {
                    if let Some(d) = done {
                        d(Err(StorageError::Unavailable("shutting down".into())));
                    }
                    return;
                }
                guard = cv.wait_timeout(guard, Duration::from_millis(200)).unwrap().0;
            }
        }
        if self.shared.state.read().unwrap().active.is_empty() {
            if let Some(d) = done {
                d(Ok(()));
            }
            return;
        }
        if let Err(e) = self.wal.sync() {
            self.shared.set_error(&e);
            if let Some(d) = done {
                d(Err(e));
            }
            return;
        }
        let new_id = self.shared.next_file_id.fetch_add(1, Ordering::AcqRel);
        let new_wal = match WalWriter::create(
            &self.shared.dirs.wal,
            &self.shared.dirs.tmp,
            WalHeader { signal: self.shared.signal, wal_id: new_id },
        ) {
            Ok(w) => w,
            Err(e) => {
                // Keep writing to the current WAL and try again later.
                self.shared.set_error(&e);
                if let Some(d) = done {
                    d(Err(e));
                }
                return;
            }
        };
        crash::point("rotate.after_wal_create");
        let old = std::mem::replace(&mut self.wal, new_wal);
        {
            let mut st = self.shared.state.write().unwrap();
            let batches = std::mem::take(&mut st.active);
            st.frozen = Some(Frozen { wal_id: old.id(), batches });
            st.active_wal = new_id;
            st.active_bytes = 0;
            st.active_events = 0;
            st.active_since = None;
        }
        // Close the frozen WAL so it can be deleted after sealing (Windows).
        drop(old);
        let _ = self.maint.send(MaintMsg::Seal(done));
    }
}

fn maintenance_loop(shared: Arc<Shared>, rx: Receiver<MaintMsg>) {
    let mut seal_waiters: Vec<Done> = Vec::new();
    let mut next_periodic = Instant::now() + shared.maintenance_interval;
    let mut retry_at: Option<Instant> = None;
    loop {
        let now = Instant::now();
        let wake = retry_at.map_or(next_periodic, |r| r.min(next_periodic));
        let msg = rx.recv_timeout(wake.saturating_duration_since(now));
        match msg {
            Ok(MaintMsg::Seal(done)) => {
                seal_waiters.extend(done);
                retry_at = Some(Instant::now());
            }
            Ok(MaintMsg::RunNow(done)) => {
                let r = run_periodic(&shared);
                done(r);
            }
            Ok(MaintMsg::Shutdown) | Err(RecvTimeoutError::Disconnected) => {
                for d in seal_waiters.drain(..) {
                    d(Err(StorageError::Unavailable("shutting down".into())));
                }
                return;
            }
            Err(RecvTimeoutError::Timeout) => {}
        }

        if retry_at.is_some_and(|r| Instant::now() >= r) {
            match seal_frozen(&shared) {
                Ok(()) => {
                    retry_at = None;
                    for d in seal_waiters.drain(..) {
                        d(Ok(()));
                    }
                }
                Err(e) => {
                    // Keep the frozen buffer (and its WAL) and retry. The
                    // data is safe; ingestion slows via backpressure.
                    shared.set_error(&e);
                    retry_at = Some(Instant::now() + Duration::from_secs(5));
                }
            }
        }

        if Instant::now() >= next_periodic {
            if let Err(e) = run_periodic(&shared) {
                shared.set_error(&e);
            }
            next_periodic = Instant::now() + shared.maintenance_interval;
        }
    }
}

fn run_periodic(shared: &Shared) -> Result<()> {
    apply_retention(shared, Timestamp::now())?;
    compact(shared)?;
    Ok(())
}

fn seal_frozen(shared: &Shared) -> Result<()> {
    let frozen = shared.state.read().unwrap().frozen.clone();
    let Some(frozen) = frozen else {
        return Ok(());
    };
    let mut refs: Vec<&TelemetryEvent> = frozen.batches.iter().flat_map(|b| b.events.iter()).collect();
    refs.sort_by_key(|e| (e.timestamp, e.id));
    let segment = if refs.is_empty() {
        None
    } else {
        let w = write_segment(
            &shared.dirs.segments,
            &shared.dirs.tmp,
            frozen.wal_id,
            shared.signal,
            &refs,
            Vec::new(),
            &shared.seg_opts,
        )?;
        Some(Arc::new(Segment::open(&w.path)?))
    };
    drop(refs);
    crash::point("seal.before_wal_delete");

    // The segment is committed; the WAL is now redundant. If deleting it
    // fails, recovery will notice the segment exists and delete it then.
    let wal_path = shared.dirs.wal.join(wal_file_name(frozen.wal_id));
    if let Err(e) = fsutil::remove_file_if_exists(&wal_path).and_then(|_| fsutil::sync_dir(&shared.dirs.wal)) {
        tracing::warn!(error = %e, "could not delete sealed WAL; it will be removed on restart");
    }

    {
        // Swap frozen → segment atomically for queries.
        let mut st = shared.state.write().unwrap();
        if let Some(s) = segment {
            tracing::debug!(
                signal = shared.signal.as_str(),
                segment = s.summary.id,
                events = s.summary.event_count,
                raw = s.summary.raw_bytes,
                stored = s.file_len,
                "sealed segment"
            );
            st.segments.push(s);
        }
        st.frozen = None;
    }
    shared.clear_error();
    let (lock, cv) = &shared.sealed;
    let _g = lock.lock().unwrap();
    cv.notify_all();
    Ok(())
}

/// Delete whole segments whose newest event is older than the retention
/// period. Removing from the catalog first means queries stop using a
/// segment before its file goes away; a crash in between merely leaves a
/// file that the next pass deletes again.
pub(crate) fn apply_retention_at(
    shared_signal: Signal,
    retention: Option<Duration>,
    now: Timestamp,
    segments: &mut Vec<Arc<Segment>>,
) -> Vec<Arc<Segment>> {
    let Some(ret) = retention else {
        return Vec::new();
    };
    let cutoff = now.saturating_sub_nanos(i64::try_from(ret.as_nanos()).unwrap_or(i64::MAX));
    let (expired, keep): (Vec<_>, Vec<_>) =
        std::mem::take(segments).into_iter().partition(|s| s.summary.max_ts < cutoff);
    *segments = keep;
    if !expired.is_empty() {
        tracing::info!(
            signal = shared_signal.as_str(),
            segments = expired.len(),
            "retention: deleting expired segments"
        );
    }
    expired
}

fn apply_retention(shared: &Shared, now: Timestamp) -> Result<()> {
    let expired = {
        let mut st = shared.state.write().unwrap();
        apply_retention_at(shared.signal, shared.config.retention, now, &mut st.segments)
    };
    if expired.is_empty() {
        return Ok(());
    }
    for s in expired {
        shared.cache.evict(shared.signal, s.summary.id);
        let path = s.path.clone();
        drop(s);
        fsutil::remove_file_if_exists(&path)?;
    }
    fsutil::sync_dir(&shared.dirs.segments)
}

/// Merge small segments (typically sealed by age at low volume) into one
/// larger segment. The output lists its inputs in `replaces`, so a crash
/// after the output is committed but before inputs are deleted is resolved
/// by recovery without duplicating events.
fn compact(shared: &Shared) -> Result<()> {
    let c = &shared.compaction;
    if !c.enabled {
        return Ok(());
    }
    let small_limit = shared.config.segment_target_bytes / 4;
    let mut candidates: Vec<Arc<Segment>> =
        shared.state.read().unwrap().segments.iter().filter(|s| s.summary.raw_bytes < small_limit).cloned().collect();
    if candidates.len() < c.min_inputs.max(2) {
        return Ok(());
    }
    candidates.sort_by_key(|s| (s.summary.min_ts, s.summary.id));
    let mut inputs = Vec::new();
    let mut total = 0u64;
    for s in candidates {
        if total + s.summary.raw_bytes > c.max_input_bytes && inputs.len() >= 2 {
            break;
        }
        total += s.summary.raw_bytes;
        inputs.push(s);
    }
    if inputs.len() < c.min_inputs.max(2) {
        return Ok(());
    }

    let mut events = Vec::new();
    for s in &inputs {
        events.extend(s.read_all()?);
    }
    events.sort_by_key(|e| (e.timestamp, e.id));
    let refs: Vec<&TelemetryEvent> = events.iter().collect();
    let id = shared.next_file_id.fetch_add(1, Ordering::AcqRel);
    let replaces: Vec<u64> = inputs.iter().map(|s| s.summary.id).collect();
    let w = write_segment(
        &shared.dirs.segments,
        &shared.dirs.tmp,
        id,
        shared.signal,
        &refs,
        replaces.clone(),
        &shared.seg_opts,
    )?;
    drop(refs);
    drop(events);
    let merged = Arc::new(Segment::open(&w.path)?);
    tracing::info!(
        signal = shared.signal.as_str(),
        inputs = replaces.len(),
        output = id,
        events = merged.summary.event_count,
        "compacted segments"
    );
    {
        let mut st = shared.state.write().unwrap();
        st.segments.retain(|s| !replaces.contains(&s.summary.id));
        st.segments.push(merged);
    }
    crash::point("compact.before_input_delete");
    for (i, s) in inputs.into_iter().enumerate() {
        if i == 1 {
            crash::point("compact.mid_input_delete");
        }
        shared.cache.evict(shared.signal, s.summary.id);
        let path = s.path.clone();
        drop(s);
        fsutil::remove_file_if_exists(&path)?;
    }
    fsutil::sync_dir(&shared.dirs.segments)
}
