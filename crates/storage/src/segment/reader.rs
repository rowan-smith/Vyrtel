//! Reading segment files.
//!
//! Opening a segment reads only the header, footer and summary. Index
//! regions (Bloom filters, zone maps, field stats) and column chunks are read
//! on demand, and only the columns a query needs are decompressed.

use std::fs::File;
use std::path::{Path, PathBuf};

use telemetry::*;

use crate::codec::{self, Head};
use crate::encoding::Reader;
use crate::error::{DecodeError, IoContext, Result, StorageError, corrupt};
use crate::fsutil::read_exact_at;

use super::format::*;

#[derive(Debug)]
pub struct Segment {
    pub summary: SegmentSummary,
    pub path: PathBuf,
    pub file_len: u64,
    file: File,
    index_ref: ChunkRef,
}

impl Segment {
    /// Open and validate a segment. Any structural problem (bad magic,
    /// checksum mismatch, truncated file) is an error: the caller
    /// quarantines the file rather than guessing at its contents.
    pub fn open(path: &Path) -> Result<Segment> {
        let file = File::open(path).ctx(path)?;
        let file_len = file.metadata().ctx(path)?.len();
        let what = || path.display().to_string();
        if file_len < (HEADER_LEN + FOOTER_LEN) as u64 {
            return Err(corrupt(what(), DecodeError::UnexpectedEof));
        }
        let mut hb = [0u8; HEADER_LEN];
        read_exact_at(&file, &mut hb, 0).ctx(path)?;
        let header = Header::decode(&hb).map_err(|e| corrupt(what(), e))?;

        let mut fb = [0u8; FOOTER_LEN];
        read_exact_at(&file, &mut fb, file_len - FOOTER_LEN as u64).ctx(path)?;
        let footer = Footer::decode(&fb).map_err(|e| corrupt(what(), e))?;

        let raw = read_chunk(&file, path, &footer.summary, file_len)?;
        let mut summary = SegmentSummary::decode(&mut Reader::new(&raw)).map_err(|e| corrupt(what(), e))?;
        if summary.id != header.segment_id || summary.signal != header.signal {
            return Err(corrupt(what(), DecodeError::Invalid("header/summary mismatch")));
        }
        for b in &summary.blocks {
            for c in &b.columns {
                if c.offset + c.len as u64 > file_len {
                    return Err(corrupt(what(), DecodeError::LengthOutOfBounds));
                }
            }
        }
        summary.index_bytes = file_len.saturating_sub(summary.data_bytes);
        Ok(Segment { summary, path: path.to_path_buf(), file_len, file, index_ref: footer.index })
    }

    pub fn id(&self) -> u64 {
        self.summary.id
    }

    pub fn index_compressed_len(&self) -> u64 {
        self.index_ref.len as u64
    }

    /// Read and decode the index region. Callers cache the result.
    pub fn read_index(&self) -> Result<SegmentIndex> {
        let raw = read_chunk(&self.file, &self.path, &self.index_ref, self.file_len)?;
        let idx =
            SegmentIndex::decode(&mut Reader::new(&raw)).map_err(|e| corrupt(self.path.display().to_string(), e))?;
        if idx.blocks.len() != self.summary.blocks.len() {
            return Err(corrupt(self.path.display().to_string(), DecodeError::Invalid("index block count")));
        }
        Ok(idx)
    }

