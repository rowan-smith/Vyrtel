//! Query execution over a stream snapshot.
//!
//! ```text
//! parse → compile → for each candidate segment (newest first):
//!     time-range elimination      (segment summary)
//!     bitmap / Bloom / zone maps  (candidate block bitmap)
//!     decode only needed columns  (meta, msg, ... or full body)
//!     evaluate remaining predicate per event
//!   stop once the result set is full and no remaining segment/block can
//!   contain a better (newer) event
//! ```

use std::cmp::Ordering;
use std::collections::BinaryHeap;
use std::ops::ControlFlow;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::Serialize;
use storage::segment::{BlockData, Column, ColumnSet, Segment, SegmentSummary};
use storage::{IndexCache, StreamSnapshot};
use telemetry::*;

use crate::error::QueryError;
use crate::eval::{CExpr, Row, eval};
use crate::prune::{IndexUsage, PruneCtx, candidate_blocks, needs_index};

#[derive(Debug, Clone)]
pub struct QueryLimits {
    pub timeout: Duration,
    /// Maximum compressed bytes a single query may read from segments.
    pub max_scan_bytes: u64,
    pub max_results: usize,
}

impl Default for QueryLimits {
    fn default() -> Self {
        Self { timeout: Duration::from_secs(30), max_scan_bytes: 4 * 1024 * 1024 * 1024, max_results: 1000 }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Direction {
    /// Newest first.
    #[default]
    Backward,
    /// Oldest first.
    Forward,
}

/// Half-open time range `[from, to)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimeRange {
    pub from: Timestamp,
    pub to: Timestamp,
}

impl TimeRange {
    pub fn new(from: Option<Timestamp>, to: Option<Timestamp>) -> Self {
        Self { from: from.unwrap_or(Timestamp::MIN), to: to.unwrap_or(Timestamp::MAX) }
    }

    pub fn all() -> Self {
        Self::new(None, None)
    }

    pub fn contains(&self, t: Timestamp) -> bool {
        t >= self.from && t < self.to
    }

    /// Does `[min, max]` (inclusive) overlap this range?
    pub fn overlaps(&self, min: Timestamp, max: Timestamp) -> bool {
        max >= self.from && min < self.to
    }

    pub fn covers(&self, min: Timestamp, max: Timestamp) -> bool {
        min >= self.from && max < self.to
    }
}

/// Opaque position for pagination: the last returned `(timestamp, id)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cursor {
    pub ts: i64,
    pub id: u64,
}

impl Cursor {
    pub fn encode(&self) -> String {
        format!("v1.{:x}.{:x}", self.ts as u64, self.id)
    }

    pub fn decode(s: &str) -> Option<Self> {
        let mut parts = s.split('.');
        if parts.next()? != "v1" {
            return None;
        }
        let ts = u64::from_str_radix(parts.next()?, 16).ok()? as i64;
        let id = u64::from_str_radix(parts.next()?, 16).ok()?;
        if parts.next().is_some() {
            return None;
        }
        Some(Self { ts, id })
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexUse {
    pub field: String,
    pub kind: &'static str,
}

/// Execution statistics returned with every query.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Diagnostics {
    pub elapsed_ms: f64,
    pub segments_considered: usize,
    pub segments_skipped: usize,
    pub segments_skipped_by_time: usize,
    pub segments_skipped_by_index: usize,
    pub segments_not_needed: usize,
    pub blocks_considered: usize,
    pub blocks_skipped: usize,
    pub blocks_read: usize,
    pub events_examined: u64,
    pub events_matched: u64,
    pub bytes_read: u64,
    pub unsealed_events_scanned: u64,
    pub indexes: Vec<IndexUse>,
}

impl Diagnostics {
    pub fn merge(&mut self, o: &Diagnostics) {
        self.segments_considered += o.segments_considered;
        self.segments_skipped += o.segments_skipped;
        self.segments_skipped_by_time += o.segments_skipped_by_time;
        self.segments_skipped_by_index += o.segments_skipped_by_index;
        self.segments_not_needed += o.segments_not_needed;
        self.blocks_considered += o.blocks_considered;
        self.blocks_skipped += o.blocks_skipped;
        self.blocks_read += o.blocks_read;
        self.events_examined += o.events_examined;
        self.events_matched += o.events_matched;
        self.bytes_read += o.bytes_read;
        self.unsealed_events_scanned += o.unsealed_events_scanned;
        for i in &o.indexes {
            if !self.indexes.iter().any(|x| x.field == i.field) {
                self.indexes.push(i.clone());
            }
        }
    }
}

/// Shared state of one query execution: limits, deadline and counters.
pub struct ExecCtx<'a> {
    pub cache: &'a IndexCache,
    pub limits: &'a QueryLimits,
    pub started: Instant,
    pub diag: Diagnostics,
    pub usage: IndexUsage,
}

