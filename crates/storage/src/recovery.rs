//! Startup recovery for one signal stream.
//!
//! Invariants that make this safe (see docs/storage-format.md):
//! * A segment is visible iff its file was renamed into `segments/`, which
//!   happens only after it was completely written and fsynced.
//! * WAL `N` and segment `N` hold the same events. The WAL is deleted only
//!   after segment `N` is committed, so if both exist the segment wins.
//! * Compaction output lists the segments it `replaces`. If any of those
//!   still exist the compaction committed but cleanup did not finish.
//! * Anything in `tmp/` is uncommitted and is deleted.
//!
//! Recovery never repairs data by guessing: unreadable segments and WAL
//! tails are moved to `quarantine/` for inspection and logged loudly.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use telemetry::{Signal, TelemetryEvent};

use crate::codec;
use crate::error::{IoContext, Result};
use crate::fsutil;
use crate::segment::{Segment, SegmentWriteOptions, parse_segment_file_name, write_segment};
use crate::stream::Batch;
use crate::wal::{self, TailState, WalHeader, WalWriter, parse_wal_file_name, wal_file_name};

#[derive(Debug, Clone)]
pub struct StreamDirs {
    pub wal: PathBuf,
    pub segments: PathBuf,
    pub quarantine: PathBuf,
    pub tmp: PathBuf,
}

impl StreamDirs {
    pub fn new(data_dir: &Path, signal: Signal) -> Self {
        Self {
            wal: data_dir.join("wal").join(signal.as_str()),
            segments: data_dir.join("segments").join(signal.as_str()),
            quarantine: data_dir.join("quarantine").join(signal.as_str()),
            tmp: data_dir.join("tmp"),
        }
    }

    pub fn create(&self) -> Result<()> {
        for d in [&self.wal, &self.segments, &self.tmp] {
            std::fs::create_dir_all(d).ctx(d)?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Default)]
pub struct RecoveryReport {
    pub segments_loaded: usize,
    pub segments_quarantined: usize,
    pub replaced_segments_removed: usize,
    pub stale_wals_removed: usize,
    pub wal_files_replayed: usize,
    pub wal_records_replayed: usize,
    pub wal_events_replayed: usize,
    pub wal_bytes_discarded: u64,
    pub wal_files_quarantined: usize,
    pub segments_sealed: usize,
}

pub struct Recovered {
    pub segments: Vec<Arc<Segment>>,
    pub wal: WalWriter,
    pub batches: Vec<Arc<Batch>>,
    pub next_file_id: u64,
    pub next_event_id: u64,
    pub report: RecoveryReport,
}

fn quarantine(dirs: &StreamDirs, path: &Path) -> Result<()> {
    std::fs::create_dir_all(&dirs.quarantine).ctx(&dirs.quarantine)?;
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("unknown");
    let dest = dirs.quarantine.join(format!("{name}.{}", telemetry::Timestamp::now().millis()));
    fsutil::rename(path, &dest)
}

fn list(dir: &Path) -> Result<Vec<(String, PathBuf)>> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir).ctx(dir)? {
        let entry = entry.ctx(dir)?;
        if entry.file_type().ctx(entry.path())?.is_file()
            && let Some(name) = entry.file_name().to_str()
        {
            out.push((name.to_string(), entry.path()));
        }
    }
    out.sort();
    Ok(out)
}

