//! Versioned binary encoding of telemetry events.
//!
//! An event is split into a fixed *head* (fields that segments store as
//! separate columns so queries can skip the rest) and a *body*. The WAL
//! stores head + body back to back; segments store them in different
//! column chunks. Both use the functions in this module, so there is exactly
//! one definition of how an event becomes bytes.
//!
//! A WAL batch in codec 2 holds each distinct resource once, in a table at
//! its start, and its events refer to it by index; segment bodies (and
//! codec 1 batches, still read) carry the resource inline.
//!
//! The layout is documented in `docs/storage-format.md`. Any change must bump
//! [`CODEC_VERSION`] and keep a decoder for older versions.

use std::collections::HashMap;
use std::sync::Arc;

use telemetry::*;

use crate::encoding::{Reader, Writer};
use crate::error::DecodeError;

/// Version of the event codec, written at the start of every WAL record.
/// Version 1 (no resource table) is still read.
pub const CODEC_VERSION: u8 = 2;

/// Maximum nesting of structured values accepted when decoding.
pub const MAX_DEPTH: usize = 64;

const TAG_NULL: u8 = 0;
const TAG_FALSE: u8 = 1;
const TAG_TRUE: u8 = 2;
const TAG_INT: u8 = 3;
const TAG_FLOAT: u8 = 4;
const TAG_STRING: u8 = 5;
const TAG_ARRAY: u8 = 6;
const TAG_OBJECT: u8 = 7;

pub fn encode_value(w: &mut Writer, v: &Value) {
    match v {
        Value::Null => w.u8(TAG_NULL),
        Value::Bool(false) => w.u8(TAG_FALSE),
        Value::Bool(true) => w.u8(TAG_TRUE),
        Value::Int(i) => {
            w.u8(TAG_INT);
            w.varint_i(*i);
        }
        Value::Float(f) => {
            w.u8(TAG_FLOAT);
            w.f64(*f);
        }
        Value::String(s) => {
            w.u8(TAG_STRING);
            w.str(s);
        }
        Value::Array(a) => {
            w.u8(TAG_ARRAY);
            w.varint(a.len() as u64);
            for item in a {
                encode_value(w, item);
            }
        }
        Value::Object(f) => {
            w.u8(TAG_OBJECT);
            encode_fields(w, f);
        }
    }
}

pub fn decode_value(r: &mut Reader, depth: usize) -> Result<Value, DecodeError> {
    if depth > MAX_DEPTH {
        return Err(DecodeError::DepthExceeded);
    }
    Ok(match r.u8()? {
        TAG_NULL => Value::Null,
        TAG_FALSE => Value::Bool(false),
        TAG_TRUE => Value::Bool(true),
        TAG_INT => Value::Int(r.varint_i()?),
        TAG_FLOAT => Value::Float(r.f64()?),
        TAG_STRING => Value::String(r.string()?),
        TAG_ARRAY => {
            // Each element needs at least one byte, so len() bounds this.
            let n = r.len()?;
            let mut a = Vec::with_capacity(n);
            for _ in 0..n {
                a.push(decode_value(r, depth + 1)?);
            }
            Value::Array(a)
        }
        TAG_OBJECT => Value::Object(decode_fields_depth(r, depth + 1)?),
        t => return Err(DecodeError::InvalidTag(t)),
    })
}

pub fn encode_fields(w: &mut Writer, f: &Fields) {
    w.varint(f.len() as u64);
    for (k, v) in f.iter() {
        w.str(k);
        encode_value(w, v);
    }
}

pub fn decode_fields(r: &mut Reader) -> Result<Fields, DecodeError> {
    decode_fields_depth(r, 0)
}

fn decode_fields_depth(r: &mut Reader, depth: usize) -> Result<Fields, DecodeError> {
    if depth > MAX_DEPTH {
        return Err(DecodeError::DepthExceeded);
    }
    let n = r.len()?;
    let mut f = Fields::with_capacity(n);
    for _ in 0..n {
        let k = r.string()?;
        let v = decode_value(r, depth)?;
        f.push(k, v);
    }
    Ok(f)
}

