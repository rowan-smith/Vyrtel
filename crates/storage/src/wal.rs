//! Append-only write-ahead log.
//!
//! ```text
//! file   := header record*
//! header := magic "VYRTWAL1" (8) | format u16 | signal u8 | reserved [u8; 5] | wal id u64
//! record := version u8 | length u32 | payload [length] | crc32 u32
//! ```
//!
//! The CRC covers version, length and payload. A record is only valid if all
//! of its bytes are present and the checksum matches. Replay stops at the
//! first record that is incomplete or corrupt: anything after it is never
//! interpreted, because a torn write may have left arbitrary bytes there and
//! we must not fabricate telemetry from them.
//!
//! Each record holds one encoded batch (see [`crate::codec`]), so a batch is
//! either entirely recovered or entirely absent — a request is never
//! half-applied.

use std::fs::{File, OpenOptions};
use std::io::{self, BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use telemetry::{Signal, TelemetryEvent};

use crate::encoding::Writer;
use crate::error::{IoContext, Result};
use crate::{codec, fault, fsutil};

pub const WAL_MAGIC: &[u8; 8] = b"VYRTWAL1";
/// The WAL magic of Observer 0.1.0, Vyrtel's first release under its
/// earlier name. Otherwise the same layout; not read by this build.
pub const OBSERVER_WAL_MAGIC: &[u8; 8] = b"OBSVWAL1";
pub const WAL_FORMAT_VERSION: u16 = 1;
pub const RECORD_VERSION: u8 = 1;
pub const HEADER_LEN: u64 = 24;
const RECORD_OVERHEAD: usize = 1 + 4 + 4;
/// Upper bound for one record. Anything larger is treated as corruption
/// rather than an instruction to allocate gigabytes.
pub const MAX_RECORD_LEN: u32 = 256 * 1024 * 1024;
/// A record is written in pieces of about this size as it is encoded.
const WRITE_CHUNK: usize = 64 << 10;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WalHeader {
    pub signal: Signal,
    pub wal_id: u64,
}

fn encode_header(h: WalHeader) -> [u8; HEADER_LEN as usize] {
    let mut b = [0u8; HEADER_LEN as usize];
    b[..8].copy_from_slice(WAL_MAGIC);
    b[8..10].copy_from_slice(&WAL_FORMAT_VERSION.to_le_bytes());
    b[10] = h.signal.code();
    b[16..24].copy_from_slice(&h.wal_id.to_le_bytes());
    b
}

/// The format version a WAL header declares, if `b` starts with the WAL
/// magic. Lets callers tell a WAL from a newer Vyrtel from a damaged one.
pub fn header_version(b: &[u8]) -> Option<u16> {
    (b.len() >= 10 && &b[..8] == WAL_MAGIC).then(|| u16::from_le_bytes([b[8], b[9]]))
}

/// Fail with [`StorageError::UnsupportedFormat`](crate::StorageError) if
/// the WAL at `path` declares a file format this build cannot read. Reads
/// only the header; a missing or damaged header passes (replay judges it).
pub fn check_wal_format(path: &Path) -> Result<()> {
    let mut hb = Vec::with_capacity(HEADER_LEN as usize);
    File::open(path).and_then(|f| f.take(HEADER_LEN).read_to_end(&mut hb)).ctx(path)?;
    if hb.starts_with(OBSERVER_WAL_MAGIC) {
        return Err(crate::StorageError::ObserverFormat { path: path.to_path_buf() });
    }
    match header_version(&hb) {
        Some(found) if found != WAL_FORMAT_VERSION => Err(crate::StorageError::UnsupportedFormat {
            path: path.to_path_buf(),
            what: "WAL file format",
            found,
            supported: WAL_FORMAT_VERSION,
        }),
        _ => Ok(()),
    }
}

fn decode_header(b: &[u8]) -> Option<WalHeader> {
    if b.len() < HEADER_LEN as usize || &b[..8] != WAL_MAGIC {
        return None;
    }
    let version = u16::from_le_bytes([b[8], b[9]]);
    if version != WAL_FORMAT_VERSION {
        return None;
    }
    Some(WalHeader { signal: Signal::from_code(b[10])?, wal_id: u64::from_le_bytes(b[16..24].try_into().ok()?) })
}

pub fn encode_record(payload: &[u8]) -> Vec<u8> {
    let mut rec = Vec::with_capacity(payload.len() + RECORD_OVERHEAD);
    rec.push(RECORD_VERSION);
    rec.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    rec.extend_from_slice(payload);
    let crc = crc32fast::hash(&rec);
    rec.extend_from_slice(&crc.to_le_bytes());
    rec
}

/// A record on its way to the file: hashed as it is produced, written in
/// [`WRITE_CHUNK`] pieces. With an injected fault, only `budget` bytes reach
/// the file before the fault's error is returned.
struct RecordOut<'f> {
    file: &'f mut File,
    buf: Vec<u8>,
    crc: crc32fast::Hasher,
    budget: usize,
    fault: Option<io::Error>,
}

