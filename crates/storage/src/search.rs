use std::collections::HashSet;
use std::ops::Bound;

use chrono::{DateTime, Utc};
use event::Event;
use serde::{Deserialize, Serialize};
use tantivy::collector::{Count, DocSetCollector, TopDocs};
use tantivy::query::{AllQuery, BooleanQuery, Occur, Query, RangeQuery};
use tantivy::schema::{Field, Type};
use tantivy::{DateTime as TantivyDateTime, DocAddress, Order, Searcher, Term};
use thiserror::Error;

use crate::schema::EventSchema;
use crate::writer::EventWriter;

#[derive(Debug, Error)]
pub enum SearchError {
    #[error("tantivy error: {0}")]
    Tantivy(#[from] tantivy::TantivyError),
    #[error("invalid cursor")]
    InvalidCursor,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchCursor {
    pub timestamp_nanos: i64,
    pub id: String,
}

impl SearchCursor {
    pub fn encode(&self) -> String {
        use base64::Engine;
        let json = serde_json::to_vec(self).unwrap_or_default();
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(json)
    }

    pub fn decode(s: &str) -> Result<Self, SearchError> {
        use base64::Engine;
        let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(s)
            .map_err(|_| SearchError::InvalidCursor)?;
        serde_json::from_slice(&bytes).map_err(|_| SearchError::InvalidCursor)
    }
}

pub struct EventSearchParams {
    pub query: Box<dyn Query>,
    pub from: Option<DateTime<Utc>>,
    pub to: Option<DateTime<Utc>>,
    pub limit: usize,
    pub cursor: Option<SearchCursor>,
}

#[derive(Debug, Clone)]
pub struct EventSearchResult {
    pub events: Vec<Event>,
    pub next_cursor: Option<String>,
    pub total_estimate: usize,
}

pub fn search_events(
    searcher: &Searcher,
    _schema: &EventSchema,
    writer_helper: &EventWriter,
    params: EventSearchParams,
) -> Result<EventSearchResult, SearchError> {
    let mut clauses: Vec<(Occur, Box<dyn Query>)> = vec![(Occur::Must, params.query)];
    if let Some(from) = params.from {
        clauses.push((Occur::Must, timestamp_range(Bound::Included(nanos(from)), Bound::Unbounded)));
    }
    if let Some(to) = params.to {
        clauses.push((Occur::Must, timestamp_range(Bound::Unbounded, Bound::Included(nanos(to)))));
    }
    let base = and_all(clauses);
    let total_estimate = searcher.search(&*base, &Count).unwrap_or(0);

    // Results are ordered by (timestamp desc, id desc). Tantivy can only sort on the timestamp,
    // so every group of events that share a timestamp at a page edge is fetched in full and
    // ordered by id here; otherwise ties would be split arbitrarily across pages.
    let fetch = params.limit.saturating_add(1);
    let cursor = params.cursor.as_ref().map(|c| (c.timestamp_nanos, c.id.as_str()));
    let mut addresses: HashSet<DocAddress> = HashSet::new();

    let older: Box<dyn Query> = match cursor {
        Some((ts, _)) => and_all(vec![
            (Occur::Must, base.box_clone()),
            (Occur::Must, timestamp_range(Bound::Unbounded, Bound::Excluded(ts))),
        ]),
        None => base.box_clone(),
    };
    let top_docs = TopDocs::with_limit(fetch)
        .order_by_fast_field::<TantivyDateTime>("timestamp", Order::Desc);
    let top = searcher.search(&*older, &top_docs)?;
    if top.len() == fetch {
        if let Some((boundary, _)) = top.last() {
            let ts = boundary.into_timestamp_nanos();
            addresses.extend(docs_at(searcher, &*base, ts)?);
        }
    }
    addresses.extend(top.into_iter().map(|(_, addr)| addr));
    // Events sharing the cursor's timestamp that sort after it.
    if let Some((ts, _)) = cursor {
        addresses.extend(docs_at(searcher, &*base, ts)?);
    }

    let mut events = Vec::with_capacity(addresses.len());
    for addr in addresses {
        let doc = searcher.doc(addr)?;
        if let Some(event) = writer_helper.from_document(&doc) {
            events.push(event);
        }
    }
    let key = |e: &Event| (e.timestamp.timestamp_nanos_opt().unwrap_or(0), e.id.to_string());
    events.sort_by_cached_key(|e| std::cmp::Reverse(key(e)));
    if let Some((cursor_ts, cursor_id)) = cursor {
        events.retain(|e| {
            let (ts, id) = key(e);
            (ts, id.as_str()) < (cursor_ts, cursor_id)
        });
    }
    events.truncate(fetch);

    let next_cursor = if events.len() > params.limit {
        events.pop();
        events.last().map(|e| {
            let (timestamp_nanos, id) = key(e);
            SearchCursor { timestamp_nanos, id }.encode()
        })
    } else {
        None
    };

    Ok(EventSearchResult {
        events,
        next_cursor,
        total_estimate,
    })
}

fn nanos(dt: DateTime<Utc>) -> i64 {
    dt.timestamp_nanos_opt().unwrap_or(0)
}

/// A range filter on `timestamp`, with bounds in nanoseconds since the epoch.
///
/// `RangeQuery::new_date_bounds` truncates its bounds to whole seconds, so the bounds are built
/// as raw terms instead. Dates and i64s share the same u64 encoding, and because `timestamp` is
/// a fast field Tantivy evaluates the range against the full-precision fast-field values.
pub fn timestamp_range(lower: Bound<i64>, upper: Bound<i64>) -> Box<dyn Query> {
    let term = |b: Bound<i64>| match b {
        Bound::Included(n) => Bound::Included(Term::from_field_i64(Field::from_field_id(0), n)),
        Bound::Excluded(n) => Bound::Excluded(Term::from_field_i64(Field::from_field_id(0), n)),
        Bound::Unbounded => Bound::Unbounded,
    };
    Box::new(RangeQuery::new_term_bounds(
        "timestamp".to_string(),
        Type::Date,
        &term(lower),
        &term(upper),
    ))
}

fn and_all(mut clauses: Vec<(Occur, Box<dyn Query>)>) -> Box<dyn Query> {
    if clauses.len() == 1 {
        clauses.remove(0).1
    } else {
        Box::new(BooleanQuery::new(clauses))
    }
}

/// Every document matching `base` whose timestamp is exactly `ts`.
fn docs_at(searcher: &Searcher, base: &dyn Query, ts: i64) -> Result<HashSet<DocAddress>, SearchError> {
    let query = and_all(vec![
        (Occur::Must, base.box_clone()),
        (Occur::Must, timestamp_range(Bound::Included(ts), Bound::Included(ts))),
    ]);
    Ok(searcher.search(&*query, &DocSetCollector)?)
}

pub fn count_all(searcher: &Searcher) -> Result<usize, SearchError> {
    Ok(searcher.search(&AllQuery, &Count)?)
}
