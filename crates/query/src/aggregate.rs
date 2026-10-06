//! Aggregations: counts, histograms, facets (filter panel) and metrics.

use std::collections::HashMap;
use std::ops::ControlFlow;

use serde::Serialize;
use storage::segment::{Column, ColumnSet};
use telemetry::*;

use crate::eval::Row;
use crate::exec::{Sink, TimeRange};

pub struct CountSink {
    pub count: u64,
}

impl Sink for CountSink {
    fn block_count(&mut self, _min: Timestamp, _max: Timestamp, count: u64) -> bool {
        self.count += count;
        true
    }

    fn row(&mut self, _row: &dyn Row) -> ControlFlow<()> {
        self.count += 1;
        ControlFlow::Continue(())
    }
}

const NICE_STEPS_MS: &[i64] = &[
    1_000,
    2_000,
    5_000,
    10_000,
    15_000,
    30_000,
    60_000,
    120_000,
    300_000,
    600_000,
    900_000,
    1_800_000,
    3_600_000,
    7_200_000,
    10_800_000,
    21_600_000,
    43_200_000,
    86_400_000,
    172_800_000,
    604_800_000,
];

/// Pick a human-friendly bucket width giving at most `max_buckets`.
pub fn choose_step_ms(range: &TimeRange, max_buckets: usize) -> i64 {
    let span_ms = (range.to.0.saturating_sub(range.from.0) / NANOS_PER_MILLI).max(1);
    let want = span_ms / max_buckets.max(1) as i64;
    NICE_STEPS_MS.iter().copied().find(|s| *s >= want).unwrap_or_else(|| (want / 86_400_000 + 1) * 86_400_000)
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Bucket {
    pub start: Timestamp,
    pub count: u64,
    /// Counts per level for logs: `[none, trace, debug, info, warn, error, fatal]`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub levels: Option<[u64; 7]>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Histogram {
    pub step_ms: i64,
    pub buckets: Vec<Bucket>,
    pub total: u64,
}

pub struct HistogramSink {
    start: i64,
    step: i64,
    with_levels: bool,
    buckets: Vec<Bucket>,
    total: u64,
}

impl HistogramSink {
    /// `range` must be bounded.
    pub fn new(range: &TimeRange, step_ms: i64, with_levels: bool) -> Self {
        let step = step_ms * NANOS_PER_MILLI;
        let start = range.from.0.div_euclid(step) * step;
        let n = ((range.to.0 - start) as i128 + step as i128 - 1) / step as i128;
        let n = n.clamp(1, 10_000) as usize;
        let buckets = (0..n)
            .map(|i| Bucket {
                start: Timestamp(start + i as i64 * step),
                count: 0,
                levels: with_levels.then_some([0; 7]),
            })
            .collect();
        Self { start, step, with_levels, buckets, total: 0 }
    }

    pub fn finish(self) -> Histogram {
        Histogram { step_ms: self.step / NANOS_PER_MILLI, buckets: self.buckets, total: self.total }
    }
}

impl Sink for HistogramSink {
    fn columns(&self) -> ColumnSet {
        if self.with_levels { ColumnSet::new().with(Column::Meta) } else { ColumnSet::new() }
    }

    fn block_count(&mut self, min: Timestamp, max: Timestamp, count: u64) -> bool {
        // Only when the whole block falls into a single bucket.
        if self.with_levels {
            return false;
        }
        let a = (min.0 - self.start).div_euclid(self.step);
        let b = (max.0 - self.start).div_euclid(self.step);
        if a != b || a < 0 || a as usize >= self.buckets.len() {
            return false;
        }
        self.buckets[a as usize].count += count;
        self.total += count;
        true
    }

    fn row(&mut self, row: &dyn Row) -> ControlFlow<()> {
        let i = (row.timestamp().0 - self.start).div_euclid(self.step);
        if i >= 0 && (i as usize) < self.buckets.len() {
            let b = &mut self.buckets[i as usize];
            b.count += 1;
            if let Some(levels) = b.levels.as_mut() {
                levels[row.level().map_or(0, |l| l.code() as usize)] += 1;
            }
            self.total += 1;
        }
        ControlFlow::Continue(())
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FacetValue {
    pub value: Value,
    pub count: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Facet {
    pub field: String,
    /// Events (in the sample) that have this field.
    pub count: u64,
    pub values: Vec<FacetValue>,
    /// True if more distinct values exist than are listed.
    pub truncated: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Facets {
    /// Number of (most recent) matching events the facets were computed on.
    pub sampled: u64,
    pub fields: Vec<Facet>,
}

const MAX_FACET_FIELDS: usize = 64;
const MAX_TRACKED_VALUES: usize = 200;

struct FieldCounter {
    events: u64,
    values: HashMap<String, (Value, u64)>,
    overflow: bool,
}

/// Facets over a sample of events: well-known fields first, then the most
/// common structured properties.
pub fn facets(events: &[TelemetryEvent], top_values: usize) -> Facets {
    let mut fields: Vec<(String, FieldCounter)> = Vec::new();
    let mut index: HashMap<String, usize> = HashMap::new();
    let mut add = |field: &str, v: Value, seen: &mut Vec<usize>| {
        let i = match index.get(field) {
            Some(i) => *i,
            None => {
                if fields.len() >= MAX_FACET_FIELDS * 4 {
                    return;
                }
                fields.push((field.to_string(), FieldCounter { events: 0, values: HashMap::new(), overflow: false }));
                index.insert(field.to_string(), fields.len() - 1);
                fields.len() - 1
            }
        };
        let fc = &mut fields[i].1;
        if !seen.contains(&i) {
            fc.events += 1;
            seen.push(i);
        }
        let key = v.to_display_string();
        if let Some(e) = fc.values.get_mut(&key) {
            e.1 += 1;
        } else if fc.values.len() < MAX_TRACKED_VALUES {
            fc.values.insert(key, (v, 1));
        } else {
            fc.overflow = true;
        }
    };
    for e in events {
        let mut seen = Vec::new();
        if let Some(l) = e.level() {
            add("level", Value::from(l.as_str()), &mut seen);
        }
        if let Some(s) = &e.service {
            add("service", Value::from(s.as_str()), &mut seen);
        }
        if let Some(s) = &e.environment {
            add("environment", Value::from(s.as_str()), &mut seen);
        }
        match &e.payload {
            TelemetryPayload::Span(s) => {
                add("name", Value::from(s.name.as_str()), &mut seen);
                add("status", Value::from(s.status.code.as_str()), &mut seen);
            }
            TelemetryPayload::Log(l) => {
                if let Some(t) = l.exception.as_ref().and_then(|x| x.kind.as_deref()) {
                    add("exception.type", Value::from(t), &mut seen);
                }
            }
            TelemetryPayload::Metric(m) => add("name", Value::from(m.name.as_str()), &mut seen),
        }
        e.attributes.for_each_leaf(&mut |p, v| {
            if !matches!(v, Value::Null) {
                add(p, v.clone(), &mut seen);
            }
        });
    }
    let well_known = ["level", "service", "environment", "name", "status", "exception.type"];
    let mut out: Vec<Facet> = fields
        .into_iter()
        .map(|(field, fc)| {
            let mut values: Vec<FacetValue> =
                fc.values.into_values().map(|(value, count)| FacetValue { value, count }).collect();
            values.sort_by(|a, b| {
                b.count.cmp(&a.count).then_with(|| a.value.to_display_string().cmp(&b.value.to_display_string()))
            });
            let truncated = fc.overflow || values.len() > top_values;
            values.truncate(top_values);
            Facet { field, count: fc.events, values, truncated }
        })
        .collect();
    out.sort_by(|a, b| {
        let wa = well_known.iter().position(|w| *w == a.field).unwrap_or(usize::MAX);
        let wb = well_known.iter().position(|w| *w == b.field).unwrap_or(usize::MAX);
        wa.cmp(&wb).then(b.count.cmp(&a.count)).then(a.field.cmp(&b.field))
    });
    out.truncate(MAX_FACET_FIELDS);
    Facets { sampled: events.len() as u64, fields: out }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize, Serialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Agg {
    Sum,
    Count,
    Min,
    Max,
    #[default]
    Avg,
    Last,
}

#[derive(Debug, Clone, Copy, Default)]
struct Acc {
    sum: f64,
    count: u64,
    min: f64,
    max: f64,
    last_ts: i64,
    last: f64,
    seen: bool,
}

impl Acc {
    fn add(&mut self, ts: i64, sum: f64, count: u64, min: f64, max: f64, last: f64) {
        if !self.seen {
            self.min = min;
            self.max = max;
            self.seen = true;
        } else {
            self.min = self.min.min(min);
            self.max = self.max.max(max);
        }
        self.sum += sum;
        self.count += count;
        if ts >= self.last_ts {
            self.last_ts = ts;
            self.last = last;
        }
    }

    fn value(&self, agg: Agg) -> Option<f64> {
        if !self.seen {
            return None;
        }
        Some(match agg {
            Agg::Sum => self.sum,
            Agg::Count => self.count as f64,
            Agg::Min => self.min,
            Agg::Max => self.max,
            Agg::Avg => {
                if self.count == 0 {
                    return None;
                }
                self.sum / self.count as f64
            }
            Agg::Last => self.last,
        })
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Point {
    pub ts: Timestamp,
    pub value: Option<f64>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Series {
    pub group: Option<String>,
    pub points: Vec<Point>,
}

#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct MetricMeta {
    pub unit: Option<String>,
    pub kind: Option<&'static str>,
    pub description: Option<String>,
}

const MAX_SERIES: usize = 20;

pub struct MetricSink {
    start: i64,
    step: i64,
    buckets: usize,
    group_by: Option<String>,
    groups: Vec<(Option<String>, Vec<Acc>)>,
    pub meta: MetricMeta,
}

impl MetricSink {
    pub fn new(range: &TimeRange, step_ms: i64, group_by: Option<String>) -> Self {
        let step = step_ms * NANOS_PER_MILLI;
        let start = range.from.0.div_euclid(step) * step;
        let n = (((range.to.0 - start) as i128 + step as i128 - 1) / step as i128).clamp(1, 10_000) as usize;
        Self { start, step, buckets: n, group_by, groups: Vec::new(), meta: MetricMeta::default() }
    }

    fn group_slot(&mut self, key: Option<String>) -> usize {
        if let Some(i) = self.groups.iter().position(|(g, _)| *g == key) {
            return i;
        }
        if self.groups.len() >= MAX_SERIES {
            let other = Some("(other)".to_string());
            if let Some(i) = self.groups.iter().position(|(g, _)| *g == other) {
                return i;
            }
            self.groups.push((other, vec![Acc::default(); self.buckets]));
            return self.groups.len() - 1;
        }
        self.groups.push((key, vec![Acc::default(); self.buckets]));
        self.groups.len() - 1
    }

    pub fn finish(mut self, agg: Agg) -> (i64, Vec<Series>) {
        self.groups.sort_by(|a, b| a.0.cmp(&b.0));
        let series = self
            .groups
            .into_iter()
            .map(|(group, accs)| Series {
                group,
                points: accs
                    .iter()
                    .enumerate()
                    .map(|(i, a)| Point { ts: Timestamp(self.start + i as i64 * self.step), value: a.value(agg) })
                    .collect(),
            })
            .collect();
        (self.step / NANOS_PER_MILLI, series)
    }
}

impl Sink for MetricSink {
    fn columns(&self) -> ColumnSet {
        ColumnSet::ALL
    }

    fn row(&mut self, row: &dyn Row) -> ControlFlow<()> {
        let Some(e) = row.event() else {
            return ControlFlow::Continue(());
        };
        let Some(m) = e.as_metric() else {
            return ControlFlow::Continue(());
        };
        if self.meta.kind.is_none() {
            self.meta =
                MetricMeta { unit: m.unit.clone(), kind: Some(m.kind.as_str()), description: m.description.clone() };
        }
        let i = (e.timestamp.0 - self.start).div_euclid(self.step);
        if i < 0 || i as usize >= self.buckets {
            return ControlFlow::Continue(());
        }
        let key = self.group_by.as_ref().map(|g| {
            e.attributes
                .lookup_path(g)
                .or_else(|| e.resource.lookup_path(g))
                .map_or_else(|| "(none)".to_string(), Value::to_display_string)
        });
        let slot = self.group_slot(key);
        let acc = &mut self.groups[slot].1[i as usize];
        match &m.value {
            MetricValue::Number(v) if v.is_finite() => acc.add(e.timestamp.0, *v, 1, *v, *v, *v),
            MetricValue::Histogram(h) => {
                let sum = h.sum.unwrap_or(0.0);
                let avg = if h.count > 0 { sum / h.count as f64 } else { 0.0 };
                acc.add(e.timestamp.0, sum, h.count, h.min.unwrap_or(avg), h.max.unwrap_or(avg), avg);
            }
            _ => {}
        }
        ControlFlow::Continue(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn step_selection() {
        let hour = TimeRange::new(Some(Timestamp(0)), Some(Timestamp::from_secs(3600)));
        assert_eq!(choose_step_ms(&hour, 60), 60_000);
        assert_eq!(choose_step_ms(&hour, 100), 60_000);
        let day = TimeRange::new(Some(Timestamp(0)), Some(Timestamp::from_secs(86_400)));
        assert_eq!(choose_step_ms(&day, 100), 900_000);
        let tiny = TimeRange::new(Some(Timestamp(0)), Some(Timestamp(5)));
        assert_eq!(choose_step_ms(&tiny, 100), 1_000);
    }

    #[test]
    fn accumulator_aggregations() {
        let mut a = Acc::default();
        assert_eq!(a.value(Agg::Sum), None);
        a.add(1, 2.0, 1, 2.0, 2.0, 2.0);
        a.add(3, 6.0, 1, 6.0, 6.0, 6.0);
        a.add(2, 4.0, 1, 4.0, 4.0, 4.0);
        assert_eq!(a.value(Agg::Sum), Some(12.0));
        assert_eq!(a.value(Agg::Count), Some(3.0));
        assert_eq!(a.value(Agg::Min), Some(2.0));
        assert_eq!(a.value(Agg::Max), Some(6.0));
        assert_eq!(a.value(Agg::Avg), Some(4.0));
        assert_eq!(a.value(Agg::Last), Some(6.0));
    }

    #[test]
    fn histogram_buckets_align() {
        let r = TimeRange::new(Some(Timestamp::from_secs(65)), Some(Timestamp::from_secs(185)));
        let h = HistogramSink::new(&r, 60_000, false).finish();
        assert_eq!(h.buckets.len(), 3);
        assert_eq!(h.buckets[0].start, Timestamp::from_secs(60));
    }
}
