//! High-level query API used by the server.

use std::collections::HashMap;
use std::sync::Arc;

use serde::Serialize;
use storage::Storage;
use telemetry::*;

use crate::aggregate::{self, Agg, CountSink, Facets, Histogram, HistogramSink, MetricMeta, MetricSink, Series};
use crate::ast::Expr;
use crate::error::QueryError;
use crate::eval::{CExpr, compile};
use crate::exec::{self, Cursor, Diagnostics, Direction, ExecCtx, QueryLimits, SearchRequest, TimeRange};
use crate::parser::parse;
use crate::stats::QueryStats;
use crate::traces::{self, SpanCollector, TraceSummary};

pub struct Engine {
    storage: Arc<Storage>,
    limits: QueryLimits,
    stats: QueryStats,
}

pub struct SearchOutput {
    pub events: Vec<TelemetryEvent>,
    pub next: Option<Cursor>,
    pub diagnostics: Diagnostics,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MetricInfo {
    pub name: String,
}

pub struct MetricQuery {
    pub name: String,
    pub filter: Option<String>,
    pub range: TimeRange,
    pub step_ms: Option<i64>,
    pub agg: Agg,
    pub group_by: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MetricOutput {
    pub name: String,
    pub agg: Agg,
    pub step_ms: i64,
    #[serde(flatten)]
    pub meta: MetricMeta,
    pub series: Vec<Series>,
    pub diagnostics: Diagnostics,
}

/// Parse and compile a query string for a signal. `Ok(None)` matches all.
pub fn compile_query(q: &str, signal: Signal) -> Result<Option<CExpr>, QueryError> {
    Ok(parse(q)?.map(|e| compile(&e, signal)))
}

fn and(a: Expr, b: Option<Expr>) -> Expr {
    match b {
        Some(b) => Expr::and(a, b),
        None => a,
    }
}

impl Engine {
    pub fn new(storage: Arc<Storage>, limits: QueryLimits) -> Self {
        Self { storage, limits, stats: QueryStats::default() }
    }

    pub fn storage(&self) -> &Arc<Storage> {
        &self.storage
    }

    pub fn limits(&self) -> &QueryLimits {
        &self.limits
    }

    pub fn stats(&self) -> &QueryStats {
        &self.stats
    }

    fn ctx(&self) -> ExecCtx<'_> {
        ExecCtx::new(self.storage.cache(), &self.limits)
    }

    fn finish(&self, signal: Signal, mut ctx: ExecCtx, filter: Option<&CExpr>) -> Diagnostics {
        if let Some(f) = filter {
            crate::prune::note_fields(f, &mut ctx.usage);
        }
        let d = ctx.finish();
        self.stats.record(signal.as_str(), &d);
        d
    }

    pub fn search(
        &self,
        signal: Signal,
        query: &str,
        range: TimeRange,
        limit: usize,
        direction: Direction,
        cursor: Option<Cursor>,
    ) -> Result<SearchOutput, QueryError> {
        let filter = compile_query(query, signal)?;
        let snapshot = self.storage.snapshot(signal);
        let mut ctx = self.ctx();
        let r = exec::search(
            &snapshot,
            &SearchRequest { signal, filter: filter.as_ref(), range, limit, direction, cursor },
            &mut ctx,
        )?;
        Ok(SearchOutput { events: r.events, next: r.next, diagnostics: self.finish(signal, ctx, filter.as_ref()) })
    }

    pub fn count(&self, signal: Signal, query: &str, range: TimeRange) -> Result<(u64, Diagnostics), QueryError> {
        let filter = compile_query(query, signal)?;
        let snapshot = self.storage.snapshot(signal);
        let mut ctx = self.ctx();
        let mut sink = CountSink { count: 0 };
        exec::scan(&snapshot, signal, filter.as_ref(), range, &mut sink, &mut ctx)?;
        Ok((sink.count, self.finish(signal, ctx, filter.as_ref())))
    }

    /// Count over time. `range` must be bounded.
    pub fn histogram(
        &self,
        signal: Signal,
        query: &str,
        range: TimeRange,
        max_buckets: usize,
    ) -> Result<(Histogram, Diagnostics), QueryError> {
        let filter = compile_query(query, signal)?;
        let step = aggregate::choose_step_ms(&range, max_buckets);
        let snapshot = self.storage.snapshot(signal);
        let mut ctx = self.ctx();
        let mut sink = HistogramSink::new(&range, step, signal == Signal::Logs);
        exec::scan(&snapshot, signal, filter.as_ref(), range, &mut sink, &mut ctx)?;
        Ok((sink.finish(), self.finish(signal, ctx, filter.as_ref())))
    }

    /// Field/value counts over the most recent `sample` matching events.
    pub fn facets(
        &self,
        signal: Signal,
        query: &str,
        range: TimeRange,
        sample: usize,
    ) -> Result<(Facets, Diagnostics), QueryError> {
        let filter = compile_query(query, signal)?;
        let snapshot = self.storage.snapshot(signal);
        let mut ctx = self.ctx();
        let limits = QueryLimits { max_results: sample, ..self.limits.clone() };
        let mut inner = ExecCtx::new(self.storage.cache(), &limits);
        let r = exec::search(
            &snapshot,
            &SearchRequest {
                signal,
                filter: filter.as_ref(),
                range,
                limit: sample,
                direction: Direction::Backward,
                cursor: None,
            },
            &mut inner,
        )?;
        ctx.diag.merge(&inner.finish());
        Ok((aggregate::facets(&r.events, 10), self.finish(signal, ctx, filter.as_ref())))
    }

