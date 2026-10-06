//! Byte-budgeted LRU cache for segment index regions.
//!
//! Index regions (Bloom filters, zone maps) are only needed by queries that
//! filter on high-cardinality or numeric fields, and keeping all of them
//! resident would make memory grow with retention. The cache holds the
//! recently used ones within a fixed budget and evicts the rest.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use telemetry::Signal;

use crate::error::Result;
use crate::segment::{Segment, SegmentIndex};

type Key = (Signal, u64);

struct Entry {
    value: Arc<SegmentIndex>,
    size: usize,
    last_used: u64,
}

struct Inner {
    map: HashMap<Key, Entry>,
    bytes: usize,
    tick: u64,
}

pub struct IndexCache {
    budget: usize,
    inner: Mutex<Inner>,
    hits: AtomicU64,
    misses: AtomicU64,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct CacheStats {
    pub budget_bytes: usize,
    pub used_bytes: usize,
    pub entries: usize,
    pub hits: u64,
    pub misses: u64,
}

impl IndexCache {
    pub fn new(budget: usize) -> Self {
        Self {
            budget,
            inner: Mutex::new(Inner { map: HashMap::new(), bytes: 0, tick: 0 }),
            hits: AtomicU64::new(0),
            misses: AtomicU64::new(0),
        }
    }

    /// Return the cached index of `segment`, loading it on a miss. Loading
    /// happens outside the lock so a slow disk read does not block other
    /// queries' cache hits.
    pub fn get(&self, segment: &Segment) -> Result<Arc<SegmentIndex>> {
        let key = (segment.summary.signal, segment.id());
        {
            let mut inner = self.inner.lock().unwrap();
            inner.tick += 1;
            let tick = inner.tick;
            if let Some(e) = inner.map.get_mut(&key) {
                e.last_used = tick;
                self.hits.fetch_add(1, Ordering::Relaxed);
                return Ok(e.value.clone());
            }
        }
        self.misses.fetch_add(1, Ordering::Relaxed);
        let value = Arc::new(segment.read_index()?);
        let size = value.approx_size();
        if size > self.budget {
            // Larger than the whole budget: use it once without caching.
            return Ok(value);
        }
        let mut inner = self.inner.lock().unwrap();
        inner.tick += 1;
        let tick = inner.tick;
        while inner.bytes + size > self.budget {
            let Some(victim) = inner.map.iter().min_by_key(|(_, e)| e.last_used).map(|(k, _)| *k) else {
                break;
            };
            if let Some(e) = inner.map.remove(&victim) {
                inner.bytes -= e.size;
            }
        }
        if let Some(old) = inner.map.insert(key, Entry { value: value.clone(), size, last_used: tick }) {
            inner.bytes -= old.size;
        }
        inner.bytes += size;
        Ok(value)
    }

    pub fn evict(&self, signal: Signal, segment_id: u64) {
        let mut inner = self.inner.lock().unwrap();
        if let Some(e) = inner.map.remove(&(signal, segment_id)) {
            inner.bytes -= e.size;
        }
    }

    pub fn stats(&self) -> CacheStats {
        let inner = self.inner.lock().unwrap();
        CacheStats {
            budget_bytes: self.budget,
            used_bytes: inner.bytes,
            entries: inner.map.len(),
            hits: self.hits.load(Ordering::Relaxed),
            misses: self.misses.load(Ordering::Relaxed),
        }
    }
}
