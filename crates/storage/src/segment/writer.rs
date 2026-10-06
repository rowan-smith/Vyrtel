//! Building and committing immutable segment files.
//!
//! Commit procedure (see docs/storage-format.md):
//! 1. write the whole file under `tmp/`
//! 2. fsync the file
//! 3. rename it into `segments/<signal>/` — the atomic commit point
//! 4. fsync the directory
//!
//! A crash before step 3 leaves only a temp file, which startup deletes.
//! A crash after step 3 leaves a complete, checksummed segment.

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

use telemetry::*;

use crate::codec;
use crate::encoding::Writer;
use crate::error::{IoContext, Result, StorageError};
use crate::fsutil;
use crate::index::keys;
use crate::index::{Bitmap, Bloom, Hll};

use super::format::*;

/// Paths of well-known fields in Bloom filters / zone maps. The query
/// planner uses the same constants when probing.
pub mod paths {
    pub const TRACE_ID: &str = "traceId";
    pub const SPAN_ID: &str = "spanId";
    pub const PARENT_SPAN_ID: &str = "parentSpanId";
    pub const MESSAGE_TEMPLATE: &str = "messageTemplate";
    pub const EXCEPTION_TYPE: &str = "exception.type";
    pub const NAME: &str = "name";
    pub const STATUS: &str = "status";
    pub const SPAN_KIND: &str = "spanKind";
    pub const DURATION_MS: &str = "durationMs";
    pub const VALUE: &str = "value";
}

#[derive(Debug, Clone)]
pub struct SegmentWriteOptions {
    /// Maximum events per block.
    pub block_events: usize,
    /// Approximate maximum uncompressed bytes per block.
    pub block_bytes: usize,
    pub zstd_level: i32,
    pub bloom_bits_per_key: usize,
    /// Numeric fields that get per-block min/max zone maps.
    pub max_zone_fields: usize,
    pub max_field_stats: usize,
    pub max_names: usize,
}

impl Default for SegmentWriteOptions {
    fn default() -> Self {
        Self {
            block_events: 1024,
            block_bytes: 1024 * 1024,
            zstd_level: 3,
            bloom_bits_per_key: 10,
            max_zone_fields: 64,
            max_field_stats: 256,
            max_names: 4096,
        }
    }
}

/// While collecting statistics we track at most this many distinct paths,
/// so a producer emitting random keys cannot blow up memory.
const FIELD_TRACKING_LIMIT: usize = 4096;

struct FieldAcc {
    present: u64,
    last_event: usize,
    types: u8,
    min: Option<f64>,
    max: Option<f64>,
    numeric: u64,
    hll: Hll,
}

/// Visit every (path, scalar) pair that a query could resolve for this
/// event: attribute and resource leaves plus well-known indexed fields.
fn visit_indexed(e: &TelemetryEvent, f: &mut dyn FnMut(&str, &Value)) {
    e.attributes.for_each_leaf(f);
    e.resource.for_each_leaf(f);
}

/// Numeric "virtual" fields with zone maps regardless of attributes.
fn virtual_numbers(e: &TelemetryEvent) -> Option<(&'static str, f64)> {
    match &e.payload {
        TelemetryPayload::Span(s) => Some((paths::DURATION_MS, s.duration_ms())),
        TelemetryPayload::Metric(m) => match m.value {
            MetricValue::Number(v) if v.is_finite() => Some((paths::VALUE, v)),
            _ => None,
        },
        _ => None,
    }
}

/// Keys for well-known non-column fields that are cheap to Bloom-index.
/// Text fields go through [`keys::stored_keys`] like attributes do, so
/// `name = 5` finds a span named "5" exactly as the evaluator would.
fn well_known_keys(e: &TelemetryEvent, out: &mut dyn FnMut(u64)) {
    let mut text = |path: &str, s: &str| keys::stored_keys(path, &Value::String(s.to_string()), out);
    if let Some(t) = &e.trace_id {
        text(paths::TRACE_ID, t.as_str());
    }
    if let Some(s) = &e.span_id {
        text(paths::SPAN_ID, s.as_str());
    }
    match &e.payload {
        TelemetryPayload::Log(l) => {
            if let Some(t) = &l.message_template {
                text(paths::MESSAGE_TEMPLATE, t);
            }
            if let Some(t) = l.exception.as_ref().and_then(|x| x.kind.as_deref()) {
                text(paths::EXCEPTION_TYPE, t);
            }
        }
        TelemetryPayload::Span(s) => {
            text(paths::NAME, &s.name);
            text(paths::STATUS, s.status.code.as_str());
            text(paths::SPAN_KIND, s.kind.as_str());
            if let Some(p) = &s.parent_span_id {
                text(paths::PARENT_SPAN_ID, p.as_str());
            }
        }
        TelemetryPayload::Metric(m) => text(paths::NAME, &m.name),
    }
}

