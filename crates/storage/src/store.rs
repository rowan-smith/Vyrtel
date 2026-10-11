//! The storage facade: three signal streams over one data directory.

use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use telemetry::{Signal, TelemetryEvent};

use crate::cache::{CacheStats, IndexCache};
use crate::config::StorageConfig;
use crate::error::{IoContext, Result, StorageError};
use crate::fault::{self, FaultOp, StorageFault};
use crate::fsutil;
use crate::recovery::{self, RecoveryReport, StreamDirs};
use crate::stream::{Ack, CommitHook, Stream, StreamParams, StreamSnapshot, StreamStats};

pub struct Storage {
    config: StorageConfig,
    streams: Vec<Stream>,
    cache: Arc<IndexCache>,
    /// Last write-probe result, reused for [`PROBE_TTL`].
    probe: Mutex<Option<(Instant, Option<StorageFault>)>>,
    _lock: File,
}

/// How long a write-probe result is reused, so frequent readiness checks
/// do not each create and fsync a file.
const PROBE_TTL: Duration = Duration::from_secs(1);

#[derive(Debug, Clone, Default)]
pub struct StorageStats {
    pub logs: StreamStats,
    pub traces: StreamStats,
    pub metrics: StreamStats,
    pub cache: CacheStats,
    pub tmp_bytes: u64,
    pub quarantine_bytes: u64,
}

impl StorageStats {
    pub fn get(&self, s: Signal) -> &StreamStats {
        match s {
            Signal::Logs => &self.logs,
            Signal::Traces => &self.traces,
            Signal::Metrics => &self.metrics,
        }
    }
}

impl Storage {
    /// Open the data directory, run crash recovery for every stream and
    /// start the writer/maintenance threads.
    pub fn open(config: StorageConfig, hook: Option<CommitHook>) -> Result<Storage> {
        let dir = &config.data_dir;
        fsutil::create_dir_all_durable(dir)?;
        let lock = lock_data_dir(dir)?;
        {
            let sub = "indexes";
            let p = dir.join(sub);
            std::fs::create_dir_all(&p).ctx(&p)?;
        }
        let tmp = dir.join("tmp");
        let removed = recovery::clean_tmp(&tmp)?;
        if removed > 0 {
            tracing::warn!(files = removed, "removed uncommitted temporary files from a previous run");
        }

        let cache = Arc::new(IndexCache::new(config.index_cache_bytes));
        let mut streams = Vec::new();
        for signal in Signal::ALL {
            let dirs = StreamDirs::new(dir, signal);
            let rec = recovery::recover(signal, &dirs, &config.segment)?;
            log_recovery(signal, &rec.report);
            let stream = Stream::start(
                StreamParams {
                    signal,
                    dirs,
                    config: config.stream(signal).clone(),
                    durability: config.durability,
                    fsync_interval: config.fsync_interval,
                    maintenance_interval: config.maintenance_interval,
                    seg_opts: config.segment.clone(),
                    compaction: config.compaction.clone(),
                    cache: cache.clone(),
                    hook: hook.clone(),
                },
                rec,
            )?;
            streams.push(stream);
        }
        Ok(Storage { config, streams, cache, probe: Mutex::new(None), _lock: lock })
    }

    pub fn config(&self) -> &StorageConfig {
        &self.config
    }

    pub fn data_dir(&self) -> &Path {
        &self.config.data_dir
    }

    pub fn stream(&self, s: Signal) -> &Stream {
        &self.streams[match s {
            Signal::Logs => 0,
            Signal::Traces => 1,
            Signal::Metrics => 2,
        }]
    }

    pub fn submit(&self, signal: Signal, events: Vec<TelemetryEvent>, ack: Ack) -> Result<()> {
        self.stream(signal).submit(events, ack)
    }

    pub fn snapshot(&self, signal: Signal) -> StreamSnapshot {
        self.stream(signal).snapshot()
    }

    pub fn cache(&self) -> &Arc<IndexCache> {
        &self.cache
    }

    pub fn recovery_report(&self, signal: Signal) -> &RecoveryReport {
        &self.stream(signal).recovery
    }

    pub fn stats(&self) -> StorageStats {
        StorageStats {
            logs: self.stream(Signal::Logs).stats(),
            traces: self.stream(Signal::Traces).stats(),
            metrics: self.stream(Signal::Metrics).stats(),
            cache: self.cache.stats(),
            tmp_bytes: fsutil::dir_size(&self.config.data_dir.join("tmp")),
            quarantine_bytes: fsutil::dir_size(&self.config.data_dir.join("quarantine")),
        }
    }

