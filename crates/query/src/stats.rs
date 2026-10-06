//! Query statistics: the raw material for adaptive indexing (v0.2+).
//!
//! For every field referenced by a query we accumulate how often it was
//! queried, how much data those queries read, and how many segments the
//! existing indexes let us skip. Nothing builds indexes from this yet; the
//! server persists it to the metadata store and exposes it via the API.

use std::collections::HashMap;
use std::sync::Mutex;

use serde::Serialize;

use crate::exec::Diagnostics;

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FieldQueryStats {
    pub signal: String,
    pub field: String,
    /// Index kind used for the field ("bloom", "bitmap", "zonemap", "none").
    pub index: String,
    pub queries: u64,
    pub bytes_read: u64,
    pub segments_scanned: u64,
    pub segments_skipped: u64,
    pub total_ms: f64,
}

#[derive(Default)]
pub struct QueryStats {
    inner: Mutex<HashMap<(String, String), FieldQueryStats>>,
}

impl QueryStats {
    pub fn record(&self, signal: &str, diag: &Diagnostics) {
        let fields: Vec<_> = diag.indexes.iter().filter(|i| i.field != "timestamp").collect();
        if fields.is_empty() {
            return;
        }
        let scanned = (diag.segments_considered - diag.segments_skipped) as u64;
        let mut m = self.inner.lock().unwrap();
        for f in fields {
            let e = m.entry((signal.to_string(), f.field.clone())).or_insert_with(|| FieldQueryStats {
                signal: signal.to_string(),
                field: f.field.clone(),
                ..Default::default()
            });
            e.index = f.kind.to_string();
            e.queries += 1;
            e.bytes_read += diag.bytes_read;
            e.segments_scanned += scanned;
            e.segments_skipped += diag.segments_skipped as u64;
            e.total_ms += diag.elapsed_ms;
        }
    }

    /// Take accumulated stats (for persisting). The caller adds them to
    /// the stored totals.
    pub fn drain(&self) -> Vec<FieldQueryStats> {
        self.inner.lock().unwrap().drain().map(|(_, v)| v).collect()
    }

    /// Current not-yet-persisted stats.
    pub fn pending(&self) -> Vec<FieldQueryStats> {
        self.inner.lock().unwrap().values().cloned().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::IndexUse;

    #[test]
    fn records_and_drains() {
        let s = QueryStats::default();
        let d = Diagnostics {
            segments_considered: 10,
            segments_skipped: 7,
            bytes_read: 100,
            elapsed_ms: 2.0,
            indexes: vec![
                IndexUse { field: "timestamp".into(), kind: "time" },
                IndexUse { field: "customerId".into(), kind: "bloom" },
            ],
            ..Default::default()
        };
        s.record("logs", &d);
        s.record("logs", &d);
        let v = s.drain();
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].queries, 2);
        assert_eq!(v[0].segments_skipped, 14);
        assert_eq!(v[0].segments_scanned, 6);
        assert!(s.drain().is_empty());
    }
}
