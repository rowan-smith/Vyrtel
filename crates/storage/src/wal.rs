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
use crate::fsutil;

pub const WAL_MAGIC: &[u8; 8] = b"VYRTWAL1";
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
}

impl WalWriter {
    /// Create a new WAL file atomically: the header is written to a temp
    /// file, fsynced and renamed into place, so a crash can never leave a
    /// WAL with a torn header.
    pub fn create(dir: &Path, tmp_dir: &Path, header: WalHeader) -> Result<Self> {
        let path = dir.join(wal_file_name(header.wal_id));
        let tmp = tmp_dir.join(format!("{}.{}.tmp", header.signal.as_str(), wal_file_name(header.wal_id)));
        {
            let mut f = File::create(&tmp).ctx(&tmp)?;
            f.write_all(&encode_header(header)).ctx(&tmp)?;
            f.sync_all().ctx(&tmp)?;
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
        Ok(Self { file, path: path.to_path_buf(), id, len: valid_len, unsynced: 0, poisoned: false })
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
        if let Err(e) = self.file.write_all(&rec) {
            // Some bytes may have been written. Remember to cut them off so
            // the next record does not land after garbage.
            self.poisoned = true;
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

    /// fsync file data. In `strict` durability this runs before every
    /// acknowledgement; in `normal` mode it runs on a timer.
    pub fn sync(&mut self) -> Result<()> {
        if self.unsynced > 0 {
            self.file.sync_data().ctx(&self.path)?;
            self.unsynced = 0;
        }
        Ok(())
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
        if prefix[0] != RECORD_VERSION {
            break TailState::Corrupt { offset, reason: "unknown record version" };
        }
        let len = u32::from_le_bytes([prefix[1], prefix[2], prefix[3], prefix[4]]);
        if len > MAX_RECORD_LEN {
            break TailState::Corrupt { offset, reason: "record length out of range" };
        }
        // A length pointing past EOF is an incomplete record, not a reason
        // to allocate: check before reading.
        if offset + RECORD_OVERHEAD as u64 + len as u64 > file_len {
            break TailState::Incomplete { offset };
        }
        payload.resize(len as usize, 0);
        let got = read_full(&mut r, &mut payload).map_err(ReplayError::Io)?;
        let mut crc_bytes = [0u8; 4];
        let got_crc = read_full(&mut r, &mut crc_bytes).map_err(ReplayError::Io)?;
        if got < payload.len() || got_crc < 4 {
            break TailState::Incomplete { offset };
        }
        let mut h = crc32fast::Hasher::new();
        h.update(&prefix);
        h.update(&payload);
        if h.finalize() != u32::from_le_bytes(crc_bytes) {
            break TailState::Corrupt { offset, reason: "checksum mismatch" };
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