    /// Check that the data directory accepts new data: create, write 4 KiB
    /// to and fsync a file in `tmp/`, where every WAL and segment file is
    /// born, then delete it. Catches a full, read-only or no longer
    /// writable volume before ingest runs into it. Blocking; results are
    /// reused for one second.
    pub fn probe_writable(&self) -> std::result::Result<(), StorageFault> {
        let mut cache = self.probe.lock().unwrap();
        if let Some((at, result)) = &*cache
            && at.elapsed() < PROBE_TTL
        {
            return result.clone().map_or(Ok(()), Err);
        }
        let dir = &self.config.data_dir;
        let result = write_probe(&dir.join("tmp").join(".write-probe"), "probe", dir).err();
        let was_failing = cache.as_ref().is_some_and(|(_, r)| r.is_some());
        match &result {
            Some(f) if !was_failing => tracing::error!(
                reason = f.reason.as_str(),
                path = %f.path,
                error = %f.message,
                "data directory is not writable; readiness reports degraded"
            ),
            None if was_failing => tracing::info!("data directory is writable again"),
            _ => {}
        }
        *cache = Some((Instant::now(), result.clone()));
        result.map_or(Ok(()), Err)
    }

    /// Blocking helper: seal the active buffer of `signal` into a segment.
    pub fn rotate_blocking(&self, signal: Signal) -> Result<()> {
        let (tx, rx) = mpsc::sync_channel(1);
        self.stream(signal).rotate(Box::new(move |r| {
            let _ = tx.send(r);
        }));
        rx.recv_timeout(Duration::from_secs(120)).map_err(|_| StorageError::Unavailable("rotation timed out".into()))?
    }

    /// Blocking helper: run retention and compaction for `signal` now.
    pub fn maintain_blocking(&self, signal: Signal) -> Result<()> {
        let (tx, rx) = mpsc::sync_channel(1);
        self.stream(signal).run_maintenance(Box::new(move |r| {
            let _ = tx.send(r);
        }));
        rx.recv_timeout(Duration::from_secs(300))
            .map_err(|_| StorageError::Unavailable("maintenance timed out".into()))?
    }

    /// Stop accepting writes, write out every queued batch and flush the
    /// WALs. Once this returns, every batch ever acknowledged is in a
    /// flushed WAL; sealing or compaction may still be running (see
    /// [`Storage::shutdown`]).
    pub fn stop_writers(&self) {
        for s in &self.streams {
            s.begin_shutdown();
        }
        for s in &self.streams {
            s.stop_writer();
        }
    }

    /// [`Storage::stop_writers`], then wait for any seal or compaction in
    /// progress and stop the background threads. Interrupting that wait
    /// (process exit) is safe: recovery redoes the seal from the WAL.
    pub fn shutdown(&self) {
        self.stop_writers();
        for s in &self.streams {
            s.shutdown();
        }
    }
}

impl Drop for Storage {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn log_recovery(signal: Signal, r: &RecoveryReport) {
    tracing::info!(
        signal = signal.as_str(),
        segments = r.segments_loaded,
        wal_records = r.wal_records_replayed,
        wal_events = r.wal_events_replayed,
        sealed = r.segments_sealed,
        "recovered stream"
    );
    if r.segments_quarantined + r.wal_files_quarantined > 0 || r.wal_bytes_discarded > 0 {
        tracing::warn!(
            signal = signal.as_str(),
            quarantined_segments = r.segments_quarantined,
            quarantined_wals = r.wal_files_quarantined,
            discarded_wal_bytes = r.wal_bytes_discarded,
            "recovery found damaged files; see the quarantine directory"
        );
    }
}

/// Create, write 4 KiB to, fsync and delete the file at `path`; `point` is
/// its fault-injection point. Each caller uses its own file, so concurrent
/// probes do not trip over each other.
pub(crate) fn write_probe(path: &Path, point: &str, data_dir: &Path) -> std::result::Result<(), StorageFault> {
    let written = (|| {
        fault::check(point, path).ctx(path)?;
        let mut f = File::create(path).ctx(path)?;
        f.write_all(&[0u8; 4096]).ctx(path)?;
        f.sync_all().ctx(path)
    })();
    let _ = std::fs::remove_file(path);
    written.map_err(|e| StorageFault::from_error(FaultOp::Probe, &e, path, data_dir))
}

/// Take an exclusive lock on the data directory so two processes never
/// write the same WAL.
fn lock_data_dir(dir: &Path) -> Result<File> {
    let path: PathBuf = dir.join(".lock");
    let f = std::fs::OpenOptions::new().create(true).truncate(false).read(true).write(true).open(&path).ctx(&path)?;
    match f.try_lock() {
        Ok(()) => Ok(f),
        Err(std::fs::TryLockError::WouldBlock) => {
            Err(StorageError::Unavailable("data directory is in use by another Vyrtel process".into()))
        }
        Err(std::fs::TryLockError::Error(e)) => Err(e).ctx(&path),
    }
}
