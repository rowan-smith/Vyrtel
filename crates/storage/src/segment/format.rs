//! Segment file layout (format version 1).
//!
//! ```text
//! ┌──────────────────────────────┐ 0
//! │ Header (32 bytes)            │ magic, format, signal, segment id, created
//! ├──────────────────────────────┤
//! │ Block 0 column chunks        │ ts | ids | meta | trace | msg | body
//! │ Block 1 column chunks        │ each chunk independently zstd-compressed
//! │ ...                          │
//! ├──────────────────────────────┤
//! │ Index region (zstd)          │ per-block Bloom filters + zone maps,
//! │                              │ per-field statistics (loaded lazily)
//! ├──────────────────────────────┤
//! │ Summary region (zstd)        │ time range, counts, dictionaries,
//! │                              │ bitmap indexes, block directory
//! ├──────────────────────────────┤
//! │ Footer (56 bytes)            │ region offsets + CRCs, magic
//! └──────────────────────────────┘
//! ```
//!
//! Every chunk records its CRC32 in the block directory; the summary and
//! index regions have CRCs in the footer, and the footer has its own CRC.

use telemetry::{Signal, Timestamp};

use crate::encoding::{Reader, Writer};
use crate::error::DecodeError;
use crate::index::{Bitmap, Bloom, Hll};

pub const SEGMENT_MAGIC: &[u8; 8] = b"OBSVSEG1";
pub const FOOTER_MAGIC: &[u8; 8] = b"OBSVSEND";
pub const FORMAT_VERSION: u16 = 1;
pub const HEADER_LEN: usize = 32;
pub const FOOTER_LEN: usize = 56;
/// Hard limit on a decompressed chunk; anything larger is corruption.
pub const MAX_CHUNK_RAW: u32 = 512 * 1024 * 1024;

/// Column chunks stored per block, in file order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Column {
    /// Timestamps: first value as i64, then zigzag varint deltas.
    Ts = 0,
    /// Event ids: zigzag varint deltas.
    Ids = 1,
    /// Per event: kind u8, level u8, service dict ref, environment dict ref.
    Meta = 2,
    /// Per event: optional trace id, optional span id.
    Trace = 3,
    /// Per event: message / span name / metric name.
    Msg = 4,
    /// Per event: length-prefixed codec body (attributes, resource, ...).
    Body = 5,
}

pub const NUM_COLUMNS: usize = 6;
pub const ALL_COLUMNS: [Column; NUM_COLUMNS] =
    [Column::Ts, Column::Ids, Column::Meta, Column::Trace, Column::Msg, Column::Body];

/// Set of columns a reader wants decoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ColumnSet(u8);

impl ColumnSet {
    pub const ALL: ColumnSet = ColumnSet(0b11_1111);

    pub fn new() -> Self {
        // Timestamps are always needed for time filtering and ordering.
        ColumnSet(1 << Column::Ts as u8)
    }

    pub fn with(mut self, c: Column) -> Self {
        self.0 |= 1 << c as u8;
        self
    }

    pub fn add(&mut self, c: Column) {
        self.0 |= 1 << c as u8;
    }

    pub fn union(self, other: ColumnSet) -> ColumnSet {
        ColumnSet(self.0 | other.0)
    }

