//! Per-request decoding limits (REL-16).
//!
//! An admitted ingest request is budgeted at a fixed multiple of
//! `ingest.max_request_size` (REL-14). Most bodies decode well within it,
//! but some shapes cost far more memory than their size on the wire: many
//! tiny events (every `TelemetryEvent` has a fixed cost of a few hundred
//! bytes, however little it carries), many small nested values, and OTLP
//! protobuf records of a few bytes each. Counting requests cannot see
//! that, so the decoders keep a [`Budget`]:
//!
//! * **events**: more than [`Limits::max_events`] records (log records,
//!   spans, metric data points or native events) fail the request as soon
//!   as the count is known, before the batch is built;
//! * **decoded bytes**: every allocation the decoders make, the parsed
//!   JSON tree, OTLP protobuf structures and the events themselves, is
//!   charged against [`Limits::max_decoded_bytes`] before it is made (by
//!   estimate: a pre-scan of the protobuf wire format, a count of the
//!   events in a native body) or, for one value at a time, as it is made.
//!   Memory freed along the way is credited back, so the charge follows
//!   the decoder's working set rather than adding up everything it ever
//!   touched;
//! * **depth**: values nested more than [`Limits::max_depth`] levels deep
//!   are refused.
//!
//! A request over any limit decodes nothing further and stores nothing.

use crate::IngestError;

/// Deepest nesting of objects and arrays within one value: a native event's
/// properties, or an OTLP attribute or body (`AnyValue`).
pub const MAX_DEPTH: usize = 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Most records one request may carry (`ingest.queue_capacity`).
    pub max_events: usize,
    /// Estimated heap the decoders may hold for one request, on top of the
    /// body itself.
    pub max_decoded_bytes: usize,
    /// See [`MAX_DEPTH`].
    pub max_depth: usize,
}

impl Limits {
    /// No event or memory limit; nesting is still limited to
    /// [`MAX_DEPTH`]. For tools and tests that decode trusted input.
    pub const UNBOUNDED: Limits =
        Limits { max_events: usize::MAX, max_decoded_bytes: usize::MAX, max_depth: MAX_DEPTH };
}

/// What one request has used of its [`Limits`] so far.
#[derive(Debug)]
pub(crate) struct Budget {
    limits: Limits,
    events: usize,
    used: usize,
    peak: usize,
    /// A limit hit inside a serde visitor, whose own error type can only
    /// carry a message. Checked first when deserialization fails.
    failure: Option<IngestError>,
}

impl Budget {
    pub fn new(limits: Limits) -> Self {
        Self { limits, events: 0, used: 0, peak: 0, failure: None }
    }

    pub fn limits(&self) -> Limits {
        self.limits
    }

    /// Fail unless `n` records fit, without counting them.
    pub fn check_events(&self, n: usize) -> Result<(), IngestError> {
        if n > self.limits.max_events {
            return Err(IngestError::TooManyEvents { limit: self.limits.max_events });
        }
        Ok(())
    }

    /// Records counted so far.
    pub fn events(&self) -> usize {
        self.events
    }

    /// Count one more record.
    pub fn event(&mut self) -> Result<(), IngestError> {
        self.events += 1;
        self.check_events(self.events)
    }

    /// Charge `bytes` of heap, failing if the total passes the limit.
    pub fn charge(&mut self, bytes: usize) -> Result<(), IngestError> {
        self.used = self.used.saturating_add(bytes);
        self.peak = self.peak.max(self.used);
        if self.used > self.limits.max_decoded_bytes {
            return Err(IngestError::TooLarge { limit_bytes: self.limits.max_decoded_bytes });
        }
        Ok(())
    }

    /// Start over after everything decoded so far has been dropped: no
    /// bytes charged and no records counted. The peak is kept, as it is the
    /// request's.
    pub fn reset(&mut self) {
        self.used = 0;
        self.events = 0;
        self.failure = None;
    }

    /// Credit back `bytes` that have been freed.
    pub fn release(&mut self, bytes: usize) {
        self.used = self.used.saturating_sub(bytes);
    }

    pub fn check_depth(&self, depth: usize) -> Result<(), IngestError> {
        if depth > self.limits.max_depth {
            return Err(IngestError::TooDeep { limit: self.limits.max_depth });
        }
        Ok(())
    }