/// Event kind code stored in the head (matches [`Signal::code`]).
pub fn kind_code(e: &TelemetryEvent) -> u8 {
    e.signal().code()
}

/// Level code (0 = none) stored in the head.
pub fn level_code(e: &TelemetryEvent) -> u8 {
    e.level().map_or(0, Level::code)
}

/// The body: everything not stored as a dedicated segment column, with the
/// resource inline.
pub fn encode_body(w: &mut Writer, e: &TelemetryEvent) {
    encode_body_as(w, e, None);
}

/// The body with the resource inline (`None`) or as an index into its
/// batch's resource table (0 for none, `i` for entry `i - 1`).
fn encode_body_as(w: &mut Writer, e: &TelemetryEvent, resource: Option<u64>) {
    // Observed time is usually within milliseconds of the event time, so a
    // zigzag delta is a couple of bytes instead of eight.
    w.varint_i(e.observed_timestamp.0.wrapping_sub(e.timestamp.0));
    match resource {
        None => encode_fields(w, &e.resource),
        Some(i) => w.varint(i),
    }
    encode_fields(w, &e.attributes);
    match &e.payload {
        TelemetryPayload::Log(l) => {
            w.opt_str(l.message_template.as_deref());
            match &l.exception {
                None => w.u8(0),
                Some(x) => {
                    w.u8(1);
                    w.opt_str(x.kind.as_deref());
                    w.opt_str(x.message.as_deref());
                    w.opt_str(x.stack_trace.as_deref());
                }
            }
        }
        TelemetryPayload::Span(s) => {
            w.opt_str(s.parent_span_id.as_ref().map(SpanId::as_str));
            w.u8(s.kind.code());
            w.varint(s.duration_nanos);
            w.u8(s.status.code.code());
            w.opt_str(s.status.message.as_deref());
            w.varint(s.events.len() as u64);
            for ev in &s.events {
                w.varint_i(ev.timestamp.0.wrapping_sub(e.timestamp.0));
                w.str(&ev.name);
                encode_fields(w, &ev.attributes);
            }
            w.varint(s.links.len() as u64);
            for l in &s.links {
                w.str(l.trace_id.as_str());
                w.str(l.span_id.as_str());
                encode_fields(w, &l.attributes);
            }
        }
        TelemetryPayload::Metric(m) => {
            w.opt_str(m.description.as_deref());
            w.opt_str(m.unit.as_deref());
            w.u8(m.kind.code());
            w.u8(m.temporality.code());
            w.u8(m.monotonic as u8);
            match &m.value {
                MetricValue::Number(v) => {
                    w.u8(0);
                    w.f64(*v);
                }
                MetricValue::Histogram(h) => {
                    w.u8(1);
                    w.varint(h.count);
                    let flags =
                        (h.sum.is_some() as u8) | ((h.min.is_some() as u8) << 1) | ((h.max.is_some() as u8) << 2);
                    w.u8(flags);
                    for v in [h.sum, h.min, h.max].into_iter().flatten() {
                        w.f64(v);
                    }
                    w.varint(h.bucket_counts.len() as u64);
                    for c in &h.bucket_counts {
                        w.varint(*c);
                    }
                    w.varint(h.explicit_bounds.len() as u64);
                    for b in &h.explicit_bounds {
                        w.f64(*b);
                    }
                }
            }
        }
    }
}

/// The columns a segment stores outside the body.
#[derive(Debug, Clone, PartialEq)]
pub struct Head {
    pub id: EventId,
    pub timestamp: Timestamp,
    pub kind: u8,
    pub level: u8,
    pub service: Option<String>,
    pub environment: Option<String>,
    pub trace_id: Option<TraceId>,
    pub span_id: Option<SpanId>,
    pub message: String,
}