impl RecordOut<'_> {
    fn put(&mut self, b: &[u8]) -> io::Result<()> {
        self.crc.update(b);
        self.buf.extend_from_slice(b);
        if self.buf.len() >= WRITE_CHUNK { self.flush() } else { Ok(()) }
    }

    fn flush(&mut self) -> io::Result<()> {
        let n = self.buf.len().min(self.budget);
        self.file.write_all(&self.buf[..n])?;
        self.budget -= n;
        if n < self.buf.len() {
            return Err(self.fault.take().unwrap_or_else(|| io::Error::other("record longer than announced")));
        }
        self.buf.clear();
        Ok(())
    }
}

pub fn wal_file_name(id: u64) -> String {
    format!("{id:012}.wal")
}

pub fn parse_wal_file_name(name: &str) -> Option<u64> {
    let stem = name.strip_suffix(".wal")?;
    if stem.len() != 12 || !stem.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    stem.parse().ok()
}

/// Writer for the active WAL file. Owned by a single writer thread.
pub struct WalWriter {
    file: File,
    path: PathBuf,
    id: u64,
    /// Length of the file up to the end of the last fully written record.
    len: u64,
    /// Bytes written since the last fsync.
    unsynced: u64,
    /// Set if a write failed half-way; further appends are refused until
    /// the file has been truncated back to `len`.
    poisoned: bool,
    /// Complete records were cut off by [`WalWriter::truncate_to`] and the
    /// new length is not yet known to be on disk: after a power loss they
    /// could come back. The next [`WalWriter::sync`] persists it.
    rollback_unsynced: bool,
}

impl WalWriter {
    /// Create a new WAL file atomically: the header is written to a temp
    /// file, fsynced and renamed into place, so a crash can never leave a
    /// WAL with a torn header.
    pub fn create(dir: &Path, tmp_dir: &Path, header: WalHeader) -> Result<Self> {
        let path = dir.join(wal_file_name(header.wal_id));
        let tmp = tmp_dir.join(format!("{}.{}.tmp", header.signal.as_str(), wal_file_name(header.wal_id)));
        let written = (|| {
            fault::check("wal.create", &tmp).ctx(&tmp)?;
            let mut f = File::create(&tmp).ctx(&tmp)?;
            f.write_all(&encode_header(header)).ctx(&tmp)?;
            f.sync_all().ctx(&tmp)
        })();
        if let Err(e) = written {
            // Nothing refers to the temp file; don't leave it filling the disk.
            let _ = std::fs::remove_file(&tmp);
            return Err(e);
        }
        fsutil::rename(&tmp, &path)?;
        fsutil::sync_dir(dir)?;
        Self::open_append(&path, header.wal_id, HEADER_LEN)
    }

    /// Re-open an existing WAL for appending after recovery. `valid_len` is
    /// the end of the last valid record; anything beyond it is discarded.
    pub fn open_append(path: &Path, id: u64, valid_len: u64) -> Result<Self> {
        let mut file = OpenOptions::new().read(true).write(true).open(path).ctx(path)?;
        let actual = file.metadata().ctx(path)?.len();
        if actual != valid_len {
            file.set_len(valid_len).ctx(path)?;
            file.sync_all().ctx(path)?;
        }
        file.seek(SeekFrom::Start(valid_len)).ctx(path)?;
        Ok(Self {
            file,
            path: path.to_path_buf(),
            id,
            len: valid_len,
            unsynced: 0,
            poisoned: false,
            rollback_unsynced: false,
        })
    }