    pub fn contains(self, c: Column) -> bool {
        self.0 & (1 << c as u8) != 0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ChunkRef {
    pub offset: u64,
    pub len: u32,
    pub raw_len: u32,
    pub crc: u32,
}

impl ChunkRef {
    fn encode(&self, w: &mut Writer) {
        w.varint(self.offset);
        w.varint(self.len as u64);
        w.varint(self.raw_len as u64);
        w.u32(self.crc);
    }

    fn decode(r: &mut Reader) -> Result<Self, DecodeError> {
        Ok(Self {
            offset: r.varint()?,
            len: u32::try_from(r.varint()?).map_err(|_| DecodeError::Invalid("chunk len"))?,
            raw_len: u32::try_from(r.varint()?).map_err(|_| DecodeError::Invalid("chunk raw len"))?,
            crc: r.u32()?,
        })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct BlockMeta {
    pub count: u32,
    pub min_ts: Timestamp,
    pub max_ts: Timestamp,
    pub columns: [ChunkRef; NUM_COLUMNS],
}

impl BlockMeta {
    pub fn raw_bytes(&self) -> u64 {
        self.columns.iter().map(|c| c.raw_len as u64).sum()
    }

    pub fn compressed_bytes(&self) -> u64 {
        self.columns.iter().map(|c| c.len as u64).sum()
    }
}

/// Resident per-segment metadata. Kept small: it stays in memory for every
/// segment, so per-block Bloom filters live in [`SegmentIndex`] instead.
#[derive(Debug, Clone, PartialEq)]
pub struct SegmentSummary {
    pub id: u64,
    pub signal: Signal,
    pub format_version: u16,
    pub created_at: Timestamp,
    pub min_ts: Timestamp,
    pub max_ts: Timestamp,
    pub min_event_id: u64,
    pub max_event_id: u64,
    pub event_count: u64,
    /// Uncompressed size of all column data.
    pub raw_bytes: u64,
    /// Compressed size of all column data.
    pub data_bytes: u64,
    /// Index + summary + header/footer bytes (file size minus column data).
    /// Derived when the segment is opened; not stored in the file.
    pub index_bytes: u64,
    /// Segments whose events this one contains (compaction output). On
    /// startup any listed segment that still exists is deleted.
    pub replaces: Vec<u64>,
    pub services: Vec<String>,
    pub environments: Vec<String>,
    /// Bitmap over blocks for each level code 0..=6 (0 = no level).
    pub level_blocks: Vec<Bitmap>,
    /// Bitmap over blocks for each service dictionary entry.
    pub service_blocks: Vec<Bitmap>,
    pub environment_blocks: Vec<Bitmap>,
    /// Distinct names (span names / metric names) when few enough; `None`
    /// when there were too many to list.
    pub names: Option<Vec<String>>,
    pub blocks: Vec<BlockMeta>,
}

impl SegmentSummary {
    pub fn encode(&self, w: &mut Writer) {
        w.u64(self.id);
        w.u8(self.signal.code());
        w.u16(self.format_version);
        w.i64(self.created_at.0);
        w.i64(self.min_ts.0);
        w.i64(self.max_ts.0);
        w.u64(self.min_event_id);
        w.u64(self.max_event_id);
        w.u64(self.event_count);
        w.u64(self.raw_bytes);
        w.u64(self.data_bytes);
        w.varint(self.replaces.len() as u64);
        for r in &self.replaces {
            w.u64(*r);
        }
        for dict in [&self.services, &self.environments] {
            w.varint(dict.len() as u64);
            for s in dict {
                w.str(s);
            }
        }
        for bitmaps in [&self.level_blocks, &self.service_blocks, &self.environment_blocks] {
            w.varint(bitmaps.len() as u64);
            for b in bitmaps {
                b.encode(w);
            }
        }
        match &self.names {
            None => w.u8(0),
            Some(names) => {
                w.u8(1);
                w.varint(names.len() as u64);
                for n in names {
                    w.str(n);
                }
            }
        }
        w.varint(self.blocks.len() as u64);
        for b in &self.blocks {
            w.varint(b.count as u64);
            w.i64(b.min_ts.0);
            w.i64(b.max_ts.0);
            for c in &b.columns {
                c.encode(w);
            }
        }
    }

    pub fn decode(r: &mut Reader) -> Result<Self, DecodeError> {
        let id = r.u64()?;
        let signal = Signal::from_code(r.u8()?).ok_or(DecodeError::Invalid("signal"))?;
        let format_version = r.u16()?;
        if format_version != FORMAT_VERSION {
            return Err(DecodeError::UnsupportedVersion(format_version));
        }
        let created_at = Timestamp(r.i64()?);
        let min_ts = Timestamp(r.i64()?);
        let max_ts = Timestamp(r.i64()?);
        let min_event_id = r.u64()?;
        let max_event_id = r.u64()?;
        let event_count = r.u64()?;
        let raw_bytes = r.u64()?;
        let data_bytes = r.u64()?;
        // Not persisted (it would depend on the summary's own size); the
        // reader fills it in from the file length.
        let index_bytes = 0;
        let n = r.len()?;
        let mut replaces = Vec::with_capacity(n);
        for _ in 0..n {
            replaces.push(r.u64()?);
        }
        let mut dicts = Vec::new();
        for _ in 0..2 {
            let n = r.len()?;
            let mut d = Vec::with_capacity(n);
            for _ in 0..n {
                d.push(r.string()?);
            }
            dicts.push(d);
        }
        let mut bitmap_sets = Vec::new();
        for _ in 0..3 {
            let n = r.len()?;
            let mut v = Vec::with_capacity(n);
            for _ in 0..n {
                v.push(Bitmap::decode(r)?);
            }
            bitmap_sets.push(v);
        }
        let names = match r.u8()? {
            0 => None,
            1 => {
                let n = r.len()?;
                let mut v = Vec::with_capacity(n);
                for _ in 0..n {
                    v.push(r.string()?);
                }
                Some(v)
            }
            t => return Err(DecodeError::InvalidTag(t)),
        };
        let n = r.len()?;
        let mut blocks = Vec::with_capacity(n);
        for _ in 0..n {
            let count = u32::try_from(r.varint()?).map_err(|_| DecodeError::Invalid("count"))?;
            let min_ts = Timestamp(r.i64()?);
            let max_ts = Timestamp(r.i64()?);
            let mut columns = [ChunkRef::default(); NUM_COLUMNS];
            for c in columns.iter_mut() {
                *c = ChunkRef::decode(r)?;
            }
            blocks.push(BlockMeta { count, min_ts, max_ts, columns });
        }
        let environment_blocks = bitmap_sets.pop().unwrap_or_default();
        let service_blocks = bitmap_sets.pop().unwrap_or_default();
        let level_blocks = bitmap_sets.pop().unwrap_or_default();
        let environments = dicts.pop().unwrap_or_default();
        let services = dicts.pop().unwrap_or_default();
        if service_blocks.len() != services.len() || environment_blocks.len() != environments.len() {
            return Err(DecodeError::Invalid("dictionary/bitmap mismatch"));
        }
        Ok(Self {
            id,
            signal,
            format_version,
            created_at,
            min_ts,
            max_ts,
            min_event_id,
            max_event_id,
            event_count,
            raw_bytes,
            data_bytes,
            index_bytes,
            replaces,
            services,
            environments,
            level_blocks,
            service_blocks,
            environment_blocks,
            names,
            blocks,
        })
    }

    /// Approximate resident memory of this summary.
    pub fn approx_size(&self) -> usize {
        std::mem::size_of::<Self>()
            + self.blocks.len() * std::mem::size_of::<BlockMeta>()
            + self.services.iter().map(|s| s.len() + 24).sum::<usize>()
            + self.environments.iter().map(|s| s.len() + 24).sum::<usize>()
            + self.names.as_ref().map_or(0, |n| n.iter().map(|s| s.len() + 24).sum())
            + (self.level_blocks.len() + self.service_blocks.len() + self.environment_blocks.len())
                * (24 + self.blocks.len().div_ceil(64) * 8)
    }
}

/// Per-field statistics, collected cheaply while writing a segment.
#[derive(Debug, Clone, PartialEq)]
pub struct FieldStats {
    pub path: String,
    /// Number of events with at least one value at this path.
    pub present: u64,
    /// Bit set of observed types (bit = Value tag: null, bool, int, float, string).
    pub types: u8,
    pub min: Option<f64>,
    pub max: Option<f64>,
    pub distinct: Hll,
}

pub const TYPE_NULL: u8 = 1;
pub const TYPE_BOOL: u8 = 2;
pub const TYPE_INT: u8 = 4;
pub const TYPE_FLOAT: u8 = 8;
pub const TYPE_STRING: u8 = 16;

pub fn type_names(bits: u8) -> Vec<&'static str> {
    [(TYPE_NULL, "null"), (TYPE_BOOL, "bool"), (TYPE_INT, "int"), (TYPE_FLOAT, "float"), (TYPE_STRING, "string")]
        .iter()
        .filter(|(b, _)| bits & b != 0)
        .map(|(_, n)| *n)
        .collect()
}

#[derive(Debug, Clone, PartialEq)]
pub struct BlockIndex {
    pub bloom: Bloom,
    /// (zone field index, min, max) for zone fields with numeric values in
    /// this block. A tracked field absent here has no numeric values in the
    /// block, which lets numeric range predicates skip it.
    pub zones: Vec<(u16, f64, f64)>,
}

/// Lazily loaded index region.
#[derive(Debug, Clone, PartialEq)]
pub struct SegmentIndex {
    pub zone_fields: Vec<String>,
    pub blocks: Vec<BlockIndex>,
    pub fields: Vec<FieldStats>,
}

impl SegmentIndex {
    pub fn zone_field(&self, path: &str) -> Option<u16> {
        self.zone_fields.iter().position(|f| f == path).map(|i| i as u16)
    }

    pub fn approx_size(&self) -> usize {
        self.blocks.iter().map(|b| b.bloom.byte_size() + b.zones.len() * 24 + 32).sum::<usize>()
            + self.fields.iter().map(|f| f.path.len() + 120).sum::<usize>()
            + self.zone_fields.iter().map(|f| f.len() + 24).sum::<usize>()
    }

    pub fn encode(&self, w: &mut Writer) {
        w.varint(self.zone_fields.len() as u64);
        for f in &self.zone_fields {
            w.str(f);
        }
        w.varint(self.blocks.len() as u64);
        for b in &self.blocks {
            b.bloom.encode(w);
            w.varint(b.zones.len() as u64);
            for (f, min, max) in &b.zones {
                w.u16(*f);
                w.f64(*min);
                w.f64(*max);
            }
        }
        w.varint(self.fields.len() as u64);
        for f in &self.fields {
            w.str(&f.path);
            w.varint(f.present);
            w.u8(f.types);
            let flags = f.min.is_some() as u8 | ((f.max.is_some() as u8) << 1);
            w.u8(flags);
            for v in [f.min, f.max].into_iter().flatten() {
                w.f64(v);
            }
            f.distinct.encode(w);
        }
    }

    pub fn decode(r: &mut Reader) -> Result<Self, DecodeError> {
        let n = r.len()?;
        let mut zone_fields = Vec::with_capacity(n);
        for _ in 0..n {
            zone_fields.push(r.string()?);
        }
        let n = r.len()?;
        let mut blocks = Vec::with_capacity(n);
        for _ in 0..n {
            let bloom = Bloom::decode(r)?;
            let z = r.len()?;
            let mut zones = Vec::with_capacity(z);
            for _ in 0..z {
                zones.push((r.u16()?, r.f64()?, r.f64()?));
            }
            blocks.push(BlockIndex { bloom, zones });
        }
        let n = r.len()?;
        let mut fields = Vec::with_capacity(n);
        for _ in 0..n {
            let path = r.string()?;
            let present = r.varint()?;
            let types = r.u8()?;
            let flags = r.u8()?;
            let min = if flags & 1 != 0 { Some(r.f64()?) } else { None };
            let max = if flags & 2 != 0 { Some(r.f64()?) } else { None };
            let distinct = Hll::decode(r)?;
            fields.push(FieldStats { path, present, types, min, max, distinct });
        }
        Ok(Self { zone_fields, blocks, fields })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Header {
    pub signal: Signal,
    pub segment_id: u64,
    pub created_at: Timestamp,
}

impl Header {
    pub fn encode(&self) -> [u8; HEADER_LEN] {
        let mut b = [0u8; HEADER_LEN];
        b[..8].copy_from_slice(SEGMENT_MAGIC);
        b[8..10].copy_from_slice(&FORMAT_VERSION.to_le_bytes());
        b[10] = self.signal.code();
        b[16..24].copy_from_slice(&self.segment_id.to_le_bytes());
        b[24..32].copy_from_slice(&self.created_at.0.to_le_bytes());
        b
    }

    pub fn decode(b: &[u8]) -> Result<Self, DecodeError> {
        if b.len() < HEADER_LEN || &b[..8] != SEGMENT_MAGIC {
            return Err(DecodeError::Invalid("segment magic"));
        }
        let v = u16::from_le_bytes([b[8], b[9]]);
        if v != FORMAT_VERSION {
            return Err(DecodeError::UnsupportedVersion(v));
        }
        Ok(Self {
            signal: Signal::from_code(b[10]).ok_or(DecodeError::Invalid("signal"))?,
            segment_id: u64::from_le_bytes(b[16..24].try_into().unwrap()),
            created_at: Timestamp(i64::from_le_bytes(b[24..32].try_into().unwrap())),
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Footer {
    pub summary: ChunkRef,
    pub index: ChunkRef,
}

impl Footer {
    pub fn encode(&self) -> [u8; FOOTER_LEN] {
        let mut w = Writer::with_capacity(FOOTER_LEN);
        for c in [self.summary, self.index] {
            w.u64(c.offset);
            w.u32(c.len);
            w.u32(c.raw_len);
            w.u32(c.crc);
        }
        w.u16(FORMAT_VERSION);
        w.u16(0);
        let crc = crc32fast::hash(&w.buf);
        w.u32(crc);
        w.raw(FOOTER_MAGIC);
        let mut out = [0u8; FOOTER_LEN];
        out.copy_from_slice(&w.buf);
        out
    }

    pub fn decode(b: &[u8]) -> Result<Self, DecodeError> {
        if b.len() != FOOTER_LEN || &b[FOOTER_LEN - 8..] != FOOTER_MAGIC {
            return Err(DecodeError::Invalid("footer magic"));
        }
        let body = &b[..FOOTER_LEN - 12];
        let crc = u32::from_le_bytes(b[FOOTER_LEN - 12..FOOTER_LEN - 8].try_into().unwrap());
        if crc32fast::hash(body) != crc {
            return Err(DecodeError::Checksum);
        }
        let mut r = Reader::new(body);
        let mut chunk = || -> Result<ChunkRef, DecodeError> {
            Ok(ChunkRef { offset: r.u64()?, len: r.u32()?, raw_len: r.u32()?, crc: r.u32()? })
        };
        let summary = chunk()?;
        let index = chunk()?;
        let v = r.u16()?;
        if v != FORMAT_VERSION {
            return Err(DecodeError::UnsupportedVersion(v));
        }
        Ok(Self { summary, index })
    }
}

pub fn segment_file_name(id: u64) -> String {
    format!("{id:012}.seg")
}

pub fn parse_segment_file_name(name: &str) -> Option<u64> {
    let stem = name.strip_suffix(".seg")?;
    if stem.len() != 12 || !stem.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    stem.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_footer_round_trip() {
        let h = Header { signal: Signal::Traces, segment_id: 9, created_at: Timestamp(123) };
        assert_eq!(Header::decode(&h.encode()).unwrap(), h);

        let f = Footer {
            summary: ChunkRef { offset: 100, len: 20, raw_len: 40, crc: 7 },
            index: ChunkRef { offset: 50, len: 50, raw_len: 90, crc: 8 },
        };
        let bytes = f.encode();
        assert_eq!(Footer::decode(&bytes).unwrap(), f);
        let mut bad = bytes;
        bad[3] ^= 1;
        assert_eq!(Footer::decode(&bad), Err(DecodeError::Checksum));
    }

    #[test]
    fn file_names() {
        assert_eq!(segment_file_name(1), "000000000001.seg");
        assert_eq!(parse_segment_file_name("000000000001.seg"), Some(1));
        assert_eq!(parse_segment_file_name("000000000001.seg.tmp"), None);
    }

    #[test]
    fn column_set() {
        let s = ColumnSet::new().with(Column::Meta);
        assert!(s.contains(Column::Ts) && s.contains(Column::Meta) && !s.contains(Column::Body));
        assert!(ColumnSet::ALL.contains(Column::Body));
    }
}