    /// Read the requested columns of one block.
    pub fn read_block(&self, block: usize, columns: ColumnSet) -> Result<BlockData> {
        let meta = &self.summary.blocks[block];
        let n = meta.count as usize;
        let what = || format!("{} block {block}", self.path.display());
        let mut data = BlockData {
            len: n,
            bytes_read: 0,
            ts: Vec::new(),
            ids: None,
            meta: None,
            trace: None,
            msg: None,
            body: None,
        };
        let mut load = |c: Column| -> Result<Vec<u8>> {
            let r = &meta.columns[c as usize];
            data.bytes_read += r.len as u64;
            read_chunk(&self.file, &self.path, r, self.file_len)
        };

        let ts_raw = load(Column::Ts)?;
        let ts = decode_ts(&ts_raw, n).map_err(|e| corrupt(what(), e))?;
        let ids = if columns.contains(Column::Ids) {
            Some(decode_ids(&load(Column::Ids)?, n).map_err(|e| corrupt(what(), e))?)
        } else {
            None
        };
        let meta_col = if columns.contains(Column::Meta) {
            Some(decode_meta(&load(Column::Meta)?, n).map_err(|e| corrupt(what(), e))?)
        } else {
            None
        };
        let trace = if columns.contains(Column::Trace) {
            Some(decode_trace(&load(Column::Trace)?, n).map_err(|e| corrupt(what(), e))?)
        } else {
            None
        };
        let msg = if columns.contains(Column::Msg) {
            Some(decode_msg(&load(Column::Msg)?, n).map_err(|e| corrupt(what(), e))?)
        } else {
            None
        };
        let body = if columns.contains(Column::Body) {
            let raw = load(Column::Body)?;
            let offsets = body_offsets(&raw, n).map_err(|e| corrupt(what(), e))?;
            Some(BodyColumn { raw, offsets })
        } else {
            None
        };
        data.ts = ts;
        data.ids = ids;
        data.meta = meta_col;
        data.trace = trace;
        data.msg = msg;
        data.body = body;
        Ok(data)
    }

    /// Decode every event in the segment (used by compaction and tests).
    pub fn read_all(&self) -> Result<Vec<TelemetryEvent>> {
        let mut out = Vec::with_capacity(self.summary.event_count as usize);
        for b in 0..self.summary.blocks.len() {
            let data = self.read_block(b, ColumnSet::ALL)?;
            for i in 0..data.len {
                out.push(data.event(&self.summary, i)?);
            }
        }
        Ok(out)
    }
}

fn read_chunk(file: &File, path: &Path, r: &ChunkRef, file_len: u64) -> Result<Vec<u8>> {
    let what = || path.display().to_string();
    if r.offset + r.len as u64 > file_len {
        return Err(corrupt(what(), DecodeError::LengthOutOfBounds));
    }
    if r.raw_len > MAX_CHUNK_RAW {
        return Err(corrupt(what(), DecodeError::LengthOutOfBounds));
    }
    let mut buf = vec![0u8; r.len as usize];
    read_exact_at(file, &mut buf, r.offset).ctx(path)?;
    // Verify before decompressing: never feed corrupt bytes to zstd.
    if crc32fast::hash(&buf) != r.crc {
        return Err(corrupt(what(), DecodeError::Checksum));
    }
    let raw = zstd::bulk::decompress(&buf, r.raw_len as usize).map_err(StorageError::Compression)?;
    if raw.len() != r.raw_len as usize {
        return Err(corrupt(what(), DecodeError::Invalid("decompressed length")));
    }
    Ok(raw)
}

fn decode_ts(raw: &[u8], n: usize) -> Result<Vec<i64>, DecodeError> {
    let mut r = Reader::new(raw);
    let mut out = Vec::with_capacity(n);
    if n == 0 {
        return Ok(out);
    }
    let mut last = r.i64()?;
    out.push(last);
    for _ in 1..n {
        last = last.wrapping_add(r.varint_i()?);
        out.push(last);
    }
    Ok(out)
}