    pub fn id(&self) -> u64 {
        self.id
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn len(&self) -> u64 {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len <= HEADER_LEN
    }

    pub fn unsynced_bytes(&self) -> u64 {
        self.unsynced
    }

    /// Append one record. The data reaches the OS page cache; call
    /// [`WalWriter::sync`] for durability against power loss.
    pub fn append(&mut self, payload: &[u8]) -> Result<()> {
        if self.poisoned {
            self.recover_from_partial_write()?;
        }
        if payload.len() > MAX_RECORD_LEN as usize {
            return Err(crate::StorageError::BatchTooLarge(payload.len()));
        }
        let rec = encode_record(payload);
        let result = match fault::check_write("wal.append", &self.path, rec.len()) {
            Some((n, e)) => self.file.write_all(&rec[..n]).and(Err(e)),
            None => self.file.write_all(&rec),
        };
        if let Err(e) = result {
            // Some bytes may have been written. Cut them off now so a
            // restart does not find a torn record; if that fails too, the
            // next append retries before writing anything.
            self.poisoned = true;
            let _ = self.recover_from_partial_write();
            return Err(e).ctx(&self.path);
        }
        self.len += rec.len() as u64;
        self.unsynced += rec.len() as u64;
        Ok(())
    }

    /// Append one batch as a record, encoding it as it is written. The
    /// bytes are exactly those of `append(&codec::encode_batch(events))`,
    /// but neither the batch nor the record is ever held whole: only its
    /// resource table, one event's encoding and a [`WRITE_CHUNK`] buffer.
    /// The length is taken by a first encoding pass, so
    /// a batch over [`MAX_RECORD_LEN`] is refused before anything is
    /// written. A failure part-way is handled like a failed `append`.
    pub fn append_batch(&mut self, events: &[TelemetryEvent]) -> Result<()> {
        if self.poisoned {
            self.recover_from_partial_write()?;
        }
        let plan = codec::plan_batch(events);
        let mut scratch = Writer::new();
        let len = codec::encoded_batch_len(events, &plan, &mut scratch);
        if len > MAX_RECORD_LEN as usize {
            return Err(crate::StorageError::BatchTooLarge(len));
        }
        let total = len + RECORD_OVERHEAD;
        let mut out = RecordOut {
            file: &mut self.file,
            buf: Vec::with_capacity(WRITE_CHUNK.min(total)),
            crc: crc32fast::Hasher::new(),
            budget: total,
            fault: None,
        };
        if let Some((n, e)) = fault::check_write("wal.append", &self.path, total) {
            (out.budget, out.fault) = (n, Some(e));
        }
        let result = (|| {
            out.put(&[RECORD_VERSION])?;
            out.put(&(len as u32).to_le_bytes())?;
            codec::encode_batch_with(events, &plan, &mut scratch, |b| out.put(b))?;
            let crc = out.crc.clone().finalize();
            out.buf.extend_from_slice(&crc.to_le_bytes());
            out.flush()
        })();
        if let Err(e) = result {
            // As in `append`: cut off whatever was written.
            self.poisoned = true;
            let _ = self.recover_from_partial_write();
            return Err(e).ctx(&self.path);
        }
        self.len += total as u64;
        self.unsynced += total as u64;
        Ok(())
    }

    fn recover_from_partial_write(&mut self) -> Result<()> {
        self.file.set_len(self.len).ctx(&self.path)?;
        self.file.seek(SeekFrom::Start(self.len)).ctx(&self.path)?;
        self.poisoned = false;
        Ok(())
    }

    /// Cut the file back to `len`, dropping complete records appended
    /// after it (records whose fsync failed, so they were never
    /// acknowledged), and fsync the new length. `Ok` means the rollback is
    /// durable: the records cannot come back, even after a power loss. On
    /// `Err` the rollback is still pending: the next append or
    /// [`WalWriter::sync`] retries it, and nothing written after it can
    /// become durable before it does.
    pub fn truncate_to(&mut self, len: u64) -> Result<()> {
        debug_assert!((HEADER_LEN..=self.len).contains(&len));
        self.unsynced = self.unsynced.saturating_sub(self.len - len);
        self.len = len;
        self.poisoned = true;
        self.rollback_unsynced = true;
        self.sync()
    }

    /// A rollback by [`WalWriter::truncate_to`] is not yet durable.
    pub fn rollback_pending(&self) -> bool {
        self.rollback_unsynced
    }

    /// Finish a pending truncation (see [`WalWriter::truncate_to`]) so a
    /// restart does not find a partial record. Used at shutdown.
    pub fn discard_partial_write(&mut self) -> Result<()> {
        if self.poisoned { self.recover_from_partial_write() } else { Ok(()) }
    }

    /// fsync file data. In `strict` durability this runs before every
    /// acknowledgement; in `normal` mode it runs on a timer. A pending
    /// truncation is finished first, so cut-off records are never made
    /// durable, and is persisted with a full `sync_all` (the file size is
    /// metadata).
    pub fn sync(&mut self) -> Result<()> {
        if self.poisoned {
            self.recover_from_partial_write()?;
        }
        if self.unsynced > 0 || self.rollback_unsynced {
            fault::check("wal.sync", &self.path).ctx(&self.path)?;
            if self.rollback_unsynced {
                self.file.sync_all().ctx(&self.path)?;
            } else {
                self.file.sync_data().ctx(&self.path)?;
            }
            self.unsynced = 0;
            self.rollback_unsynced = false;
        }
        Ok(())
    }

    /// fsync unconditionally, even with nothing new written. Used to check
    /// whether a failing WAL works again while no requests arrive.
    pub fn sync_now(&mut self) -> Result<()> {
        self.rollback_unsynced = true;
        self.sync()
    }
}

/// Why replay stopped before the end of the file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TailState {
    /// Every byte belonged to a valid record.
    Clean,
    /// The final record was cut short (typical after a crash mid-write).
    Incomplete { offset: u64 },
    /// A record's checksum or framing was wrong.
    Corrupt { offset: u64, reason: &'static str },
}

#[derive(Debug)]
pub struct WalReplay {
    pub header: WalHeader,
    pub records: usize,
    /// Offset just past the last valid record.
    pub valid_len: u64,
    pub file_len: u64,
    pub tail: TailState,
}

/// Errors that make a whole WAL file unusable (as opposed to a bad tail).
#[derive(Debug)]
pub enum ReplayError {
    Io(io::Error),
    BadHeader,
    /// Observer 0.1.0 wrote this WAL ([`OBSERVER_WAL_MAGIC`]). Not damage.
    Observer,
    /// The header or a checksummed record declares a version this build
    /// cannot read (`what` names which). Not damage: nothing is replayed.
    Unsupported {
        what: &'static str,
        found: u16,
        supported: u16,
    },
}

/// Read every valid record, calling `on_record` with each payload in order.
/// Stops at the first incomplete or corrupt record.
pub fn replay(
    path: &Path,
    mut on_record: impl FnMut(&[u8]) -> std::result::Result<(), &'static str>,
) -> std::result::Result<WalReplay, ReplayError> {
    let file = File::open(path).map_err(ReplayError::Io)?;
    let file_len = file.metadata().map_err(ReplayError::Io)?.len();
    let mut r = BufReader::with_capacity(1 << 20, file);

    let mut hb = [0u8; HEADER_LEN as usize];
    if read_full(&mut r, &mut hb).map_err(ReplayError::Io)? < hb.len() {
        return Err(ReplayError::BadHeader);
    }
    if hb.starts_with(OBSERVER_WAL_MAGIC) {
        return Err(ReplayError::Observer);
    }
    if let Some(found) = header_version(&hb)
        && found != WAL_FORMAT_VERSION
    {
        return Err(ReplayError::Unsupported { what: "WAL file format", found, supported: WAL_FORMAT_VERSION });
    }
    let header = decode_header(&hb).ok_or(ReplayError::BadHeader)?;

    let mut offset = HEADER_LEN;
    let mut records = 0usize;
    let mut payload = Vec::new();
    let tail = loop {
        let mut prefix = [0u8; 5];
        let n = read_full(&mut r, &mut prefix).map_err(ReplayError::Io)?;
        if n == 0 {
            break TailState::Clean;
        }
        if n < prefix.len() {
            break TailState::Incomplete { offset };
        }
        // An unknown version is only believed if the record's checksum
        // holds; otherwise it is a torn or corrupt record like any other.
        let known = prefix[0] == RECORD_VERSION;
        let unknown = TailState::Corrupt { offset, reason: "unknown record version" };
        let len = u32::from_le_bytes([prefix[1], prefix[2], prefix[3], prefix[4]]);
        if len > MAX_RECORD_LEN {
            break if known { TailState::Corrupt { offset, reason: "record length out of range" } } else { unknown };
        }
        // A length pointing past EOF is an incomplete record, not a reason
        // to allocate: check before reading.
        if offset + RECORD_OVERHEAD as u64 + len as u64 > file_len {
            break if known { TailState::Incomplete { offset } } else { unknown };
        }
        payload.resize(len as usize, 0);
        let got = read_full(&mut r, &mut payload).map_err(ReplayError::Io)?;
        let mut crc_bytes = [0u8; 4];
        let got_crc = read_full(&mut r, &mut crc_bytes).map_err(ReplayError::Io)?;
        if got < payload.len() || got_crc < 4 {
            break if known { TailState::Incomplete { offset } } else { unknown };
        }
        let mut h = crc32fast::Hasher::new();
        h.update(&prefix);
        h.update(&payload);
        if h.finalize() != u32::from_le_bytes(crc_bytes) {
            break if known { TailState::Corrupt { offset, reason: "checksum mismatch" } } else { unknown };
        }
        if !known {
            return Err(ReplayError::Unsupported {
                what: "WAL record format",
                found: prefix[0].into(),
                supported: RECORD_VERSION.into(),
            });
        }
        if let Err(reason) = on_record(&payload) {
            // Checksum was fine but the payload does not decode: treat as
            // corruption and stop, never guess.
            break TailState::Corrupt { offset, reason };
        }
        records += 1;
        offset += (RECORD_OVERHEAD + payload.len()) as u64;
    };

    Ok(WalReplay { header, records, valid_len: offset, file_len, tail })
}

/// Find a record, or a batch inside one, in a format this build cannot
/// read, as [`replay`] would, without replaying: for each record only its
/// 5-byte prefix and the first payload byte (the batch's codec version) are
/// read, and the rest is skipped. Only when one of those versions is unknown
/// is the record's checksum computed, streaming the payload in 64 KiB
/// pieces, because an unknown version is believed only if it holds. Memory
/// stays bounded whatever the record sizes, and no event is decoded.
///
/// Damage is not reported, here or after it: a bad header passes, and the
/// scan stops at a torn or out-of-range record. A known record whose
/// checksum would fail is skipped by its length without being verified, so
/// the scan may look further than replay, which stops there; it can then
/// only refuse more, never less.
pub fn check_record_formats(path: &Path, codec_version: u8) -> std::result::Result<(), ReplayError> {
    const CHUNK: usize = 64 * 1024;
    let file = File::open(path).map_err(ReplayError::Io)?;
    let file_len = file.metadata().map_err(ReplayError::Io)?.len();
    let mut r = BufReader::with_capacity(CHUNK, file);
    let mut hb = [0u8; HEADER_LEN as usize];
    if read_full(&mut r, &mut hb).map_err(ReplayError::Io)? < hb.len() {
        return Ok(());
    }
    if hb.starts_with(OBSERVER_WAL_MAGIC) {
        return Err(ReplayError::Observer);
    }
    if let Some(found) = header_version(&hb)
        && found != WAL_FORMAT_VERSION
    {
        return Err(ReplayError::Unsupported { what: "WAL file format", found, supported: WAL_FORMAT_VERSION });
    }
    if decode_header(&hb).is_none() {
        return Ok(());
    }
    let mut offset = HEADER_LEN;
    let mut buf = Vec::new();
    loop {
        let mut prefix = [0u8; 5];
        if read_full(&mut r, &mut prefix).map_err(ReplayError::Io)? < prefix.len() {
            return Ok(());
        }
        let len = u32::from_le_bytes([prefix[1], prefix[2], prefix[3], prefix[4]]);
        if len > MAX_RECORD_LEN || offset + RECORD_OVERHEAD as u64 + len as u64 > file_len {
            return Ok(());
        }
        let mut first = [0u8; 1];
        let got_first = if len > 0 { read_full(&mut r, &mut first).map_err(ReplayError::Io)? } else { 0 };
        let known = prefix[0] == RECORD_VERSION;
        if known && got_first == 1 && (1..=codec_version).contains(&first[0]) {
            r.seek_relative(len as i64 - 1 + 4).map_err(ReplayError::Io)?;
            offset += (RECORD_OVERHEAD + len as usize) as u64;
            continue;
        }
        // Something unknown (or an empty batch): believe it only if the
        // record's checksum holds.
        let mut h = crc32fast::Hasher::new();
        h.update(&prefix);
        h.update(&first[..got_first]);
        let mut left = len as usize - got_first;
        buf.resize(CHUNK.min(left), 0);
        while left > 0 {
            let n = buf.len().min(left);
            if read_full(&mut r, &mut buf[..n]).map_err(ReplayError::Io)? < n {
                return Ok(());
            }
            h.update(&buf[..n]);
            left -= n;
        }
        let mut crc = [0u8; 4];
        if read_full(&mut r, &mut crc).map_err(ReplayError::Io)? < 4 || h.finalize() != u32::from_le_bytes(crc) {
            return Ok(());
        }
        return if !known {
            Err(ReplayError::Unsupported {
                what: "WAL record format",
                found: prefix[0].into(),
                supported: RECORD_VERSION.into(),
            })
        } else if got_first == 1 {
            Err(ReplayError::Unsupported {
                what: "event codec",
                found: first[0].into(),
                supported: codec_version.into(),
            })
        } else {
            // An empty batch does not decode: replay stops here.
            Ok(())
        };
    }
}

fn read_full(r: &mut impl Read, buf: &mut [u8]) -> io::Result<usize> {
    let mut total = 0;
    while total < buf.len() {
        match r.read(&mut buf[total..]) {
            Ok(0) => break,
            Ok(n) => total += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn setup() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let d = tempfile::tempdir().unwrap();
        let wal = d.path().join("wal");
        let tmp = d.path().join("tmp");
        std::fs::create_dir_all(&wal).unwrap();
        std::fs::create_dir_all(&tmp).unwrap();
        (d, wal, tmp)
    }

    fn header(id: u64) -> WalHeader {
        WalHeader { signal: Signal::Logs, wal_id: id }
    }

    fn collect(path: &Path) -> (Vec<Vec<u8>>, WalReplay) {
        let mut out = Vec::new();
        let rep = replay(path, |p| {
            out.push(p.to_vec());
            Ok(())
        })
        .unwrap();
        (out, rep)
    }

    /// Events with a 100-attribute resource: one shared by all of them, or
    /// a different one each.
    fn resource_events(n: usize, shared: bool) -> Vec<TelemetryEvent> {
        use std::sync::Arc;
        use telemetry::*;
        let resource = |i: usize| -> Arc<Fields> {
            Arc::new((0..100).map(|k| (format!("attr.{k}"), Value::from(format!("{i:024}")))).collect())
        };
        let one = resource(0);
        (0..n)
            .map(|i| TelemetryEvent {
                id: EventId(i as u64 + 1),
                timestamp: Timestamp(i as i64),
                observed_timestamp: Timestamp(i as i64),
                service: Some("api".into()),
                environment: None,
                trace_id: None,
                span_id: None,
                resource: if shared { one.clone() } else { resource(i) },
                attributes: Fields::new(),
                payload: TelemetryPayload::Log(LogEvent {
                    level: Level::Information,
                    message: format!("event {i}"),
                    message_template: None,
                    exception: None,
                }),
            })
            .collect()
    }

    // Streaming a batch into the WAL writes byte for byte what encoding it
    // whole did, over any number of write chunks, and replays to the same
    // events.
    #[test]
    fn append_batch_writes_the_same_bytes_as_append() {
        for (n, shared) in [(0, true), (1, true), (7, false), (200, true), (200, false)] {
            let events = resource_events(n, shared);
            let (_d, dir, tmp) = setup();
            let mut whole = WalWriter::create(&dir, &tmp, header(1)).unwrap();
            whole.append(&codec::encode_batch(&events)).unwrap();
            let mut streamed = WalWriter::create(&dir, &tmp, header(2)).unwrap();
            streamed.append_batch(&events).unwrap();
            streamed.append_batch(&events).unwrap();
            let (a, b) = (std::fs::read(whole.path()).unwrap(), std::fs::read(streamed.path()).unwrap());
            let record = &a[HEADER_LEN as usize..];
            assert_eq!(&b[HEADER_LEN as usize..], [record, record].concat(), "{n} events");
            assert_eq!(streamed.len(), HEADER_LEN + 2 * record.len() as u64);
            if n == 200 && !shared {
                assert!(record.len() > 4 * WRITE_CHUNK, "spans several chunks: {}", record.len());
            }
            let (payloads, _) = collect(streamed.path());
            assert_eq!(payloads.len(), 2);
            assert_eq!(codec::decode_batch(&payloads[1]).unwrap(), events);
        }
    }

    #[test]
    fn encoded_batch_len_matches_the_encoding() {
        let events = resource_events(50, false);
        let plan = codec::plan_batch(&events);
        assert_eq!(codec::encoded_batch_len(&events, &plan, &mut Writer::new()), codec::encode_batch(&events).len());
    }

    #[test]
    fn empty_wal_replays_nothing() {
        let (_d, wal, tmp) = setup();
        let w = WalWriter::create(&wal, &tmp, header(1)).unwrap();
        let (recs, rep) = collect(w.path());
        assert!(recs.is_empty());
        assert_eq!(rep.tail, TailState::Clean);
        assert_eq!(rep.header, header(1));
        assert_eq!(rep.valid_len, HEADER_LEN);
    }

    #[test]
    fn valid_records_replay_in_order() {
        let (_d, wal, tmp) = setup();
        let mut w = WalWriter::create(&wal, &tmp, header(3)).unwrap();
        for i in 0..10u8 {
            w.append(&vec![i; i as usize * 10]).unwrap();
        }
        w.sync().unwrap();
        let (recs, rep) = collect(w.path());
        assert_eq!(recs.len(), 10);
        for (i, r) in recs.iter().enumerate() {
            assert_eq!(r, &vec![i as u8; i * 10]);
        }
        assert_eq!(rep.tail, TailState::Clean);
        assert_eq!(rep.valid_len, w.len());
    }

    #[test]
    fn truncated_final_record_is_dropped() {
        let (_d, wal, tmp) = setup();
        let mut w = WalWriter::create(&wal, &tmp, header(1)).unwrap();
        w.append(b"first").unwrap();
        let good = w.len();
        w.append(b"second record").unwrap();
        let path = w.path().to_path_buf();
        drop(w);
        // Chop the last record in half, as a crash mid-write would.
        let f = OpenOptions::new().write(true).open(&path).unwrap();
        f.set_len(good + 7).unwrap();
        let (recs, rep) = collect(&path);
        assert_eq!(recs, vec![b"first".to_vec()]);
        assert_eq!(rep.valid_len, good);
        assert_eq!(rep.tail, TailState::Incomplete { offset: good });
    }

    #[test]
    fn checksum_mismatch_stops_replay() {
        let (_d, wal, tmp) = setup();
        let mut w = WalWriter::create(&wal, &tmp, header(1)).unwrap();
        w.append(b"aaaa").unwrap();
        let first_end = w.len();
        w.append(b"bbbb").unwrap();
        w.append(b"cccc").unwrap();
        let path = w.path().to_path_buf();
        drop(w);
        let mut bytes = std::fs::read(&path).unwrap();
        // Flip a payload bit in the second record.
        bytes[first_end as usize + 6] ^= 0x01;
        std::fs::write(&path, &bytes).unwrap();
        let (recs, rep) = collect(&path);
        assert_eq!(recs, vec![b"aaaa".to_vec()]);
        assert!(matches!(rep.tail, TailState::Corrupt { offset, .. } if offset == first_end));
    }

    #[test]
    fn huge_length_is_not_allocated() {
        let (_d, wal, tmp) = setup();
        let w = WalWriter::create(&wal, &tmp, header(1)).unwrap();
        let path = w.path().to_path_buf();
        drop(w);
        let mut f = OpenOptions::new().append(true).open(&path).unwrap();
        f.write_all(&[RECORD_VERSION, 0xff, 0xff, 0xff, 0xff]).unwrap();
        let (recs, rep) = collect(&path);
        assert!(recs.is_empty());
        assert!(matches!(rep.tail, TailState::Corrupt { .. }));
    }

    #[test]
    fn bad_header_rejected() {
        let (_d, wal, _tmp) = setup();
        let path = wal.join(wal_file_name(1));
        std::fs::write(&path, b"not a wal at all, definitely").unwrap();
        assert!(matches!(replay(&path, |_| Ok(())), Err(ReplayError::BadHeader)));
        std::fs::write(&path, b"short").unwrap();
        assert!(matches!(replay(&path, |_| Ok(())), Err(ReplayError::BadHeader)));
        check_wal_format(&path).unwrap();
    }

    #[test]
    fn newer_header_version_is_unsupported_not_bad() {
        let (_d, wal, tmp) = setup();
        let path = WalWriter::create(&wal, &tmp, header(1)).unwrap().path().to_path_buf();
        check_wal_format(&path).unwrap();
        let mut b = std::fs::read(&path).unwrap();
        b[8..10].copy_from_slice(&2u16.to_le_bytes());
        std::fs::write(&path, &b).unwrap();
        assert!(matches!(
            replay(&path, |_| Ok(())),
            Err(ReplayError::Unsupported { what: "WAL file format", found: 2, supported: 1 })
        ));
        assert!(matches!(
            check_wal_format(&path),
            Err(crate::StorageError::UnsupportedFormat { found: 2, supported: 1, .. })
        ));
    }

    #[test]
    fn reopen_truncates_garbage_and_appends() {
        let (_d, wal, tmp) = setup();
        let mut w = WalWriter::create(&wal, &tmp, header(1)).unwrap();
        w.append(b"one").unwrap();
        let path = w.path().to_path_buf();
        drop(w);
        let mut f = OpenOptions::new().append(true).open(&path).unwrap();
        f.write_all(&[RECORD_VERSION, 9, 0]).unwrap();
        drop(f);
        let (_, rep) = collect(&path);
        let mut w = WalWriter::open_append(&path, 1, rep.valid_len).unwrap();
        w.append(b"two").unwrap();
        drop(w);
        let (recs, rep) = collect(&path);
        assert_eq!(recs, vec![b"one".to_vec(), b"two".to_vec()]);
        assert_eq!(rep.tail, TailState::Clean);
    }

    #[test]
    fn truncate_to_drops_unacknowledged_records() {
        let (_d, wal, tmp) = setup();
        let mut w = WalWriter::create(&wal, &tmp, header(1)).unwrap();
        w.append(b"acked").unwrap();
        w.sync().unwrap();
        let acked = w.len();
        w.append(b"fsync failed for this one").unwrap();
        w.truncate_to(acked).unwrap();
        assert_eq!(w.len(), acked);
        assert_eq!(w.unsynced_bytes(), 0);
        assert!(!w.rollback_pending(), "truncate_to syncs the new length");
        assert_eq!(std::fs::metadata(w.path()).unwrap().len(), acked);
        w.append(b"next").unwrap();
        let path = w.path().to_path_buf();
        drop(w);
        let (recs, rep) = collect(&path);
        assert_eq!(recs, vec![b"acked".to_vec(), b"next".to_vec()]);
        assert_eq!(rep.tail, TailState::Clean);
    }

    /// A record with any version byte and payload, and a valid checksum.
    fn raw_record(version: u8, payload: &[u8]) -> Vec<u8> {
        let mut rec = vec![version];
        rec.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        rec.extend_from_slice(payload);
        let crc = crc32fast::hash(&rec);
        rec.extend_from_slice(&crc.to_le_bytes());
        rec
    }

    #[test]
    fn record_format_scan_reads_prefixes_and_believes_only_checksums() {
        let (_d, wal, tmp) = setup();
        let mut w = WalWriter::create(&wal, &tmp, header(1)).unwrap();
        // Known records, one much larger than the scan's 64 KiB buffer.
        w.append(&[1, 0]).unwrap();
        let mut big = vec![7u8; 3 << 20];
        big[0] = 1;
        w.append(&big).unwrap();
        let path = w.path().to_path_buf();
        drop(w);
        let scan = || check_record_formats(&path, 1);
        assert!(scan().is_ok());
        let original = std::fs::read(&path).unwrap();
        let with = |tail: &[u8]| std::fs::write(&path, [original.as_slice(), tail].concat()).unwrap();

        // A large record in a newer format: its checksum is streamed and holds.
        let mut newer = vec![9u8; 200_000];
        newer[0] = 1;
        with(&raw_record(2, &newer));
        assert!(matches!(scan(), Err(ReplayError::Unsupported { what: "WAL record format", found: 2, .. })));
        // The same with one flipped byte is a corrupt tail, not a newer format.
        let mut bad = raw_record(2, &newer);
        bad[100_000] ^= 1;
        with(&bad);
        assert!(scan().is_ok());
        // A newer batch codec in a known record.
        with(&raw_record(RECORD_VERSION, &[5, 0]));
        assert!(matches!(scan(), Err(ReplayError::Unsupported { what: "event codec", found: 5, supported: 1 })));
        let mut bad = raw_record(RECORD_VERSION, &[5, 0]);
        bad[6] ^= 1;
        with(&bad);
        assert!(scan().is_ok());
        // Torn, empty, out-of-range and bad-header tails pass (replay judges them).
        with(&raw_record(2, &[1, 0])[..6]);
        assert!(scan().is_ok());
        with(&raw_record(RECORD_VERSION, &[]));
        assert!(scan().is_ok());
        with(&[2, 0xff, 0xff, 0xff, 0xff]);
        assert!(scan().is_ok());
        std::fs::write(&path, b"not a wal at all, long enough for a header").unwrap();
        assert!(scan().is_ok());
    }

    #[test]
    fn file_names() {
        assert_eq!(wal_file_name(42), "000000000042.wal");
        assert_eq!(parse_wal_file_name("000000000042.wal"), Some(42));
        assert_eq!(parse_wal_file_name("42.wal"), None);
        assert_eq!(parse_wal_file_name("000000000042.wal.tmp"), None);
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(64))]

        /// random WAL records → replay → identical committed records, and a
        /// cut anywhere yields exactly the records fully before the cut.
        #[test]
        fn random_records_and_cuts(
            records in proptest::collection::vec(proptest::collection::vec(any::<u8>(), 0..300), 0..20),
            cut in any::<prop::sample::Index>(),
        ) {
            let (_d, wal, tmp) = setup();
            let mut w = WalWriter::create(&wal, &tmp, header(1)).unwrap();
            let mut ends = Vec::new();
            for r in &records {
                w.append(r).unwrap();
                ends.push(w.len());
            }
            let path = w.path().to_path_buf();
            let total = w.len();
            drop(w);

            let (recs, rep) = collect(&path);
            prop_assert_eq!(&recs, &records);
            prop_assert_eq!(rep.tail, TailState::Clean);

            let at = HEADER_LEN + cut.index((total - HEADER_LEN + 1) as usize) as u64;
            let f = OpenOptions::new().write(true).open(&path).unwrap();
            f.set_len(at).unwrap();
            drop(f);
            let (recs, rep) = collect(&path);
            let expected = ends.iter().filter(|e| **e <= at).count();
            prop_assert_eq!(recs.len(), expected);
            prop_assert_eq!(&recs[..], &records[..expected]);
            prop_assert!(rep.valid_len <= at);
        }
    }
}
