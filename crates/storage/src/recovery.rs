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
//! tails are moved to `quarantine/<signal>/` for inspection, fsynced there
//! before the original is removed or truncated, logged loudly and listed in
//! [`RecoveryReport::quarantined`]. An I/O error that is not evidence of
//! damage (permissions, a failing disk) aborts startup instead: moving a
//! readable-but-unread WAL aside would hide acknowledged telemetry.

use std::collections::BTreeSet;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use telemetry::{Signal, TelemetryEvent};

use crate::codec;
use crate::crash;
use crate::error::{IoContext, Result, StorageError};
use crate::fsutil;
use crate::segment::{Segment, SegmentWriteOptions, parse_segment_file_name, write_segment};
use crate::stream::Batch;
use crate::wal::{self, ReplayError, TailState, WalHeader, WalReplay, WalWriter, parse_wal_file_name, wal_file_name};

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

/// What kind of evidence recovery moved aside.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuarantineKind {
    /// A whole segment file that failed validation. Its events are rebuilt
    /// only if the WAL it was sealed from still exists.
    Segment,
    /// A whole WAL file whose header is unreadable or names another stream.
    Wal,
    /// The bytes after the last valid record of a WAL (a torn or corrupt
    /// tail). The valid prefix was replayed.
    WalTail,
}

impl QuarantineKind {
    pub fn as_str(self) -> &'static str {
        match self {
            QuarantineKind::Segment => "segment",
            QuarantineKind::Wal => "wal",
            QuarantineKind::WalTail => "walTail",
        }
    }
}

/// One file (or WAL tail) that recovery preserved in `quarantine/<signal>/`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuarantinedFile {
    pub signal: Signal,
    pub kind: QuarantineKind,
    /// Name of the damaged file in its stream directory, e.g. `000000000003.wal`.
    pub source: String,
    /// Where the preserved bytes now live.
    pub path: PathBuf,
    pub bytes: u64,
    pub reason: String,
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
    /// Everything moved to quarantine by this recovery, in discovery order.
    pub quarantined: Vec<QuarantinedFile>,
}

pub struct Recovered {
    pub segments: Vec<Arc<Segment>>,
    pub wal: WalWriter,
    pub batches: Vec<Arc<Batch>>,
    pub next_file_id: u64,
    pub next_event_id: u64,
    pub report: RecoveryReport,
}

/// A destination in the quarantine directory that does not exist yet, so a
/// second crash never overwrites the evidence of the first.
fn quarantine_dest(dirs: &StreamDirs, base: &str) -> Result<PathBuf> {
    std::fs::create_dir_all(&dirs.quarantine).ctx(&dirs.quarantine)?;
    let stamp = telemetry::Timestamp::now().millis();
    let mut n = 0u32;
    loop {
        let name = if n == 0 { format!("{base}.{stamp}") } else { format!("{base}.{stamp}-{n}") };
        let dest = dirs.quarantine.join(name);
        if !dest.exists() {
            return Ok(dest);
        }
        n += 1;
    }
}

/// Move a whole damaged file into quarantine and make the move durable.
fn quarantine(
    dirs: &StreamDirs,
    signal: Signal,
    kind: QuarantineKind,
    path: &Path,
    reason: String,
    report: &mut RecoveryReport,
) -> Result<()> {
    let source = path.file_name().and_then(|n| n.to_str()).unwrap_or("unknown").to_string();
    let bytes = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    let dest = quarantine_dest(dirs, &source)?;
    fsutil::rename(path, &dest)?;
    fsutil::sync_dir(&dirs.quarantine)?;
    if let Some(parent) = path.parent() {
        fsutil::sync_dir(parent)?;
    }
    tracing::error!(
        signal = signal.as_str(),
        kind = kind.as_str(),
        file = %source,
        quarantined_to = %dest.display(),
        bytes,
        reason = %reason,
        "moved damaged file to quarantine"
    );
    report.quarantined.push(QuarantinedFile { signal, kind, source, path: dest, bytes, reason });
    Ok(())
}