fn type_bit(v: &Value) -> u8 {
    match v {
        Value::Null => TYPE_NULL,
        Value::Bool(_) => TYPE_BOOL,
        Value::Int(_) => TYPE_INT,
        Value::Float(_) => TYPE_FLOAT,
        Value::String(_) => TYPE_STRING,
        _ => 0,
    }
}

struct Dict {
    values: Vec<String>,
    index: HashMap<String, u32>,
}

impl Dict {
    fn new() -> Self {
        Self { values: Vec::new(), index: HashMap::new() }
    }

    /// 0 = absent, otherwise dictionary index + 1.
    fn code(&mut self, v: Option<&str>) -> u32 {
        let Some(v) = v else { return 0 };
        if let Some(i) = self.index.get(v) {
            return i + 1;
        }
        let i = self.values.len() as u32;
        self.values.push(v.to_string());
        self.index.insert(v.to_string(), i);
        i + 1
    }
}

struct Columns {
    ts: Writer,
    ids: Writer,
    meta: Writer,
    trace: Writer,
    msg: Writer,
    body: Writer,
    count: usize,
    min_ts: i64,
    max_ts: i64,
    last_ts: i64,
    last_id: u64,
    keys: HashSet<u64>,
    zones: HashMap<u16, (f64, f64)>,
}

impl Columns {
    fn new() -> Self {
        Self {
            ts: Writer::new(),
            ids: Writer::new(),
            meta: Writer::new(),
            trace: Writer::new(),
            msg: Writer::new(),
            body: Writer::new(),
            count: 0,
            min_ts: i64::MAX,
            max_ts: i64::MIN,
            last_ts: 0,
            last_id: 0,
            keys: HashSet::new(),
            zones: HashMap::new(),
        }
    }

    fn raw_len(&self) -> usize {
        self.ts.len() + self.ids.len() + self.meta.len() + self.trace.len() + self.msg.len() + self.body.len()
    }
}

/// Output of a committed segment write.
pub struct WrittenSegment {
    pub path: PathBuf,
    pub summary: SegmentSummary,
}

/// Write `events` (which must be sorted by `(timestamp, id)`) as segment
/// `id` and commit it into `dir`.
pub fn write_segment(
    dir: &Path,
    tmp_dir: &Path,
    id: u64,
    signal: Signal,
    events: &[&TelemetryEvent],
    replaces: Vec<u64>,
    opts: &SegmentWriteOptions,
) -> Result<WrittenSegment> {
    debug_assert!(events.windows(2).all(|w| (w[0].timestamp, w[0].id) <= (w[1].timestamp, w[1].id)));

    let final_path = dir.join(segment_file_name(id));
    let tmp_path = tmp_dir.join(format!("{}.{}.tmp", signal.as_str(), segment_file_name(id)));
    let result = write_file(&tmp_path, id, signal, events, replaces, opts);
    let summary = match result {
        Ok(s) => s,
        Err(e) => {
            let _ = std::fs::remove_file(&tmp_path);
            return Err(e);
        }
    };
    // Commit point: after this rename the segment is visible to recovery.
    if let Err(e) = fsutil::rename(&tmp_path, &final_path) {
        let _ = std::fs::remove_file(&tmp_path);
        return Err(e);
    }
    fsutil::sync_dir(dir)?;
    Ok(WrittenSegment { path: final_path, summary })
}

struct CountingWriter {
    inner: BufWriter<File>,
    offset: u64,
    path: PathBuf,
}

impl CountingWriter {
    fn write(&mut self, b: &[u8]) -> Result<()> {
        self.inner.write_all(b).ctx(&self.path)?;
        self.offset += b.len() as u64;
        Ok(())
    }

    fn write_chunk(&mut self, raw: &[u8], level: i32) -> Result<ChunkRef> {
        let compressed = zstd::bulk::compress(raw, level).map_err(StorageError::Compression)?;
        let chunk = ChunkRef {
            offset: self.offset,
            len: u32::try_from(compressed.len()).map_err(|_| StorageError::BatchTooLarge(compressed.len()))?,
            raw_len: u32::try_from(raw.len()).map_err(|_| StorageError::BatchTooLarge(raw.len()))?,
            crc: crc32fast::hash(&compressed),
        };
        self.write(&compressed)?;
        Ok(chunk)
    }
}