    /// Remember why a serde visitor stopped; see [`Budget::failure`].
    pub fn fail<E: serde::de::Error>(&mut self, e: IngestError) -> E {
        let msg = e.to_string();
        self.failure = Some(e);
        E::custom(msg)
    }

    /// The limit that stopped deserialization, if one did.
    pub fn take_failure(&mut self) -> Option<IngestError> {
        self.failure.take()
    }

    /// Bytes charged and not yet credited back.
    pub fn used(&self) -> usize {
        self.used
    }

    /// Highest charge so far.
    #[cfg(test)]
    pub fn peak(&self) -> usize {
        self.peak
    }
}

/// The capacity a `Vec` reaches when it grows past `cap` by pushing one
/// element at a time (doubling, at least 4; `RawVec`'s policy for elements
/// of up to 1 KiB). Decoders grow their vectors this way explicitly, so the
/// charge for a vector is its real allocation.
pub(crate) fn grown(cap: usize) -> usize {
    cap.saturating_mul(2).max(4)
}

/// Capacity of a vector of `n` elements built by pushing one at a time.
#[cfg(test)]
pub(crate) fn pushed_capacity(n: usize) -> usize {
    if n == 0 { 0 } else { n.next_power_of_two().max(4) }
}

/// Heap of a resource shared through an `Arc`: the allocation holding the
/// counts and the `Fields`, and what the fields own.
pub(crate) fn shared_fields(f: &telemetry::Fields) -> usize {
    2 * size_of::<usize>() + size_of::<telemetry::Fields>() + f.heap_size()
}

/// Push `item`, first charging any growth of `v`.
pub(crate) fn push<T>(v: &mut Vec<T>, item: T, budget: &mut Budget) -> Result<(), IngestError> {
    if v.len() == v.capacity() {
        let cap = grown(v.capacity());
        budget.charge((cap - v.capacity()) * size_of::<T>())?;
        v.reserve_exact(cap - v.len());
    }
    v.push(item);
    Ok(())
}

/// Reserve room for exactly `n` more elements, charging it first.
pub(crate) fn reserve<T>(v: &mut Vec<T>, n: usize, budget: &mut Budget) -> Result<(), IngestError> {
    let extra = (v.len() + n).saturating_sub(v.capacity());
    budget.charge(extra.saturating_mul(size_of::<T>()))?;
    v.reserve_exact(n);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn limits(max_events: usize, max_decoded_bytes: usize) -> Limits {
        Limits { max_events, max_decoded_bytes, max_depth: MAX_DEPTH }
    }

    #[test]
    fn events_are_limited_inclusively() {
        let mut b = Budget::new(limits(2, usize::MAX));
        assert!(b.check_events(2).is_ok());
        assert_eq!(b.check_events(3), Err(IngestError::TooManyEvents { limit: 2 }));
        b.event().unwrap();
        b.event().unwrap();
        assert_eq!(b.event(), Err(IngestError::TooManyEvents { limit: 2 }));
    }

    #[test]
    fn bytes_are_charged_and_released() {
        let mut b = Budget::new(limits(usize::MAX, 100));
        b.charge(60).unwrap();
        b.release(50);
        b.charge(90).unwrap();
        assert_eq!(b.peak(), 100);
        assert_eq!(b.charge(1), Err(IngestError::TooLarge { limit_bytes: 100 }));
    }

    #[test]
    fn vector_growth_is_charged_before_it_happens() {
        let mut b = Budget::new(limits(usize::MAX, 8 * 8));
        let mut v: Vec<u64> = Vec::new();
        for i in 0..8 {
            push(&mut v, i, &mut b).unwrap();
            assert_eq!(v.capacity(), pushed_capacity(v.len()));
        }
        // The ninth element needs 16 slots: refused, nothing allocated.
        assert!(push(&mut v, 8, &mut b).is_err());
        assert_eq!(v.capacity(), 8);
        let mut w: Vec<u64> = Vec::new();
        let mut b = Budget::new(limits(usize::MAX, 24));
        reserve(&mut w, 3, &mut b).unwrap();
        assert_eq!(w.capacity(), 3);
        assert!(reserve(&mut w, 4, &mut b).is_err());
    }

    #[test]
    fn depth_is_limited_inclusively() {
        let b = Budget::new(Limits::UNBOUNDED);
        assert!(b.check_depth(MAX_DEPTH).is_ok());
        assert_eq!(b.check_depth(MAX_DEPTH + 1), Err(IngestError::TooDeep { limit: MAX_DEPTH }));
    }
}