fn decode_ids(raw: &[u8], n: usize) -> Result<Vec<u64>, DecodeError> {
    let mut r = Reader::new(raw);
    let mut out = Vec::with_capacity(n);
    if n == 0 {
        return Ok(out);
    }
    let mut last = r.varint()?;
    out.push(last);
    for _ in 1..n {
        last = last.wrapping_add(r.varint_i()? as u64);
        out.push(last);
    }
    Ok(out)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MetaRow {
    pub kind: u8,
    pub level: u8,
    /// 0 = none, otherwise dictionary index + 1.
    pub service: u32,
    pub environment: u32,
}

fn decode_meta(raw: &[u8], n: usize) -> Result<Vec<MetaRow>, DecodeError> {
    let mut r = Reader::new(raw);
    let mut out = Vec::with_capacity(n);
    for _ in 0..n {
        out.push(MetaRow {
            kind: r.u8()?,
            level: r.u8()?,
            service: u32::try_from(r.varint()?).map_err(|_| DecodeError::Invalid("dict ref"))?,
            environment: u32::try_from(r.varint()?).map_err(|_| DecodeError::Invalid("dict ref"))?,
        });
    }
    Ok(out)
}

/// Per event: (trace id, span id).
pub type TraceColumn = Vec<(Option<String>, Option<String>)>;

fn decode_trace(raw: &[u8], n: usize) -> Result<TraceColumn, DecodeError> {
    let mut r = Reader::new(raw);
    let mut out = Vec::with_capacity(n);
    for _ in 0..n {
        out.push((r.opt_string()?, r.opt_string()?));
    }
    Ok(out)
}

fn decode_msg(raw: &[u8], n: usize) -> Result<Vec<String>, DecodeError> {
    let mut r = Reader::new(raw);
    let mut out = Vec::with_capacity(n);
    for _ in 0..n {
        out.push(r.string()?);
    }
    Ok(out)
}

fn body_offsets(raw: &[u8], n: usize) -> Result<Vec<(usize, usize)>, DecodeError> {
    let mut r = Reader::new(raw);
    let mut out = Vec::with_capacity(n);
    for _ in 0..n {
        let len = r.len()?;
        let start = r.position();
        r.take(len)?;
        out.push((start, len));
    }
    Ok(out)
}

pub struct BodyColumn {
    raw: Vec<u8>,
    offsets: Vec<(usize, usize)>,
}

impl BodyColumn {
    pub fn bytes(&self, i: usize) -> &[u8] {
        let (s, l) = self.offsets[i];
        &self.raw[s..s + l]
    }
}

/// Decoded columns of one block. Columns that were not requested are
/// `None`; accessing them is a programming error in the caller's column
/// selection and panics.
pub struct BlockData {
    pub len: usize,
    pub bytes_read: u64,
    pub ts: Vec<i64>,
    pub ids: Option<Vec<u64>>,
    pub meta: Option<Vec<MetaRow>>,
    pub trace: Option<TraceColumn>,
    pub msg: Option<Vec<String>>,
    pub body: Option<BodyColumn>,
}

impl BlockData {
    pub fn id(&self, i: usize) -> u64 {
        self.ids.as_ref().expect("ids column not loaded")[i]
    }

    pub fn meta(&self, i: usize) -> MetaRow {
        self.meta.as_ref().expect("meta column not loaded")[i]
    }

    pub fn trace_id(&self, i: usize) -> Option<&str> {
        self.trace.as_ref().expect("trace column not loaded")[i].0.as_deref()
    }

    pub fn span_id(&self, i: usize) -> Option<&str> {
        self.trace.as_ref().expect("trace column not loaded")[i].1.as_deref()
    }

    pub fn message(&self, i: usize) -> &str {
        &self.msg.as_ref().expect("msg column not loaded")[i]
    }

    pub fn body_bytes(&self, i: usize) -> &[u8] {
        self.body.as_ref().expect("body column not loaded").bytes(i)
    }

    /// Materialise a full event. Requires all columns.
    pub fn event(&self, summary: &SegmentSummary, i: usize) -> Result<TelemetryEvent> {
        let m = self.meta(i);
        let dict = |d: &Vec<String>, code: u32| -> Result<Option<String>> {
            if code == 0 {
                return Ok(None);
            }
            d.get(code as usize - 1)
                .cloned()
                .map(Some)
                .ok_or_else(|| corrupt(format!("segment {}", summary.id), DecodeError::Invalid("dictionary reference")))
        };
        let (trace, span) = &self.trace.as_ref().expect("trace column not loaded")[i];
        let head = Head {
            id: EventId(self.id(i)),
            timestamp: Timestamp(self.ts[i]),
            kind: m.kind,
            level: m.level,
            service: dict(&summary.services, m.service)?,
            environment: dict(&summary.environments, m.environment)?,
            trace_id: trace.clone().map(TraceId::from_stored),
            span_id: span.clone().map(SpanId::from_stored),
            message: self.message(i).to_string(),
        };
        let mut r = Reader::new(self.body_bytes(i));
        codec::decode_with_body(head, &mut r).map_err(|e| corrupt(format!("segment {} event body", summary.id), e))
    }
}