fn write_file(
    path: &Path,
    id: u64,
    signal: Signal,
    events: &[&TelemetryEvent],
    replaces: Vec<u64>,
    opts: &SegmentWriteOptions,
) -> Result<SegmentSummary> {
    let created_at = Timestamp::now();

    // Pass 1: field statistics, used to pick zone-map fields.
    let mut fields: HashMap<String, FieldAcc> = HashMap::new();
    for (i, e) in events.iter().enumerate() {
        visit_indexed(e, &mut |p, v| {
            if !fields.contains_key(p) && fields.len() >= FIELD_TRACKING_LIMIT {
                return;
            }
            let acc = fields.entry(p.to_string()).or_insert_with(|| FieldAcc {
                present: 0,
                last_event: usize::MAX,
                types: 0,
                min: None,
                max: None,
                numeric: 0,
                hll: Hll::default(),
            });
            if acc.last_event != i {
                acc.present += 1;
                acc.last_event = i;
            }
            acc.types |= type_bit(v);
            if let Some(n) = v.as_f64() {
                acc.numeric += 1;
                acc.min = Some(acc.min.map_or(n, |m| m.min(n)));
                acc.max = Some(acc.max.map_or(n, |m| m.max(n)));
            }
            let mut first = None;
            keys::stored_keys(p, v, &mut |h| {
                first.get_or_insert(h);
            });
            if let Some(h) = first {
                acc.hll.insert(h);
            }
        });
    }

    let mut zone_fields: Vec<String> = Vec::new();
    match signal {
        Signal::Traces => zone_fields.push(paths::DURATION_MS.into()),
        Signal::Metrics => zone_fields.push(paths::VALUE.into()),
        Signal::Logs => {}
    }
    let mut numeric: Vec<(&String, u64)> =
        fields.iter().filter(|(_, a)| a.numeric > 0).map(|(k, a)| (k, a.numeric)).collect();
    numeric.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
    for (k, _) in numeric {
        if zone_fields.len() >= opts.max_zone_fields {
            break;
        }
        if !zone_fields.contains(k) {
            zone_fields.push(k.clone());
        }
    }
    let zone_index: HashMap<&str, u16> = zone_fields.iter().enumerate().map(|(i, f)| (f.as_str(), i as u16)).collect();

    let file = File::create(path).ctx(path)?;
    let mut out =
        CountingWriter { inner: BufWriter::with_capacity(1 << 20, file), offset: 0, path: path.to_path_buf() };
    out.write(&Header { signal, segment_id: id, created_at }.encode())?;

    let mut services = Dict::new();
    let mut envs = Dict::new();
    let mut level_blocks = vec![Bitmap::new(); 7];
    let mut service_blocks: Vec<Bitmap> = Vec::new();
    let mut env_blocks: Vec<Bitmap> = Vec::new();
    let mut names: Option<HashSet<String>> = (signal != Signal::Logs).then(HashSet::new);
    let mut blocks: Vec<BlockMeta> = Vec::new();
    let mut block_indexes: Vec<BlockIndex> = Vec::new();
    let mut raw_bytes = 0u64;
    let mut data_bytes = 0u64;

    let mut cols = Columns::new();
    let mut body = Writer::new();

    // Pass 2: columns, block by block.
    for e in events {
        let block_no = blocks.len();
        let ts = e.timestamp.0;
        if cols.count == 0 {
            cols.ts.i64(ts);
            cols.ids.varint(e.id.0);
        } else {
            cols.ts.varint_i(ts.wrapping_sub(cols.last_ts));
            cols.ids.varint_i(e.id.0.wrapping_sub(cols.last_id) as i64);
        }
        cols.last_ts = ts;
        cols.last_id = e.id.0;
        cols.min_ts = cols.min_ts.min(ts);
        cols.max_ts = cols.max_ts.max(ts);

        let level = codec::level_code(e);
        let svc = services.code(e.service.as_deref());
        let env = envs.code(e.environment.as_deref());
        cols.meta.u8(codec::kind_code(e));
        cols.meta.u8(level);
        cols.meta.varint(svc as u64);
        cols.meta.varint(env as u64);
        level_blocks[level as usize].set(block_no);
        if svc > 0 {
            let i = (svc - 1) as usize;
            if service_blocks.len() <= i {
                service_blocks.resize(i + 1, Bitmap::new());
            }
            service_blocks[i].set(block_no);
        }
        if env > 0 {
            let i = (env - 1) as usize;
            if env_blocks.len() <= i {
                env_blocks.resize(i + 1, Bitmap::new());
            }
            env_blocks[i].set(block_no);
        }

        cols.trace.opt_str(e.trace_id.as_ref().map(TraceId::as_str));
        cols.trace.opt_str(e.span_id.as_ref().map(SpanId::as_str));
        cols.msg.str(e.message());
        if let Some(set) = names.as_mut() {
            if set.len() < opts.max_names {
                set.insert(e.message().to_string());
            } else if !set.contains(e.message()) {
                names = None;
            }
        }

        body.buf.clear();
        codec::encode_body(&mut body, e);
        cols.body.bytes(&body.buf);

        // Index keys and zone maps for this block.
        let keyset = &mut cols.keys;
        let zones = &mut cols.zones;
        let mut zone_add = |path: &str, n: f64| {
            if let Some(z) = zone_index.get(path) {
                let entry = zones.entry(*z).or_insert((n, n));
                entry.0 = entry.0.min(n);
                entry.1 = entry.1.max(n);
            }
        };
        visit_indexed(e, &mut |p, v| {
            keys::stored_keys(p, v, &mut |h| {
                keyset.insert(h);
            });
            if let Some(n) = v.as_f64() {
                zone_add(p, n);
            }
        });
        if let Some((p, n)) = virtual_numbers(e) {
            zone_add(p, n);
        }
        well_known_keys(e, &mut |h| {
            keyset.insert(h);
        });

        cols.count += 1;
        if cols.count >= opts.block_events || cols.raw_len() >= opts.block_bytes {
            let (meta, index) = flush_block(&mut out, &mut cols, opts)?;
            raw_bytes += meta.raw_bytes();
            data_bytes += meta.compressed_bytes();
            blocks.push(meta);
            block_indexes.push(index);
        }
    }
    if cols.count > 0 {
        let (meta, index) = flush_block(&mut out, &mut cols, opts)?;
        raw_bytes += meta.raw_bytes();
        data_bytes += meta.compressed_bytes();
        blocks.push(meta);
        block_indexes.push(index);
    }

    // Field statistics: keep the most frequently present fields.
    let mut stats: Vec<FieldStats> = fields
        .into_iter()
        .map(|(path, a)| FieldStats {
            path,
            present: a.present,
            types: a.types,
            min: a.min,
            max: a.max,
            distinct: a.hll,
        })
        .collect();
    stats.sort_by(|a, b| b.present.cmp(&a.present).then(a.path.cmp(&b.path)));
    stats.truncate(opts.max_field_stats);

    let index = SegmentIndex { zone_fields, blocks: block_indexes, fields: stats };
    let mut iw = Writer::new();
    index.encode(&mut iw);
    let index_ref = out.write_chunk(&iw.buf, opts.zstd_level)?;

    let mut names: Option<Vec<String>> = names.map(|s| s.into_iter().collect());
    if let Some(n) = names.as_mut() {
        n.sort();
    }
    let mut summary = SegmentSummary {
        id,
        signal,
        format_version: FORMAT_VERSION,
        created_at,
        min_ts: Timestamp(events.iter().map(|e| e.timestamp.0).min().unwrap_or(0)),
        max_ts: Timestamp(events.iter().map(|e| e.timestamp.0).max().unwrap_or(0)),
        min_event_id: events.iter().map(|e| e.id.0).min().unwrap_or(0),
        max_event_id: events.iter().map(|e| e.id.0).max().unwrap_or(0),
        event_count: events.len() as u64,
        raw_bytes,
        data_bytes,
        index_bytes: 0,
        replaces,
        services: services.values,
        environments: envs.values,
        level_blocks,
        service_blocks,
        environment_blocks: env_blocks,
        names,
        blocks,
    };
    let mut sw = Writer::new();
    summary.encode(&mut sw);
    let summary_ref = out.write_chunk(&sw.buf, opts.zstd_level)?;
    out.write(&Footer { summary: summary_ref, index: index_ref }.encode())?;

    let file_len = out.offset;
    let file = out.inner.into_inner().map_err(|e| e.into_error()).ctx(path)?;
    // Durability: the data must be on disk before the rename publishes it.
    file.sync_all().ctx(path)?;
    summary.index_bytes = file_len - data_bytes;
    Ok(summary)
}

fn flush_block(
    out: &mut CountingWriter,
    cols: &mut Columns,
    opts: &SegmentWriteOptions,
) -> Result<(BlockMeta, BlockIndex)> {
    let c = std::mem::replace(cols, Columns::new());
    let mut refs = [ChunkRef::default(); NUM_COLUMNS];
    for (i, raw) in [&c.ts, &c.ids, &c.meta, &c.trace, &c.msg, &c.body].into_iter().enumerate() {
        refs[i] = out.write_chunk(&raw.buf, opts.zstd_level)?;
    }
    let mut bloom = Bloom::with_capacity(c.keys.len(), opts.bloom_bits_per_key);
    for k in &c.keys {
        bloom.insert(*k);
    }
    let mut zones: Vec<(u16, f64, f64)> = c.zones.into_iter().map(|(f, (a, b))| (f, a, b)).collect();
    zones.sort_by_key(|z| z.0);
    Ok((
        BlockMeta { count: c.count as u32, min_ts: Timestamp(c.min_ts), max_ts: Timestamp(c.max_ts), columns: refs },
        BlockIndex { bloom, zones },
    ))
}