pub fn recover(signal: Signal, dirs: &StreamDirs, seg_opts: &SegmentWriteOptions) -> Result<Recovered> {
    dirs.create()?;
    let mut report = RecoveryReport::default();
    let mut max_file_id = 0u64;
    let mut max_event_id = 0u64;

    // 1. Segments.
    let mut segments: Vec<Arc<Segment>> = Vec::new();
    for (name, path) in list(&dirs.segments)? {
        let Some(id) = parse_segment_file_name(&name) else {
            tracing::warn!(signal = signal.as_str(), file = %name, "ignoring unexpected file in segment directory");
            continue;
        };
        max_file_id = max_file_id.max(id);
        match Segment::open(&path) {
            Ok(s) if s.summary.id == id && s.summary.signal == signal => {
                segments.push(Arc::new(s));
            }
            Ok(_) | Err(_) => {
                tracing::error!(
                    signal = signal.as_str(),
                    segment = id,
                    "segment failed validation; moving to quarantine"
                );
                quarantine(dirs, &path)?;
                report.segments_quarantined += 1;
            }
        }
    }

    // 2. Finish interrupted compactions: inputs listed in `replaces` that
    //    still exist are superseded by the committed output.
    let replaced: BTreeSet<u64> = segments.iter().flat_map(|s| s.summary.replaces.iter().copied()).collect();
    max_file_id = max_file_id.max(replaced.iter().copied().max().unwrap_or(0));
    let (dead, live): (Vec<_>, Vec<_>) = segments.into_iter().partition(|s| replaced.contains(&s.summary.id));
    for s in dead {
        let path = s.path.clone();
        drop(s);
        fsutil::remove_file_if_exists(&path)?;
        report.replaced_segments_removed += 1;
    }
    let segments = live;
    if report.replaced_segments_removed > 0 {
        fsutil::sync_dir(&dirs.segments)?;
    }
    for s in &segments {
        max_event_id = max_event_id.max(s.summary.max_event_id);
    }
    let committed: BTreeSet<u64> = segments.iter().map(|s| s.summary.id).collect();
    report.segments_loaded = segments.len();

    // 3. WAL files.
    let mut wals: Vec<(u64, PathBuf)> = Vec::new();
    for (name, path) in list(&dirs.wal)? {
        match parse_wal_file_name(&name) {
            Some(id) => wals.push((id, path)),
            None => tracing::warn!(signal = signal.as_str(), file = %name, "ignoring unexpected file in WAL directory"),
        }
    }
    let mut replayed: Vec<(u64, PathBuf, u64, Vec<Arc<Batch>>)> = Vec::new();
    for (id, path) in wals {
        max_file_id = max_file_id.max(id);
        if committed.contains(&id) || replaced.contains(&id) {
            // Sealed before the crash; the segment is authoritative.
            fsutil::remove_file_if_exists(&path)?;
            report.stale_wals_removed += 1;
            continue;
        }
        let mut batches = Vec::new();
        let result = wal::replay(&path, |payload| {
            let events = codec::decode_batch(payload).map_err(|_| "undecodable batch")?;
            if !events.is_empty() {
                batches.push(Arc::new(Batch::new(events)));
            }
            Ok(())
        });
        let rep = match result {
            Ok(r) => r,
            Err(_) => {
                tracing::error!(signal = signal.as_str(), wal = id, "WAL header unreadable; moving to quarantine");
                quarantine(dirs, &path)?;
                report.wal_files_quarantined += 1;
                continue;
            }
        };
        if rep.header != (WalHeader { signal, wal_id: id }) {
            tracing::error!(
                signal = signal.as_str(),
                wal = id,
                "WAL header does not match file name; moving to quarantine"
            );
            quarantine(dirs, &path)?;
            report.wal_files_quarantined += 1;
            continue;
        }
        if rep.tail != TailState::Clean {
            let discarded = rep.file_len - rep.valid_len;
            tracing::warn!(
                signal = signal.as_str(),
                wal = id,
                tail = ?rep.tail,
                discarded_bytes = discarded,
                "WAL ends with an incomplete or corrupt record; replaying only the valid prefix"
            );
            preserve_tail(dirs, &path, rep.valid_len, id)?;
            report.wal_bytes_discarded += discarded;
        }
        report.wal_files_replayed += 1;
        report.wal_records_replayed += rep.records;
        for b in &batches {
            report.wal_events_replayed += b.events.len();
            for e in &b.events {
                max_event_id = max_event_id.max(e.id.0);
            }
        }
        replayed.push((id, path, rep.valid_len, batches));
    }

    // 4. Every WAL except the newest is sealed now; the newest stays active.
    let active = replayed.pop();
    let mut segments = segments;
    for (id, path, _, batches) in replayed {
        if !batches.is_empty() {
            let mut refs: Vec<&TelemetryEvent> = batches.iter().flat_map(|b| b.events.iter()).collect();
            refs.sort_by_key(|e| (e.timestamp, e.id));
            let w = write_segment(&dirs.segments, &dirs.tmp, id, signal, &refs, vec![], seg_opts)?;
            segments.push(Arc::new(Segment::open(&w.path)?));
            report.segments_sealed += 1;
        }
        fsutil::remove_file_if_exists(&path)?;
    }
    fsutil::sync_dir(&dirs.wal)?;

    let next_file_id = max_file_id + 1;
    let (wal, batches, next_file_id) = match active {
        Some((id, path, valid_len, batches)) => (WalWriter::open_append(&path, id, valid_len)?, batches, next_file_id),
        None => (
            WalWriter::create(&dirs.wal, &dirs.tmp, WalHeader { signal, wal_id: next_file_id })?,
            Vec::new(),
            next_file_id + 1,
        ),
    };

    segments.sort_by_key(|s| s.summary.id);
    Ok(Recovered { segments, wal, batches, next_file_id, next_event_id: max_event_id + 1, report })
}

/// Copy the unusable tail of a WAL into quarantine before it is truncated,
/// so an operator can inspect what was lost.
fn preserve_tail(dirs: &StreamDirs, path: &Path, valid_len: u64, id: u64) -> Result<()> {
    let bytes = std::fs::read(path).ctx(path)?;
    if (valid_len as usize) < bytes.len() {
        std::fs::create_dir_all(&dirs.quarantine).ctx(&dirs.quarantine)?;
        let dest = dirs.quarantine.join(format!("{}.tail-{valid_len}", wal_file_name(id)));
        std::fs::write(&dest, &bytes[valid_len as usize..]).ctx(&dest)?;
    }
    Ok(())
}

/// Delete stale temporary files. Nothing in `tmp/` is ever committed data.
pub fn clean_tmp(tmp: &Path) -> Result<usize> {
    std::fs::create_dir_all(tmp).ctx(tmp)?;
    let mut n = 0;
    for (_, path) in list(tmp)? {
        fsutil::remove_file_if_exists(&path)?;
        n += 1;
    }
    Ok(n)
}
