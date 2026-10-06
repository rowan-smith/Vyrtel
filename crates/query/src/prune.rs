//! Index-based skipping.
//!
//! For a segment we compute a bitmap of blocks that *may* contain matches.
//! Every rule here must over-approximate: a block may only be dropped if
//! the evaluator would reject every event in it. False positives cost a
//! block decode; false negatives lose results. The randomized tests in
//! `tests/correctness.rs` compare indexed results with brute force.
//!
//! * `And` → intersection, `Or` → union, `Not` → all blocks (no pruning)
//! * level / service / environment → bitmap indexes; the comparison is
//!   evaluated against each dictionary entry with the evaluator itself
//! * equality on indexed paths → Bloom filters (canonical keys)
//! * numeric literals → zone maps (per-block min/max)
//! * timestamp comparisons → block time ranges

use std::collections::BTreeMap;

use storage::index::Bitmap;
use storage::index::keys;
use storage::segment::{SegmentIndex, SegmentSummary};
use telemetry::Signal;

use crate::ast::{Literal, Op};
use crate::eval::{CExpr, Cmp, level_matches, text_matches};
use crate::fields::FieldRef;

/// Which index structures a query used, for diagnostics.
#[derive(Debug, Clone, Default)]
pub struct IndexUsage {
    /// Name → kind ("bitmap", "bloom", "zonemap", "time", "none").
    pub fields: BTreeMap<String, &'static str>,
}

impl IndexUsage {
    fn note(&mut self, field: String, kind: &'static str) {
        let e = self.fields.entry(field).or_insert(kind);
        // Prefer reporting a real index over "none" when both apply.
        if *e == "none" {
            *e = kind;
        }
    }
}

/// Record every field the expression references, so diagnostics and query
/// statistics list them even when no segment was consulted.
pub fn note_fields(e: &CExpr, usage: &mut IndexUsage) {
    match e {
        CExpr::And(a, b) | CExpr::Or(a, b) => {
            note_fields(a, usage);
            note_fields(b, usage);
        }
        CExpr::Not(a) => note_fields(a, usage),
        CExpr::Cmp(c) => usage.note(c.field.name(), "none"),
    }
}

/// Does the expression benefit from the lazily loaded index region?
pub fn needs_index(e: &CExpr, signal: Signal) -> bool {
    match e {
        CExpr::And(a, b) | CExpr::Or(a, b) => needs_index(a, signal) || needs_index(b, signal),
        CExpr::Not(_) => false,
        CExpr::Cmp(c) => match c.op {
            Op::Eq => c.field.bloom_path(signal).is_some() || (c.lit.is_number() && c.field.zone_path().is_some()),
            Op::Gt | Op::Ge | Op::Lt | Op::Le => c.lit.is_number() && c.field.zone_path().is_some(),
            _ => false,
        },
    }
}

pub struct PruneCtx<'a> {
    pub signal: Signal,
    pub summary: &'a SegmentSummary,
    pub index: Option<&'a SegmentIndex>,
}

impl PruneCtx<'_> {
    fn all(&self) -> Bitmap {
        Bitmap::full(self.summary.blocks.len())
    }

    fn filter_blocks(&self, mut keep: impl FnMut(usize) -> bool) -> Bitmap {
        let mut b = Bitmap::with_len(self.summary.blocks.len());
        for i in 0..self.summary.blocks.len() {
            if keep(i) {
                b.set(i);
            }
        }
        b
    }
}

/// Blocks of the segment that may contain events matching `e`.
pub fn candidate_blocks(e: &CExpr, ctx: &PruneCtx, usage: &mut IndexUsage) -> Bitmap {
    match e {
        CExpr::And(a, b) => {
            let mut x = candidate_blocks(a, ctx, usage);
            if x.is_empty() {
                return x;
            }
            x.and_with(&candidate_blocks(b, ctx, usage));
            x
        }
        CExpr::Or(a, b) => {
            let mut x = candidate_blocks(a, ctx, usage);
            x.or_with(&candidate_blocks(b, ctx, usage));
            x
        }
        // Negation cannot be answered by "may contain" indexes.
        CExpr::Not(_) => ctx.all(),
        CExpr::Cmp(c) => comparison_blocks(c, ctx, usage),
    }
}