impl Head {
    pub fn of(e: &TelemetryEvent) -> Head {
        Head {
            id: e.id,
            timestamp: e.timestamp,
            kind: kind_code(e),
            level: level_code(e),
            service: e.service.clone(),
            environment: e.environment.clone(),
            trace_id: e.trace_id.clone(),
            span_id: e.span_id.clone(),
            message: e.message().to_string(),
        }
    }
}

/// Rebuild a full event from its head and an encoded body.
pub fn decode_with_body(head: Head, r: &mut Reader) -> Result<TelemetryEvent, DecodeError> {
    decode_body_as(head, r, None)
}

/// A resource read inline, shared if empty.
fn shared(f: Fields) -> Arc<Fields> {
    if f.is_empty() { Fields::shared_empty() } else { Arc::new(f) }
}

/// The body with the resource inline (`None`) or as an index into
/// `table` (see [`encode_body_as`]).
fn decode_body_as(head: Head, r: &mut Reader, table: Option<&[Arc<Fields>]>) -> Result<TelemetryEvent, DecodeError> {
    let observed = Timestamp(head.timestamp.0.wrapping_add(r.varint_i()?));
    let resource = match table {
        None => shared(decode_fields(r)?),
        Some(t) => match r.varint()? {
            0 => Fields::shared_empty(),
            i => t.get(i as usize - 1).cloned().ok_or(DecodeError::Invalid("resource index out of range"))?,
        },
    };
    let attributes = decode_fields(r)?;
    let payload = match head.kind {
        1 => {
            let level = Level::from_code(head.level).ok_or(DecodeError::Invalid("log level"))?;
            let message_template = r.opt_string()?;
            let exception = match r.u8()? {
                0 => None,
                1 => Some(Exception { kind: r.opt_string()?, message: r.opt_string()?, stack_trace: r.opt_string()? }),
                t => return Err(DecodeError::InvalidTag(t)),
            };
            TelemetryPayload::Log(LogEvent { level, message: head.message, message_template, exception })
        }
        2 => {
            let parent_span_id = r.opt_string()?.map(SpanId::from_stored);
            let kind = SpanKind::from_code(r.u8()?);
            let duration_nanos = r.varint()?;
            let code = StatusCode::from_code(r.u8()?);
            let message = r.opt_string()?;
            let n = r.len()?;
            let mut events = Vec::with_capacity(n);
            for _ in 0..n {
                let ts = Timestamp(head.timestamp.0.wrapping_add(r.varint_i()?));
                events.push(SpanEventItem { timestamp: ts, name: r.string()?, attributes: decode_fields(r)? });
            }
            let n = r.len()?;
            let mut links = Vec::with_capacity(n);
            for _ in 0..n {
                links.push(SpanLink {
                    trace_id: TraceId::from_stored(r.string()?),
                    span_id: SpanId::from_stored(r.string()?),
                    attributes: decode_fields(r)?,
                });
            }
            TelemetryPayload::Span(SpanEvent {
                parent_span_id,
                name: head.message,
                kind,
                duration_nanos,
                status: SpanStatus { code, message },
                events,
                links,
            })
        }
        3 => {
            let description = r.opt_string()?;
            let unit = r.opt_string()?;
            let kind = MetricKind::from_code(r.u8()?).ok_or(DecodeError::Invalid("metric kind"))?;
            let temporality = Temporality::from_code(r.u8()?);
            let monotonic = r.u8()? != 0;
            let value = match r.u8()? {
                0 => MetricValue::Number(r.f64()?),
                1 => {
                    let count = r.varint()?;
                    let flags = r.u8()?;
                    let mut opt = |bit: u8| -> Result<Option<f64>, DecodeError> {
                        if flags & bit != 0 { Ok(Some(r.f64()?)) } else { Ok(None) }
                    };
                    let sum = opt(1)?;
                    let min = opt(2)?;
                    let max = opt(4)?;
                    let n = r.len()?;
                    let mut bucket_counts = Vec::with_capacity(n);
                    for _ in 0..n {
                        bucket_counts.push(r.varint()?);
                    }
                    let n = r.len()?;
                    let mut explicit_bounds = Vec::with_capacity(n);
                    for _ in 0..n {
                        explicit_bounds.push(r.f64()?);
                    }
                    MetricValue::Histogram(Histogram { count, sum, min, max, bucket_counts, explicit_bounds })
                }
                t => return Err(DecodeError::InvalidTag(t)),
            };
            TelemetryPayload::Metric(MetricEvent {
                name: head.message,
                description,
                unit,
                kind,
                temporality,
                monotonic,
                value,
            })
        }
        k => return Err(DecodeError::InvalidTag(k)),
    };
    Ok(TelemetryEvent {
        id: head.id,
        timestamp: head.timestamp,
        observed_timestamp: observed,
        service: head.service,
        environment: head.environment,
        trace_id: head.trace_id,
        span_id: head.span_id,
        resource,
        attributes,
        payload,
    })
}