impl<'a> ExecCtx<'a> {
    pub fn new(cache: &'a IndexCache, limits: &'a QueryLimits) -> Self {
        Self { cache, limits, started: Instant::now(), diag: Diagnostics::default(), usage: IndexUsage::default() }
    }

    fn check_budget(&self) -> Result<(), QueryError> {
        if self.started.elapsed() > self.limits.timeout {
            return Err(QueryError::Timeout(self.limits.timeout.as_millis() as u64));
        }
        if self.diag.bytes_read > self.limits.max_scan_bytes {
            return Err(QueryError::TooExpensive(self.limits.max_scan_bytes));
        }
        Ok(())
    }

    pub fn finish(mut self) -> Diagnostics {
        self.diag.elapsed_ms = self.started.elapsed().as_secs_f64() * 1000.0;
        self.diag.segments_skipped =
            self.diag.segments_skipped_by_time + self.diag.segments_skipped_by_index + self.diag.segments_not_needed;
        self.diag.indexes = std::iter::once(IndexUse { field: "timestamp".into(), kind: "time" })
            .chain(
                self.usage
                    .fields
                    .into_iter()
                    .filter(|(f, _)| f != "timestamp")
                    .map(|(field, kind)| IndexUse { field, kind }),
            )
            .collect();
        self.diag
    }

    /// Candidate blocks of a segment for this query, in time order.
    /// Returns `None` when the whole segment can be skipped.
    fn plan_segment(
        &mut self,
        seg: &Segment,
        signal: Signal,
        filter: Option<&CExpr>,
        range: &TimeRange,
    ) -> Result<Option<Vec<usize>>, QueryError> {
        let s = &seg.summary;
        self.diag.segments_considered += 1;
        if !range.overlaps(s.min_ts, s.max_ts) {
            self.diag.segments_skipped_by_time += 1;
            return Ok(None);
        }
        let mut blocks: Vec<usize> =
            (0..s.blocks.len()).filter(|&i| range.overlaps(s.blocks[i].min_ts, s.blocks[i].max_ts)).collect();
        self.diag.blocks_considered += s.blocks.len();
        if let Some(f) = filter {
            let index = if needs_index(f, signal) { Some(self.cache.get(seg)?) } else { None };
            let ctx = PruneCtx { signal, summary: s, index: index.as_deref() };
            let cand = candidate_blocks(f, &ctx, &mut self.usage);
            blocks.retain(|b| cand.get(*b));
        }
        self.diag.blocks_skipped += s.blocks.len() - blocks.len();
        if blocks.is_empty() {
            self.diag.segments_skipped_by_index += 1;
            return Ok(None);
        }
        Ok(Some(blocks))
    }

    fn read_block(&mut self, seg: &Segment, block: usize, cols: ColumnSet) -> Result<BlockData, QueryError> {
        self.check_budget()?;
        let data = seg.read_block(block, cols)?;
        self.diag.blocks_read += 1;
        self.diag.bytes_read += data.bytes_read;
        Ok(data)
    }
}

/// Lightweight row over decoded segment columns (no body).
struct BlockRow<'a> {
    data: &'a BlockData,
    summary: &'a SegmentSummary,
    i: usize,
}

