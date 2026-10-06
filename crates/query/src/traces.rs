//! Trace search and assembly.

use std::collections::{BTreeMap, HashMap};
use std::ops::ControlFlow;

use serde::Serialize;
use storage::segment::ColumnSet;
use telemetry::*;

use crate::ast::{Expr, Literal, Op};
use crate::eval::{CExpr, Row, compile};
use crate::exec::Sink;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TraceSummary {
    pub trace_id: String,
    pub root_name: Option<String>,
    pub root_service: Option<String>,
    pub start: Timestamp,
    pub duration_ms: f64,
    pub span_count: usize,
    pub services: Vec<String>,
    pub error_count: usize,
}

/// `traceId = a or traceId = b or ...` as a balanced tree.
pub fn trace_id_filter(ids: &[String]) -> Option<CExpr> {
    fn build(ids: &[String]) -> Expr {
        if ids.len() == 1 {
            return Expr::cmp("traceId", Op::Eq, Literal::String(ids[0].clone()));
        }
        let mid = ids.len() / 2;
        Expr::or(build(&ids[..mid]), build(&ids[mid..]))
    }
    if ids.is_empty() {
        return None;
    }
    Some(compile(&build(ids), Signal::Traces))
}

/// Collects full spans grouped by trace id.
pub struct SpanCollector {
    pub traces: HashMap<String, Vec<TelemetryEvent>>,
    pub max_spans_per_trace: usize,
    pub truncated: bool,
}

impl SpanCollector {
    pub fn new(max_spans_per_trace: usize) -> Self {
        Self { traces: HashMap::new(), max_spans_per_trace, truncated: false }
    }
}

impl Sink for SpanCollector {
    fn columns(&self) -> ColumnSet {
        ColumnSet::ALL
    }

    fn row(&mut self, row: &dyn Row) -> ControlFlow<()> {
        if let Some(e) = row.event()
            && let Some(t) = &e.trace_id
        {
            let v = self.traces.entry(t.as_str().to_string()).or_default();
            if v.len() < self.max_spans_per_trace {
                v.push(e.clone());
            } else {
                self.truncated = true;
            }
        }
        ControlFlow::Continue(())
    }
}

pub fn summarize(trace_id: &str, spans: &[TelemetryEvent]) -> TraceSummary {
    let ids: std::collections::HashSet<&str> =
        spans.iter().filter_map(|s| s.span_id.as_ref().map(SpanId::as_str)).collect();
    // Root: no parent, or a parent we never received. Earliest wins.
    let root = spans
        .iter()
        .filter(|s| s.as_span().and_then(|sp| sp.parent_span_id.as_ref()).is_none_or(|p| !ids.contains(p.as_str())))
        .min_by_key(|s| s.timestamp);
    let start = spans.iter().map(|s| s.timestamp).min().unwrap_or_default();
    let end = spans
        .iter()
        .map(|s| {
            s.timestamp.saturating_add_nanos(s.as_span().map_or(0, |sp| sp.duration_nanos.min(i64::MAX as u64) as i64))
        })
        .max()
        .unwrap_or(start);
    let mut services: BTreeMap<String, ()> = BTreeMap::new();
    for s in spans {
        if let Some(svc) = &s.service {
            services.insert(svc.clone(), ());
        }
    }
    TraceSummary {
        trace_id: trace_id.to_string(),
        root_name: root.map(|r| r.message().to_string()),
        root_service: root.and_then(|r| r.service.clone()),
        start,
        duration_ms: (end.0 - start.0) as f64 / 1e6,
        span_count: spans.len(),
        services: services.into_keys().collect(),
        error_count: spans.iter().filter(|s| s.as_span().is_some_and(|sp| sp.status.code == StatusCode::Error)).count(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn span(id: &str, parent: Option<&str>, start: i64, dur: u64, svc: &str, err: bool) -> TelemetryEvent {
        TelemetryEvent {
            id: EventId(0),
            timestamp: Timestamp(start),
            observed_timestamp: Timestamp(start),
            service: Some(svc.into()),
            environment: None,
            trace_id: TraceId::new("t1"),
            span_id: SpanId::new(id),
            resource: Fields::new(),
            attributes: Fields::new(),
            payload: TelemetryPayload::Span(SpanEvent {
                parent_span_id: parent.and_then(SpanId::new),
                name: format!("op-{id}"),
                kind: SpanKind::Server,
                duration_nanos: dur,
                status: SpanStatus { code: if err { StatusCode::Error } else { StatusCode::Ok }, message: None },
                events: vec![],
                links: vec![],
            }),
        }
    }

    #[test]
    fn summary_finds_root_and_extent() {
        let spans = vec![
            span("b", Some("a"), 1_000, 500, "db", true),
            span("a", None, 900, 2_000, "api", false),
            span("c", Some("a"), 1_500, 2_000, "cache", false),
        ];
        let s = summarize("t1", &spans);
        assert_eq!(s.root_name.as_deref(), Some("op-a"));
        assert_eq!(s.root_service.as_deref(), Some("api"));
        assert_eq!(s.start, Timestamp(900));
        assert_eq!(s.duration_ms, (3_500 - 900) as f64 / 1e6);
        assert_eq!(s.span_count, 3);
        assert_eq!(s.services, vec!["api", "cache", "db"]);
        assert_eq!(s.error_count, 1);
    }

    #[test]
    fn orphan_spans_count_as_roots() {
        let spans = vec![span("x", Some("missing"), 5, 1, "svc", false)];
        assert_eq!(summarize("t1", &spans).root_name.as_deref(), Some("op-x"));
    }

    #[test]
    fn id_filter_builds_balanced_or() {
        assert!(trace_id_filter(&[]).is_none());
        let ids: Vec<String> = (0..5).map(|i| format!("t{i}")).collect();
        assert!(matches!(trace_id_filter(&ids), Some(CExpr::Or(..))));
    }
}