/// Self-contained event encoding, resource inline (codec 1 batches).
pub fn encode_event(w: &mut Writer, e: &TelemetryEvent) {
    encode_head(w, e);
    encode_body(w, e);
}

fn encode_head(w: &mut Writer, e: &TelemetryEvent) {
    w.varint(e.id.0);
    w.i64(e.timestamp.0);
    w.u8(kind_code(e));
    w.u8(level_code(e));
    w.opt_str(e.service.as_deref());
    w.opt_str(e.environment.as_deref());
    w.opt_str(e.trace_id.as_ref().map(TraceId::as_str));
    w.opt_str(e.span_id.as_ref().map(SpanId::as_str));
    w.str(e.message());
}

pub fn decode_event(r: &mut Reader) -> Result<TelemetryEvent, DecodeError> {
    let head = decode_head(r)?;
    decode_with_body(head, r)
}

fn decode_head(r: &mut Reader) -> Result<Head, DecodeError> {
    Ok(Head {
        id: EventId(r.varint()?),
        timestamp: Timestamp(r.i64()?),
        kind: r.u8()?,
        level: r.u8()?,
        service: r.opt_string()?,
        environment: r.opt_string()?,
        trace_id: r.opt_string()?.map(TraceId::from_stored),
        span_id: r.opt_string()?.map(SpanId::from_stored),
        message: r.string()?,
    })
}

/// The resource table of a batch about to be encoded (codec 2): each
/// distinct non-empty resource once, and every event's index into it.
/// Events that share a resource (`Arc`) are found by pointer; others with
/// identical resources, such as native events each carrying their own copy,
/// by their encoding.
pub struct BatchPlan {
    resources: u64,
    table: Vec<u8>,
    index: Vec<u64>,
}

pub fn plan_batch(events: &[TelemetryEvent]) -> BatchPlan {
    let mut plan = BatchPlan { resources: 0, table: Vec::new(), index: Vec::with_capacity(events.len()) };
    let mut by_ptr: HashMap<*const Fields, u64> = HashMap::new();
    let mut by_bytes: HashMap<Vec<u8>, u64> = HashMap::new();
    for e in events {
        if e.resource.is_empty() {
            plan.index.push(0);
            continue;
        }
        let ptr = Arc::as_ptr(&e.resource);
        let i = match by_ptr.get(&ptr) {
            Some(&i) => i,
            None => {
                let mut w = Writer::new();
                encode_fields(&mut w, &e.resource);
                let i = *by_bytes.entry(w.into_inner()).or_insert_with_key(|bytes| {
                    plan.table.extend_from_slice(bytes);
                    plan.resources += 1;
                    plan.resources
                });
                by_ptr.insert(ptr, i);
                i
            }
        };
        plan.index.push(i);
    }
    plan
}