fn dict(d: &[String], code: u32) -> Option<&str> {
    if code == 0 { None } else { d.get(code as usize - 1).map(String::as_str) }
}

impl Row for BlockRow<'_> {
    fn timestamp(&self) -> Timestamp {
        Timestamp(self.data.ts[self.i])
    }
    fn level(&self) -> Option<Level> {
        Level::from_code(self.data.meta(self.i).level)
    }
    fn service(&self) -> Option<&str> {
        dict(&self.summary.services, self.data.meta(self.i).service)
    }
    fn environment(&self) -> Option<&str> {
        dict(&self.summary.environments, self.data.meta(self.i).environment)
    }
    fn message(&self) -> &str {
        self.data.message(self.i)
    }
    fn trace_id(&self) -> Option<&str> {
        self.data.trace_id(self.i)
    }
    fn span_id(&self) -> Option<&str> {
        self.data.span_id(self.i)
    }
    fn event(&self) -> Option<&TelemetryEvent> {
        None
    }
}

pub struct SearchRequest<'a> {
    pub signal: Signal,
    pub filter: Option<&'a CExpr>,
    pub range: TimeRange,
    pub limit: usize,
    pub direction: Direction,
    pub cursor: Option<Cursor>,
}

pub struct SearchResult {
    pub events: Vec<TelemetryEvent>,
    pub next: Option<Cursor>,
    pub diagnostics: Diagnostics,
}

struct Item {
    key: (i64, u64),
    /// For backward search the heap must pop the *oldest* item first.
    backward: bool,
    event: TelemetryEvent,
}

impl PartialEq for Item {
    fn eq(&self, o: &Self) -> bool {
        self.key == o.key
    }
}
impl Eq for Item {}
impl PartialOrd for Item {
    fn partial_cmp(&self, o: &Self) -> Option<Ordering> {
        Some(self.cmp(o))
    }
}
impl Ord for Item {
    // Max-heap ordered so that the *worst* kept result is on top.
    fn cmp(&self, o: &Self) -> Ordering {
        if self.backward { o.key.cmp(&self.key) } else { self.key.cmp(&o.key) }
    }
}

struct TopK {
    heap: BinaryHeap<Item>,
    cap: usize,
    backward: bool,
}

impl TopK {
    fn full(&self) -> bool {
        self.heap.len() >= self.cap
    }

    fn worst(&self) -> Option<(i64, u64)> {
        if self.full() { self.heap.peek().map(|i| i.key) } else { None }
    }

    /// Could an event with this key enter the result set?
    fn admits(&self, key: (i64, u64)) -> bool {
        match self.worst() {
            None => true,
            Some(w) => {
                if self.backward {
                    key > w
                } else {
                    key < w
                }
            }
        }
    }

    /// Could anything in a time span `[min, max]` enter the result set?
    fn span_can_beat(&self, min: Timestamp, max: Timestamp) -> bool {
        match self.worst() {
            None => true,
            Some((wts, _)) => {
                if self.backward {
                    max.0 >= wts
                } else {
                    min.0 <= wts
                }
            }
        }
    }

    fn push(&mut self, event: TelemetryEvent) {
        let key = (event.timestamp.0, event.id.0);
        self.heap.push(Item { key, backward: self.backward, event });
        if self.heap.len() > self.cap {
            self.heap.pop();
        }
    }
}

fn in_cursor(key: (i64, u64), cursor: Option<Cursor>, dir: Direction) -> bool {
    match cursor {
        None => true,
        Some(c) => match dir {
            Direction::Backward => key < (c.ts, c.id),
            Direction::Forward => key > (c.ts, c.id),
        },
    }
}