fn comparison_blocks(c: &Cmp, ctx: &PruneCtx, usage: &mut IndexUsage) -> Bitmap {
    let s = ctx.summary;
    match &c.field {
        FieldRef::Level => {
            usage.note("level".into(), "bitmap");
            let mut out = Bitmap::new();
            for (code, blocks) in s.level_blocks.iter().enumerate() {
                if level_matches(code as u8, c) {
                    out.or_with(blocks);
                }
            }
            out
        }
        FieldRef::Service => dictionary_blocks(c, &s.services, &s.service_blocks, ctx, usage, "service"),
        FieldRef::Environment => {
            dictionary_blocks(c, &s.environments, &s.environment_blocks, ctx, usage, "environment")
        }
        FieldRef::Timestamp => {
            usage.note("timestamp".into(), "time");
            let Some(t) = time_literal(c) else {
                return ctx.all();
            };
            ctx.filter_blocks(|i| {
                let b = &s.blocks[i];
                match c.op {
                    Op::Eq => b.min_ts <= t && t <= b.max_ts,
                    Op::Gt => b.max_ts > t,
                    Op::Ge => b.max_ts >= t,
                    Op::Lt => b.min_ts < t,
                    Op::Le => b.min_ts <= t,
                    Op::Ne | Op::Contains => true,
                }
            })
        }
        field => {
            let name = field.name();
            let Some(index) = ctx.index else {
                usage.note(name, "none");
                return ctx.all();
            };
            match c.op {
                Op::Eq => {
                    let mut result: Option<Bitmap> = None;
                    if let Some(path) = field.bloom_path(ctx.signal)
                        && let Some(probes) = bloom_probes(field, path, c)
                    {
                        usage.note(name.clone(), "bloom");
                        result = Some(ctx.filter_blocks(|i| {
                            let bloom = &index.blocks[i].bloom;
                            probes.iter().any(|k| bloom.may_contain(*k))
                        }));
                    }
                    if let (Some(n), Some(path)) = (c.lit.as_f64(), field.zone_path())
                        && let Some(z) = zone_blocks(index, path, ctx, |min, max| min <= n && n <= max)
                    {
                        usage.note(name.clone(), "zonemap");
                        result = Some(match result {
                            Some(mut r) => {
                                r.and_with(&z);
                                r
                            }
                            None => z,
                        });
                    }
                    result.unwrap_or_else(|| {
                        usage.note(name, "none");
                        ctx.all()
                    })
                }
                Op::Gt | Op::Ge | Op::Lt | Op::Le if c.lit.is_number() => {
                    let n = c.lit.as_f64().unwrap_or(0.0);
                    let op = c.op;
                    let overlaps = move |min: f64, max: f64| match op {
                        Op::Gt => max > n,
                        Op::Ge => max >= n,
                        Op::Lt => min < n,
                        Op::Le => min <= n,
                        _ => true,
                    };
                    match field.zone_path().and_then(|p| zone_blocks(index, p, ctx, overlaps)) {
                        Some(z) => {
                            usage.note(name, "zonemap");
                            z
                        }
                        None => {
                            usage.note(name, "none");
                            ctx.all()
                        }
                    }
                }
                _ => {
                    usage.note(name, "none");
                    ctx.all()
                }
            }
        }
    }
}

/// Service / environment: evaluate the comparison against each dictionary
/// value and union the bitmaps of those that match.
fn dictionary_blocks(
    c: &Cmp,
    dict: &[String],
    bitmaps: &[Bitmap],
    ctx: &PruneCtx,
    usage: &mut IndexUsage,
    name: &str,
) -> Bitmap {
    usage.note(name.into(), "bitmap");
    // Events without a value are not tracked by a bitmap; if they could
    // match (e.g. `service != "x"`), we cannot prune.
    if text_matches(None, c) {
        return ctx.all();
    }
    let mut out = Bitmap::new();
    for (value, blocks) in dict.iter().zip(bitmaps) {
        if text_matches(Some(value), c) {
            out.or_with(blocks);
        }
    }
    out
}

fn bloom_probes(field: &FieldRef, path: &str, c: &Cmp) -> Option<Vec<u64>> {
    match field {
        // Identifiers are normalised by the evaluator; probe the same form.
        FieldRef::TraceId | FieldRef::SpanId | FieldRef::ParentSpanId => {
            if c.lit == Literal::Null {
                return None;
            }
            let text = telemetry::normalize_id(c.lit.as_text().trim());
            keys::probe_keys(path, &telemetry::Value::String(text))
        }
        _ => keys::probe_keys(path, &c.lit.to_value()),
    }
}

/// Blocks whose zone for `path` satisfies `overlaps`. `None` when the
/// field has no zone map in this segment (not tracked → cannot prune).
fn zone_blocks(
    index: &SegmentIndex,
    path: &str,
    ctx: &PruneCtx,
    overlaps: impl Fn(f64, f64) -> bool,
) -> Option<Bitmap> {
    let z = index.zone_field(path)?;
    Some(ctx.filter_blocks(|i| {
        // A tracked field with no zone entry has no numeric values in the
        // block, so no numeric comparison can match there.
        index.blocks[i].zones.iter().find(|(f, _, _)| *f == z).is_some_and(|(_, min, max)| overlaps(*min, *max))
    }))
}

fn time_literal(c: &Cmp) -> Option<telemetry::Timestamp> {
    match &c.lit {
        Literal::String(s) => telemetry::Timestamp::parse_rfc3339(s),
        Literal::Int(i) => telemetry::Timestamp::from_unix_number(*i as f64),
        Literal::Float(f) => telemetry::Timestamp::from_unix_number(*f),
        _ => None,
    }
}