/// Encode a batch: codec version, resource table, count, events.
pub fn encode_batch(events: &[TelemetryEvent]) -> Vec<u8> {
    let plan = plan_batch(events);
    let mut out = Vec::with_capacity(plan.table.len() + events.len() * 256);
    let r: std::io::Result<()> = encode_batch_with(events, &plan, &mut Writer::new(), |b| {
        out.extend_from_slice(b);
        Ok(())
    });
    r.expect("writing to a Vec cannot fail");
    out
}

/// Encode a batch piece by piece: `put` receives, in order, exactly the
/// bytes of [`encode_batch`], one event at a time, so the batch is never
/// held whole. `plan` is [`plan_batch`] of the same events; `scratch` holds
/// the piece being encoded.
pub fn encode_batch_with<E>(
    events: &[TelemetryEvent],
    plan: &BatchPlan,
    scratch: &mut Writer,
    mut put: impl FnMut(&[u8]) -> Result<(), E>,
) -> Result<(), E> {
    scratch.buf.clear();
    scratch.u8(CODEC_VERSION);
    scratch.varint(plan.resources);
    put(&scratch.buf)?;
    put(&plan.table)?;
    scratch.buf.clear();
    scratch.varint(events.len() as u64);
    put(&scratch.buf)?;
    for (e, &i) in events.iter().zip(&plan.index) {
        scratch.buf.clear();
        encode_head(scratch, e);
        encode_body_as(scratch, e, Some(i));
        put(&scratch.buf)?;
    }
    Ok(())
}

/// Length of [`encode_batch`]'s output, computed one event at a time.
pub fn encoded_batch_len(events: &[TelemetryEvent], plan: &BatchPlan, scratch: &mut Writer) -> usize {
    let mut n = 0;
    let r: Result<(), std::convert::Infallible> = encode_batch_with(events, plan, scratch, |b| {
        n += b.len();
        Ok(())
    });
    let Ok(()) = r;
    n
}

/// A batch in codec 1, as releases before 0.1.2 wrote it (every resource
/// inline, no table). For tests of data written by those releases.
#[doc(hidden)]
pub fn encode_batch_v1(events: &[TelemetryEvent]) -> Vec<u8> {
    let mut w = Writer::with_capacity(events.len() * 256);
    w.u8(1);
    w.varint(events.len() as u64);
    for e in events {
        encode_event(&mut w, e);
    }
    w.into_inner()
}

