use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use event::Event;
use serde::Serialize;
use storage::EventStore;
use tantivy::collector::TopDocs;
use tantivy::query::Query;

use crate::ast::{Aggregation, TimeInterval};
use crate::planner::PlannerError;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SeriesPoint {
    pub key: String,
    pub value: f64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum AggregationResult {
    Number { value: f64 },
    Groups { points: Vec<SeriesPoint> },
    TimeSeries { points: Vec<SeriesPoint>, interval: String },
    Table { rows: Vec<BTreeMap<String, serde_json::Value>> },
}

pub fn execute_aggregation(
    store: &EventStore,
    query: Box<dyn Query>,
    from: Option<DateTime<Utc>>,
    to: Option<DateTime<Utc>>,
    aggregation: &Aggregation,
) -> Result<AggregationResult, PlannerError> {
    // Fetch matching events (bounded) and aggregate in-memory for MVP simplicity.
    let mut clauses: Vec<(tantivy::query::Occur, Box<dyn Query>)> =
        vec![(tantivy::query::Occur::Must, query)];

    if let Some(from) = from {
        clauses.push((
            tantivy::query::Occur::Must,
            storage::timestamp_range(
                std::ops::Bound::Included(from.timestamp_nanos_opt().unwrap_or(0)),
                std::ops::Bound::Unbounded,
            ),
        ));
    }
    if let Some(to) = to {
        clauses.push((
            tantivy::query::Occur::Must,
            storage::timestamp_range(
                std::ops::Bound::Unbounded,
                std::ops::Bound::Included(to.timestamp_nanos_opt().unwrap_or(0)),
            ),
        ));
    }

    let final_query: Box<dyn Query> = if clauses.len() == 1 {
        clauses.remove(0).1
    } else {
        Box::new(tantivy::query::BooleanQuery::new(clauses))
    };

    let searcher = store.searcher();
    let top = TopDocs::with_limit(50_000);
    let docs = searcher
        .search(&*final_query, &top)
        .map_err(PlannerError::Tantivy)?;

    let mut events = Vec::with_capacity(docs.len());
    for (_score, addr) in docs {
        let doc = searcher.doc(addr).map_err(PlannerError::Tantivy)?;
        if let Some(event) = store.writer_helper().from_document(&doc) {
            events.push(event);
        }
    }

    Ok(aggregate_events(&events, aggregation))
}

fn aggregate_events(events: &[Event], aggregation: &Aggregation) -> AggregationResult {
    match aggregation {
        Aggregation::Count => AggregationResult::Number {
            value: events.len() as f64,
        },
        Aggregation::CountBy { field } => {
            let mut counts: BTreeMap<String, u64> = BTreeMap::new();
            for event in events {
                let key = field_value(event, field).unwrap_or_else(|| "(null)".into());
                *counts.entry(key).or_default() += 1;
            }
            let points = counts
                .into_iter()
                .map(|(key, value)| SeriesPoint {
                    key,
                    value: value as f64,
                })
                .collect();
            AggregationResult::Groups { points }
        }
        Aggregation::CountByTime { interval } => {
            let secs = interval.as_secs();
            let mut counts: BTreeMap<i64, u64> = BTreeMap::new();
            for event in events {
                let ts = event.timestamp.timestamp();
                let bucket = (ts / secs) * secs;
                *counts.entry(bucket).or_default() += 1;
            }
            let points = counts
                .into_iter()
                .map(|(bucket, value)| {
                    let dt = DateTime::from_timestamp(bucket, 0)
                        .unwrap_or_else(|| DateTime::<Utc>::UNIX_EPOCH);
                    SeriesPoint {
                        key: dt.to_rfc3339(),
                        value: value as f64,
                    }
                })
                .collect();
            AggregationResult::TimeSeries {
                points,
                interval: interval_str(*interval).into(),
            }
        }
        Aggregation::Avg { field } => {
            let nums = numeric_values(events, field);
            let value = if nums.is_empty() {
                0.0
            } else {
                nums.iter().sum::<f64>() / nums.len() as f64
            };
            AggregationResult::Number { value }
        }
        Aggregation::Sum { field } => {
            let value = numeric_values(events, field).into_iter().sum();
            AggregationResult::Number { value }
        }
        Aggregation::Min { field } => {
            let value = numeric_values(events, field)
                .into_iter()
                .fold(f64::INFINITY, f64::min);
            AggregationResult::Number {
                value: if value.is_finite() { value } else { 0.0 },
            }
        }
        Aggregation::Max { field } => {
            let value = numeric_values(events, field)
                .into_iter()
                .fold(f64::NEG_INFINITY, f64::max);
            AggregationResult::Number {
                value: if value.is_finite() { value } else { 0.0 },
            }
        }
    }
}

fn interval_str(interval: TimeInterval) -> &'static str {
    interval.as_str()
}

fn field_value(event: &Event, field: &str) -> Option<String> {
    match field {
        "level" => event.level.map(|l| l.as_str().to_string()),
        "service" => event.service.clone(),
        "environment" => event.environment.clone(),
        "event_type" => Some(event.event_type.as_str().to_string()),
        "trace_id" => event.trace_id.clone(),
        "span_id" => event.span_id.clone(),
        "message" => event.message.clone(),
        other => {
            // attributes path
            let path = other.strip_prefix("attributes.").unwrap_or(other);
            get_path(&event.attributes, path).map(|v| value_to_string(&v))
        }
    }
}

fn numeric_values(events: &[Event], field: &str) -> Vec<f64> {
    events
        .iter()
        .filter_map(|e| match field {
            "duration_ns" => e.duration_ns.map(|n| n as f64),
            other => {
                let path = other.strip_prefix("attributes.").unwrap_or(other);
                get_path(&e.attributes, path).and_then(|v| match v {
                    serde_json::Value::Number(n) => n.as_f64(),
                    serde_json::Value::String(s) => s.parse().ok(),
                    _ => None,
                })
            }
        })
        .collect()
}

fn get_path(map: &serde_json::Map<String, serde_json::Value>, path: &str) -> Option<serde_json::Value> {
    let mut current = serde_json::Value::Object(map.clone());
    for part in path.split('.') {
        match current {
            serde_json::Value::Object(ref m) => {
                current = m.get(part)?.clone();
            }
            _ => return None,
        }
    }
    Some(current)
}

fn value_to_string(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Null => "null".into(),
        other => other.to_string(),
    }
}
