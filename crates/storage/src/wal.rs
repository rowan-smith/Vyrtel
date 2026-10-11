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

use telemetry::Signal;

use crate::error::{IoContext, Result};
use crate::{fault, fsutil};

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