/// Decode a batch in codec 1 or 2. Events of one resource table entry
/// share it.
pub fn decode_batch(bytes: &[u8]) -> Result<Vec<TelemetryEvent>, DecodeError> {
    let mut r = Reader::new(bytes);
    let version = r.u8()?;
    let table = match version {
        1 => None,
        2 => {
            let n = r.len()?;
            let mut table = Vec::with_capacity(n);
            for _ in 0..n {
                table.push(shared(decode_fields(&mut r)?));
            }
            Some(table)
        }
        v => return Err(DecodeError::UnsupportedVersion(v as u16)),
    };
    let n = r.len()?;
    let mut events = Vec::with_capacity(n);
    for _ in 0..n {
        let head = decode_head(&mut r)?;
        events.push(decode_body_as(head, &mut r, table.as_deref())?);
    }
    if !r.is_empty() {
        return Err(DecodeError::Invalid("trailing bytes after batch"));
    }
    Ok(events)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use telemetry::testing::any_event;

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(512))]

        /// encode(event) → decode() == event
        #[test]
        fn event_round_trip(e in any_event()) {
            let mut w = Writer::new();
            encode_event(&mut w, &e);
            let mut r = Reader::new(&w.buf);
            let back = decode_event(&mut r).unwrap();
            prop_assert!(r.is_empty());
            prop_assert_eq!(back, e);
        }

        #[test]
        fn batch_round_trip(events in proptest::collection::vec(any_event(), 0..8)) {
            let bytes = encode_batch(&events);
            prop_assert_eq!(&decode_batch(&bytes).unwrap(), &events);
            // Batches written before 0.1.2 still read the same.
            prop_assert_eq!(decode_batch(&encode_batch_v1(&events)).unwrap(), events);
        }

        /// Arbitrary garbage must produce an error, never a panic.
        #[test]
        fn garbage_never_panics(bytes in proptest::collection::vec(any::<u8>(), 0..256)) {
            let _ = decode_batch(&bytes);
        }

        /// Truncating a valid batch anywhere must fail cleanly.
        #[test]
        fn truncation_detected(events in proptest::collection::vec(any_event(), 1..4), cut in any::<prop::sample::Index>()) {
            let bytes = encode_batch(&events);
            let at = cut.index(bytes.len());
            prop_assert!(decode_batch(&bytes[..at]).is_err());
        }
    }

    fn with_resource(id: u64, resource: Arc<Fields>) -> TelemetryEvent {
        TelemetryEvent {
            id: EventId(id),
            timestamp: Timestamp(id as i64),
            observed_timestamp: Timestamp(id as i64),
            service: None,
            environment: None,
            trace_id: None,
            span_id: None,
            resource,
            attributes: Fields::new(),
            payload: TelemetryPayload::Log(LogEvent {
                level: Level::Information,
                message: format!("m{id}"),
                message_template: None,
                exception: None,
            }),
        }
    }

    fn big_resource(tag: &str) -> Fields {
        (0..50).map(|k| (format!("attr.{k}"), Value::from(format!("{tag}-{k:020}")))).collect()
    }

    // Codec 2 stores each distinct resource once per batch, whether events
    // share it or carry equal copies, and decoded events share it again.
    #[test]
    fn resources_are_stored_once_per_batch() {
        let a = Arc::new(big_resource("a"));
        let events: Vec<TelemetryEvent> = (0..100)
            .map(|i| match i % 4 {
                0 | 1 => with_resource(i, a.clone()),
                2 => with_resource(i, Arc::new(big_resource("b"))), // an equal copy each time
                _ => with_resource(i, Fields::shared_empty()),
            })
            .collect();
        let plan = plan_batch(&events);
        assert_eq!(plan.resources, 2);
        let v2 = encode_batch(&events);
        let v1 = encode_batch_v1(&events);
        assert!(v2.len() * 10 < v1.len(), "codec 2: {} bytes, codec 1: {}", v2.len(), v1.len());
        let back = decode_batch(&v2).unwrap();
        assert_eq!(back, events);
        assert!(Arc::ptr_eq(&back[0].resource, &back[1].resource));
        assert!(Arc::ptr_eq(&back[2].resource, &back[6].resource));
        assert!(!Arc::ptr_eq(&back[0].resource, &back[2].resource));
        assert!(back[3].resource.is_empty());
    }

    #[test]
    fn resource_index_out_of_range_is_an_error() {
        let mut bytes = encode_batch(&[with_resource(1, Arc::new(big_resource("a")))]);
        // Drop the table entry but keep the event's reference to it.
        let mut w = Writer::new();
        encode_fields(&mut w, &big_resource("a"));
        let entry = w.into_inner();
        let at = bytes.windows(entry.len()).position(|x| x == entry).unwrap();
        bytes.drain(at..at + entry.len());
        bytes[1] = 0;
        assert_eq!(decode_batch(&bytes), Err(DecodeError::Invalid("resource index out of range")));
    }

    #[test]
    fn deep_nesting_rejected() {
        let mut w = Writer::new();
        for _ in 0..(MAX_DEPTH + 2) {
            w.u8(TAG_ARRAY);
            w.varint(1);
        }
        w.u8(TAG_NULL);
        assert_eq!(decode_value(&mut Reader::new(&w.buf), 0), Err(DecodeError::DepthExceeded));
    }

    #[test]
    fn version_checked() {
        let mut bytes = encode_batch(&[]);
        bytes[0] = 99;
        assert_eq!(decode_batch(&bytes), Err(DecodeError::UnsupportedVersion(99)));
        bytes[0] = 3;
        assert_eq!(decode_batch(&bytes), Err(DecodeError::UnsupportedVersion(3)));
    }
}