/// `Some(reason)` when an error proves a file's *content* is unusable, as
/// opposed to the file being unreadable right now. The reason omits the
/// path, which the report already names, so it is safe to show over the API.
fn damage_reason(e: &StorageError) -> Option<String> {
    match e {
        StorageError::Corrupt { source, .. } => Some(format!("corrupt: {source}")),
        StorageError::Compression(source) => Some(format!("compression: {source}")),
        StorageError::Io { source, .. } if source.kind() == std::io::ErrorKind::UnexpectedEof => {
            Some(format!("truncated: {source}"))
        }
        _ => None,
    }
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
        let reason = match Segment::open(&path) {
            Ok(s) if s.summary.id == id && s.summary.signal == signal => {
                segments.push(Arc::new(s));
                continue;
            }
            Ok(s) => format!(
                "file name says segment {id} of {}, but its summary says segment {} of {}",
                signal.as_str(),
                s.summary.id,
                s.summary.signal.as_str()
            ),
            Err(e) => match damage_reason(&e) {
                Some(reason) => reason,
                // Not proof of damage: refuse to start rather than hide data.
                None => return Err(e),
            },
        };
        quarantine(dirs, signal, QuarantineKind::Segment, &path, reason, &mut report)?;
        report.segments_quarantined += 1;
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
            Err(ReplayError::BadHeader) => {
                let reason = "WAL header is missing, truncated or has an unknown magic or version".to_string();
                quarantine(dirs, signal, QuarantineKind::Wal, &path, reason, &mut report)?;
                report.wal_files_quarantined += 1;
                continue;
            }
            // Not proof of damage: refuse to start rather than hide data.
            Err(ReplayError::Io(source)) => return Err(StorageError::Io { path, source }),
        };
        if rep.header != (WalHeader { signal, wal_id: id }) {
            let reason = format!(
                "file name says WAL {id} of {}, but its header says WAL {} of {}",
                signal.as_str(),
                rep.header.wal_id,
                rep.header.signal.as_str()
            );
            quarantine(dirs, signal, QuarantineKind::Wal, &path, reason, &mut report)?;
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
            preserve_tail(dirs, signal, &path, &rep, &mut report)?;
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
            crash::point("recovery.before_wal_delete");
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
/// so an operator can inspect what was lost. The copy is fsynced before the
/// caller truncates or deletes the WAL: the evidence must outlive the
/// original even if the machine dies during recovery.
fn preserve_tail(
    dirs: &StreamDirs,
    signal: Signal,
    path: &Path,
    rep: &WalReplay,
    report: &mut RecoveryReport,
) -> Result<()> {
    let bytes = std::fs::read(path).ctx(path)?;
    let valid_len = rep.valid_len as usize;
    if valid_len >= bytes.len() {
        return Ok(());
    }
    let tail = &bytes[valid_len..];
    let source = wal_file_name(rep.header.wal_id);
    let dest = quarantine_dest(dirs, &format!("{source}.tail-{valid_len}"))?;
    {
        let mut f = std::fs::File::create(&dest).ctx(&dest)?;
        f.write_all(tail).ctx(&dest)?;
        f.sync_all().ctx(&dest)?;
    }
    fsutil::sync_dir(&dirs.quarantine)?;
    let reason = match &rep.tail {
        TailState::Clean => "bytes after the last valid record".to_string(),
        TailState::Incomplete { offset } => format!("incomplete record at offset {offset} (torn write)"),
        TailState::Corrupt { offset, reason } => format!("corrupt record at offset {offset}: {reason}"),
    };
    tracing::warn!(
        signal = signal.as_str(),
        kind = QuarantineKind::WalTail.as_str(),
        file = %source,
        quarantined_to = %dest.display(),
        bytes = tail.len(),
        reason = %reason,
        "preserved unusable WAL tail in quarantine"
    );
    report.quarantined.push(QuarantinedFile {
        signal,
        kind: QuarantineKind::WalTail,
        source,
        path: dest,
        bytes: tail.len() as u64,
        reason,
    });
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::DecodeError;

    #[test]
    fn only_proof_of_damage_leads_to_quarantine() {
        let io = |kind: std::io::ErrorKind| StorageError::Io {
            path: "/data/segments/logs/x.seg".into(),
            source: kind.into(),
        };
        // Transient or environmental failures must stop startup instead:
        // quarantining a readable file would hide acknowledged telemetry.
        assert_eq!(damage_reason(&io(std::io::ErrorKind::PermissionDenied)), None);
        assert_eq!(damage_reason(&io(std::io::ErrorKind::Other)), None);
        assert_eq!(damage_reason(&StorageError::Unavailable("x".into())), None);

        let truncated = damage_reason(&io(std::io::ErrorKind::UnexpectedEof)).unwrap();
        assert!(truncated.starts_with("truncated"));
        let corrupt = StorageError::Corrupt { what: "/data/segments/logs/x.seg".into(), source: DecodeError::Checksum };
        let reason = damage_reason(&corrupt).unwrap();
        assert_eq!(reason, "corrupt: checksum mismatch");
        assert!(!truncated.contains("/data") && !reason.contains("/data"), "reasons never carry host paths");
    }
}