/// Ordered search with early termination and cursor pagination.
pub fn search(snapshot: &StreamSnapshot, req: &SearchRequest, ctx: &mut ExecCtx) -> Result<SearchResult, QueryError> {
    let limit = req.limit.clamp(1, ctx.limits.max_results.max(1));
    let backward = req.direction == Direction::Backward;
    // One extra result tells us whether another page exists.
    let mut top = TopK { heap: BinaryHeap::with_capacity(limit + 1), cap: limit + 1, backward };
    let mut range = req.range;
    // Narrow the time range with the cursor so whole segments get skipped.
    if let Some(c) = req.cursor {
        match req.direction {
            Direction::Backward => range.to = range.to.min(Timestamp(c.ts.saturating_add(1))),
            Direction::Forward => range.from = range.from.max(Timestamp(c.ts)),
        }
    }
    let accept = |key: (i64, u64)| range.contains(Timestamp(key.0)) && in_cursor(key, req.cursor, req.direction);

    // Unsealed data: unsorted, bounded by the active segment size.
    for b in &snapshot.batches {
        if !range.overlaps(b.min_ts, b.max_ts) {
            continue;
        }
        for e in &b.events {
            ctx.diag.unsealed_events_scanned += 1;
            let key = (e.timestamp.0, e.id.0);
            if !accept(key) || !top.admits(key) {
                continue;
            }
            ctx.diag.events_examined += 1;
            if req.filter.is_none_or(|f| eval(f, e)) {
                ctx.diag.events_matched += 1;
                top.push(e.clone());
            }
        }
    }

    let mut segments: Vec<&Arc<Segment>> = snapshot.segments.iter().collect();
    if backward {
        segments.sort_by_key(|s| std::cmp::Reverse((s.summary.max_ts, s.summary.id)));
    } else {
        segments.sort_by_key(|s| (s.summary.min_ts, s.summary.id));
    }
    let filter_cols = req.filter.map_or(ColumnSet::new(), CExpr::columns).with(Column::Ids);
    let needs_body = req.filter.is_some_and(CExpr::needs_body);

    for (n, seg) in segments.iter().enumerate() {
        let s = &seg.summary;
        if !top.span_can_beat(s.min_ts, s.max_ts) {
            // Segments are ordered, so none of the rest can contribute.
            let rest = segments.len() - n;
            ctx.diag.segments_considered += rest;
            ctx.diag.segments_not_needed += rest;
            break;
        }
        let Some(mut blocks) = ctx.plan_segment(seg, req.signal, req.filter, &range)? else {
            continue;
        };
        if backward {
            blocks.reverse();
        }
        for b in blocks {
            let bm = &s.blocks[b];
            if !top.span_can_beat(bm.min_ts, bm.max_ts) {
                break;
            }
            let data = ctx.read_block(seg, b, if needs_body { ColumnSet::ALL } else { filter_cols })?;
            let mut full: Option<BlockData> = None;
            let rows: Box<dyn Iterator<Item = usize>> =
                if backward { Box::new((0..data.len).rev()) } else { Box::new(0..data.len) };
            for i in rows {
                let key = (data.ts[i], data.id(i));
                if !accept(key) {
                    continue;
                }
                if !top.admits(key) {
                    // Rows are in time order: nothing later in this
                    // iteration can be admitted either.
                    break;
                }
                ctx.diag.events_examined += 1;
                let event = if needs_body {
                    let e = data.event(s, i)?;
                    if !req.filter.is_none_or(|f| eval(f, &e)) {
                        continue;
                    }
                    e
                } else {
                    let row = BlockRow { data: &data, summary: s, i };
                    if !req.filter.is_none_or(|f| eval(f, &row)) {
                        continue;
                    }
                    // Materialise from a full decode of this block, read once.
                    if full.is_none() {
                        full = Some(ctx.read_block(seg, b, ColumnSet::ALL)?);
                    }
                    full.as_ref().unwrap().event(s, i)?
                };
                ctx.diag.events_matched += 1;
                top.push(event);
            }
        }
    }

    let mut items: Vec<Item> = top.heap.into_vec();
    items.sort_by(|a, b| if backward { b.key.cmp(&a.key) } else { a.key.cmp(&b.key) });
    let more = items.len() > limit;
    items.truncate(limit);
    let next = if more { items.last().map(|i| Cursor { ts: i.key.0, id: i.key.1 }) } else { None };
    Ok(SearchResult { events: items.into_iter().map(|i| i.event).collect(), next, diagnostics: Diagnostics::default() })
}

