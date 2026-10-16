//! High-level query API used by the server.
//!
//! Every request takes a [`CancelToken`] and runs within one [`Budget`]
//! built from the engine's [`QueryLimits`]; see [`crate::budget`].

use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, RwLock};

use serde::Serialize;
use storage::Storage;
use telemetry::*;

use crate::aggregate::{self, Agg, CountSink, Facets, Histogram, HistogramSink, MetricMeta, MetricSink, Series};
use crate::ast::Expr;
use crate::budget::{Budget, CancelToken, Checkpoint, CheckpointHook};
use crate::error::QueryError;
use crate::eval::{CExpr, compile};
use crate::exec::{self, Cursor, Diagnostics, Direction, ExecCtx, QueryLimits, SearchRequest, TimeRange};
use crate::parser::parse;
use crate::stats::QueryStats;
use crate::traces::{self, SpanCollector, TraceSummary};

/// Most points one metric series may have.
pub const MAX_METRIC_POINTS: usize = 10_000;

/// Spans kept per trace while summarising trace search results.
const MAX_SUMMARY_SPANS: usize = 10_000;

pub struct Engine {
    storage: Arc<Storage>,
    limits: QueryLimits,
    stats: QueryStats,
    hook: RwLock<Option<CheckpointHook>>,
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

/// `GET /api/v1/metrics`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MetricNames {
    pub metrics: Vec<MetricInfo>,
    /// More names exist than `query.max_metric_names`.
    pub truncated: bool,
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
        Self { storage, limits, stats: QueryStats::default(), hook: RwLock::new(None) }
    }

    /// Test seam: run `hook` at every checkpoint of queries started from
    /// now on (e.g. to hold a query until it is cancelled). `None` removes it.
    pub fn set_checkpoint_hook(&self, hook: Option<CheckpointHook>) {
        *self.hook.write().unwrap_or_else(|e| e.into_inner()) = hook;
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

    /// The budget of one request.
    pub fn budget(&self, cancel: &CancelToken) -> Budget {
        let hook = self.hook.read().unwrap_or_else(|e| e.into_inner()).clone();
        Budget::with_hook(&self.limits, cancel.clone(), hook)
    }

    fn ctx<'a>(&'a self, budget: &'a Budget) -> ExecCtx<'a> {
        ExecCtx::new(self.storage.cache(), budget, self.limits.max_results)
    }

    fn finish(&self, signal: Signal, mut ctx: ExecCtx, filter: Option<&CExpr>) -> Diagnostics {
        if let Some(f) = filter {
            crate::prune::note_fields(f, &mut ctx.usage);
        }
        let d = ctx.finish();
        self.stats.record(signal.as_str(), &d);
        d
    }

    // One argument per field of the search request, plus its cancellation.
    #[allow(clippy::too_many_arguments)]
    pub fn search(
        &self,
        signal: Signal,
        query: &str,
        range: TimeRange,
        limit: usize,
        direction: Direction,
        cursor: Option<Cursor>,
        cancel: &CancelToken,
    ) -> Result<SearchOutput, QueryError> {
        let filter = compile_query(query, signal)?;
        let snapshot = self.storage.snapshot(signal);
        let budget = self.budget(cancel);
        let mut ctx = self.ctx(&budget);
        let r = exec::search(
            &snapshot,
            &SearchRequest { signal, filter: filter.as_ref(), range, limit, direction, cursor },
            &mut ctx,
        )?;
        Ok(SearchOutput { events: r.events, next: r.next, diagnostics: self.finish(signal, ctx, filter.as_ref()) })
    }

    pub fn count(
        &self,
        signal: Signal,
        query: &str,
        range: TimeRange,
        cancel: &CancelToken,
    ) -> Result<(u64, Diagnostics), QueryError> {
        let filter = compile_query(query, signal)?;
        let snapshot = self.storage.snapshot(signal);
        let budget = self.budget(cancel);
        let mut ctx = self.ctx(&budget);
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
        cancel: &CancelToken,
    ) -> Result<(Histogram, Diagnostics), QueryError> {
        let filter = compile_query(query, signal)?;
        let step = aggregate::choose_step_ms(&range, max_buckets);
        let snapshot = self.storage.snapshot(signal);
        let budget = self.budget(cancel);
        let mut ctx = self.ctx(&budget);
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
        cancel: &CancelToken,
    ) -> Result<(Facets, Diagnostics), QueryError> {
        let filter = compile_query(query, signal)?;
        let snapshot = self.storage.snapshot(signal);
        let budget = self.budget(cancel);
        let mut ctx = self.ctx(&budget);
        let mut inner = ExecCtx::new(self.storage.cache(), &budget, sample);
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
        ctx.check(Checkpoint::Assemble)?;
        let facets = aggregate::facets(&r.events, 10, &budget)?;
        Ok((facets, self.finish(signal, ctx, filter.as_ref())))
    }

    /// Distinct metric names in name order, at most
    /// `QueryLimits::max_metric_names` of them (the first ones by name;
    /// `truncated` says more exist). Memory stays bounded by that limit
    /// however many names are stored, and the request's budget is checked
    /// per segment and every [`crate::budget::ROWS_PER_CHECK`] names or
    /// unsealed events.
    pub fn metric_names(&self, cancel: &CancelToken) -> Result<MetricNames, QueryError> {
        let budget = self.budget(cancel);
        budget.check(Checkpoint::Start)?;
        let max = self.limits.max_metric_names.max(1);
        let snap = self.storage.snapshot(Signal::Metrics);
        let mut names: BTreeSet<String> = BTreeSet::new();
        let mut truncated = false;
        let mut add = |name: &str| -> Result<(), QueryError> {
            budget.tick(Checkpoint::Aggregate)?;
            // Keep the `max` smallest names: past the largest kept one,
            // nothing new can get in.
            if names.len() >= max && names.last().is_some_and(|l| name >= l.as_str()) {
                truncated |= !names.contains(name);
                return Ok(());
            }
            if names.insert(name.to_string()) && names.len() > max {
                names.pop_last();
                truncated = true;
            }
            Ok(())
        };
        for s in &snap.segments {
            budget.check(Checkpoint::Segment)?;
            for name in s.summary.names.iter().flatten() {
                add(name)?;
            }
        }
        for b in &snap.batches {
            for e in &b.events {
                add(e.message())?;
            }
        }
        budget.check(Checkpoint::Assemble)?;
        Ok(MetricNames { metrics: names.into_iter().map(|name| MetricInfo { name }).collect(), truncated })
    }

    pub fn metric_query(&self, q: &MetricQuery, cancel: &CancelToken) -> Result<MetricOutput, QueryError> {
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
        // Refuse rather than return series that silently stop early.
        if aggregate::bucket_count(&q.range, step) > MAX_METRIC_POINTS as u64 {
            return Err(QueryError::ResultLimit {
                what: "points per series",
                limit: MAX_METRIC_POINTS as u64,
                hint: "use a larger stepMs or a shorter time range",
            });
        }
        let snapshot = self.storage.snapshot(Signal::Metrics);
        let budget = self.budget(cancel);
        let mut ctx = self.ctx(&budget);
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
        cancel: &CancelToken,
    ) -> Result<(Vec<TraceSummary>, Diagnostics), QueryError> {
        let filter = compile_query(query, Signal::Traces)?;
        let snapshot = self.storage.snapshot(Signal::Traces);
        let budget = self.budget(cancel);
        let mut ctx = self.ctx(&budget);
        let limit = limit.clamp(1, 200);

        // 1. Newest matching spans → distinct trace ids.
        let mut order: Vec<String> = Vec::new();
        let mut seen: HashMap<String, (Timestamp, Timestamp)> = HashMap::new();
        let mut cursor = None;
        for _ in 0..20 {
            // Pages share the request's budget: their time and bytes add up.
            let mut page_ctx = ExecCtx::new(self.storage.cache(), &budget, 500);
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
        let mut sink = SpanCollector::new(MAX_SUMMARY_SPANS);
        ctx.check(Checkpoint::Assemble)?;
        exec::scan(&snapshot, Signal::Traces, id_filter.as_ref(), window, &mut sink, &mut ctx)?;

        let mut out: Vec<TraceSummary> = order
            .iter()
            .filter_map(|id| {
                sink.traces.get(id).map(|spans| TraceSummary {
                    truncated: sink.truncated_traces.contains(id),
                    ..traces::summarize(id, spans)
                })
            })
            .collect();
        out.sort_by_key(|t| std::cmp::Reverse(t.start));
        Ok((out, self.finish(Signal::Traces, ctx, filter.as_ref())))
    }

    /// All spans of one trace, ordered by start time.
    pub fn trace(
        &self,
        trace_id: &str,
        range: TimeRange,
        cancel: &CancelToken,
    ) -> Result<(Vec<TelemetryEvent>, Diagnostics), QueryError> {
        let id = normalize_id(trace_id.trim());
        if id.is_empty() {
            return Err(QueryError::InvalidRequest("trace id is required".into()));
        }
        let filter = traces::trace_id_filter(std::slice::from_ref(&id));
        let snapshot = self.storage.snapshot(Signal::Traces);
        let budget = self.budget(cancel);
        let mut ctx = self.ctx(&budget);
        let mut sink = SpanCollector::new(self.limits.max_trace_spans);
        exec::scan(&snapshot, Signal::Traces, filter.as_ref(), range, &mut sink, &mut ctx)?;
        // A partial trace would look complete in the waterfall.
        if sink.truncated {
            return Err(QueryError::ResultLimit {
                what: "spans in one trace",
                limit: self.limits.max_trace_spans as u64,
                hint: "narrow the time range with from/to, or raise query.max_trace_spans",
            });
        }
        let mut spans = sink.traces.remove(&id).unwrap_or_default();
        spans.sort_by_key(|s| (s.timestamp, s.id));
        Ok((spans, self.finish(Signal::Traces, ctx, None)))
    }
}