    /// Distinct metric names, newest data first.
    pub fn metric_names(&self) -> Vec<MetricInfo> {
        let snap = self.storage.snapshot(Signal::Metrics);
        let mut names: HashMap<String, ()> = HashMap::new();
        for s in &snap.segments {
            if let Some(n) = &s.summary.names {
                for name in n {
                    names.insert(name.clone(), ());
                }
            }
        }
        for b in &snap.batches {
            for e in &b.events {
                names.insert(e.message().to_string(), ());
            }
        }
        let mut v: Vec<MetricInfo> = names.into_keys().map(|name| MetricInfo { name }).collect();
        v.sort_by(|a, b| a.name.cmp(&b.name));
        v
    }

    pub fn metric_query(&self, q: &MetricQuery) -> Result<MetricOutput, QueryError> {
        if q.name.trim().is_empty() {
            return Err(QueryError::InvalidRequest("metric name is required".into()));
        }
        let extra = match &q.filter {
            Some(f) => parse(f)?,
            None => None,
        };
        let expr = and(Expr::cmp("name", crate::ast::Op::Eq, crate::ast::Literal::String(q.name.clone())), extra);
        let filter = compile(&expr, Signal::Metrics);
        let step = q.step_ms.filter(|s| *s >= 1000).unwrap_or_else(|| aggregate::choose_step_ms(&q.range, 120));
        let snapshot = self.storage.snapshot(Signal::Metrics);
        let mut ctx = self.ctx();
        let mut sink = MetricSink::new(&q.range, step, q.group_by.clone().filter(|g| !g.is_empty()));
        exec::scan(&snapshot, Signal::Metrics, Some(&filter), q.range, &mut sink, &mut ctx)?;
        let meta = sink.meta.clone();
        let (step_ms, series) = sink.finish(q.agg);
        Ok(MetricOutput {
            name: q.name.clone(),
            agg: q.agg,
            step_ms,
            meta,
            series,
            diagnostics: self.finish(Signal::Metrics, ctx, Some(&filter)),
        })
    }

    /// Find recent traces with at least one span matching `query`.
    pub fn trace_search(
        &self,
        query: &str,
        range: TimeRange,
        limit: usize,
    ) -> Result<(Vec<TraceSummary>, Diagnostics), QueryError> {
        let filter = compile_query(query, Signal::Traces)?;
        let snapshot = self.storage.snapshot(Signal::Traces);
        let mut ctx = self.ctx();
        let limit = limit.clamp(1, 200);

        // 1. Newest matching spans → distinct trace ids.
        let mut order: Vec<String> = Vec::new();
        let mut seen: HashMap<String, (Timestamp, Timestamp)> = HashMap::new();
        let mut cursor = None;
        let page_limits = QueryLimits { max_results: 500, ..self.limits.clone() };
        for _ in 0..20 {
            let mut page_ctx = ExecCtx::new(self.storage.cache(), &page_limits);
            let r = exec::search(
                &snapshot,
                &SearchRequest {
                    signal: Signal::Traces,
                    filter: filter.as_ref(),
                    range,
                    limit: 500,
                    direction: Direction::Backward,
                    cursor,
                },
                &mut page_ctx,
            )?;
            ctx.diag.merge(&page_ctx.finish());
            for e in &r.events {
                if let Some(t) = &e.trace_id {
                    let id = t.as_str().to_string();
                    let entry = seen.entry(id.clone()).or_insert_with(|| {
                        order.push(id);
                        (e.timestamp, e.timestamp)
                    });
                    entry.0 = entry.0.min(e.timestamp);
                    entry.1 = entry.1.max(e.timestamp);
                }
            }
            cursor = r.next;
            if order.len() >= limit || cursor.is_none() {
                break;
            }
        }
        order.truncate(limit);
        if order.is_empty() {
            return Ok((Vec::new(), self.finish(Signal::Traces, ctx, filter.as_ref())));
        }

        // 2. All spans of those traces. Spans of one trace are close in
        //    time, so widen the window by an hour on each side.
        let hour = 3_600 * NANOS_PER_SEC;
        let lo = order.iter().map(|id| seen[id].0).min().unwrap_or_default();
        let hi = order.iter().map(|id| seen[id].1).max().unwrap_or_default();
        let window = TimeRange::new(Some(lo.saturating_sub_nanos(hour)), Some(hi.saturating_add_nanos(hour + 1)));
        let id_filter = traces::trace_id_filter(&order);
        let mut sink = SpanCollector::new(10_000);
        exec::scan(&snapshot, Signal::Traces, id_filter.as_ref(), window, &mut sink, &mut ctx)?;

        let mut out: Vec<TraceSummary> =
            order.iter().filter_map(|id| sink.traces.get(id).map(|spans| traces::summarize(id, spans))).collect();
        out.sort_by_key(|t| std::cmp::Reverse(t.start));
        Ok((out, self.finish(Signal::Traces, ctx, filter.as_ref())))
    }

    /// All spans of one trace, ordered by start time.
    pub fn trace(&self, trace_id: &str, range: TimeRange) -> Result<(Vec<TelemetryEvent>, Diagnostics), QueryError> {
        let id = normalize_id(trace_id.trim());
        if id.is_empty() {
            return Err(QueryError::InvalidRequest("trace id is required".into()));
        }
        let filter = traces::trace_id_filter(std::slice::from_ref(&id));
        let snapshot = self.storage.snapshot(Signal::Traces);
        let mut ctx = self.ctx();
        let mut sink = SpanCollector::new(50_000);
        exec::scan(&snapshot, Signal::Traces, filter.as_ref(), range, &mut sink, &mut ctx)?;
        let mut spans = sink.traces.remove(&id).unwrap_or_default();
        spans.sort_by_key(|s| (s.timestamp, s.id));
        Ok((spans, self.finish(Signal::Traces, ctx, None)))
    }
}