/// Receives every matching event of an unordered scan.
pub trait Sink {
    /// Columns the sink reads from rows (beyond what the filter needs).
    fn columns(&self) -> ColumnSet {
        ColumnSet::new()
    }

    /// Whether rows must carry the full event.
    fn needs_event(&self) -> bool {
        self.columns().contains(Column::Body)
    }

    /// Fast path for unfiltered scans: a whole block lies inside the range
    /// and nothing needs inspecting per event. Return `true` if handled.
    fn block_count(&mut self, _min: Timestamp, _max: Timestamp, _count: u64) -> bool {
        false
    }

    fn row(&mut self, row: &dyn Row) -> ControlFlow<()>;
}

/// Visit every matching event in `range` (no particular order).
pub fn scan(
    snapshot: &StreamSnapshot,
    signal: Signal,
    filter: Option<&CExpr>,
    range: TimeRange,
    sink: &mut dyn Sink,
    ctx: &mut ExecCtx,
) -> Result<(), QueryError> {
    for b in &snapshot.batches {
        if !range.overlaps(b.min_ts, b.max_ts) {
            continue;
        }
        for e in &b.events {
            ctx.diag.unsealed_events_scanned += 1;
            if !range.contains(e.timestamp) {
                continue;
            }
            ctx.diag.events_examined += 1;
            if filter.is_none_or(|f| eval(f, e)) {
                ctx.diag.events_matched += 1;
                if sink.row(e).is_break() {
                    return Ok(());
                }
            }
        }
    }

    let cols = filter.map_or(ColumnSet::new(), CExpr::columns).union(sink.columns());
    let needs_body = cols.contains(Column::Body) || sink.needs_event();
    let cols = if needs_body { ColumnSet::ALL } else { cols };

    for seg in &snapshot.segments {
        let Some(blocks) = ctx.plan_segment(seg, signal, filter, &range)? else {
            continue;
        };
        let s = &seg.summary;
        for b in blocks {
            let bm = &s.blocks[b];
            if filter.is_none()
                && range.covers(bm.min_ts, bm.max_ts)
                && sink.block_count(bm.min_ts, bm.max_ts, bm.count as u64)
            {
                ctx.diag.events_matched += bm.count as u64;
                continue;
            }
            let data = ctx.read_block(seg, b, cols)?;
            for i in 0..data.len {
                if !range.contains(Timestamp(data.ts[i])) {
                    continue;
                }
                ctx.diag.events_examined += 1;
                let flow = if needs_body {
                    let e = data.event(s, i)?;
                    if !filter.is_none_or(|f| eval(f, &e)) {
                        continue;
                    }
                    ctx.diag.events_matched += 1;
                    sink.row(&e)
                } else {
                    let row = BlockRow { data: &data, summary: s, i };
                    if !filter.is_none_or(|f| eval(f, &row)) {
                        continue;
                    }
                    ctx.diag.events_matched += 1;
                    sink.row(&row)
                };
                if flow.is_break() {
                    return Ok(());
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursor_round_trip() {
        for c in [Cursor { ts: 0, id: 0 }, Cursor { ts: -5, id: 7 }, Cursor { ts: i64::MAX, id: u64::MAX }] {
            assert_eq!(Cursor::decode(&c.encode()), Some(c));
        }
        assert_eq!(Cursor::decode("garbage"), None);
        assert_eq!(Cursor::decode("v1.1.2.3"), None);
        assert_eq!(Cursor::decode("v2.1.2"), None);
    }

    #[test]
    fn time_range_semantics() {
        let r = TimeRange::new(Some(Timestamp(10)), Some(Timestamp(20)));
        assert!(r.contains(Timestamp(10)) && !r.contains(Timestamp(20)));
        assert!(r.overlaps(Timestamp(0), Timestamp(10)));
        assert!(!r.overlaps(Timestamp(20), Timestamp(30)));
        assert!(r.covers(Timestamp(11), Timestamp(19)));
        assert!(!r.covers(Timestamp(5), Timestamp(19)));
    }
}
