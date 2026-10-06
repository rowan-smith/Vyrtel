//! Versioned binary encoding of telemetry events.
//!
//! An event is split into a fixed *head* (fields that segments store as
//! separate columns so queries can skip the rest) and a *body*. The WAL
//! stores head + body back to back; segments store them in different
//! column chunks. Both use the functions in this module, so there is exactly
//! one definition of how an event becomes bytes.
//!
//! The layout is documented in `docs/storage-format.md`. Any change must bump
//! [`CODEC_VERSION`] and keep a decoder for older versions.

use telemetry::*;

use crate::encoding::{Reader, Writer};
use crate::error::DecodeError;

/// Version of the event codec, written at the start of every WAL record.
pub const CODEC_VERSION: u8 = 1;

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

/// The body: everything not stored as a dedicated segment column.
pub fn encode_body(w: &mut Writer, e: &TelemetryEvent) {
    // Observed time is usually within milliseconds of the event time, so a
    // zigzag delta is a couple of bytes instead of eight.
    w.varint_i(e.observed_timestamp.0.wrapping_sub(e.timestamp.0));
    encode_fields(w, &e.resource);
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
    let observed = Timestamp(head.timestamp.0.wrapping_add(r.varint_i()?));
    let resource = decode_fields(r)?;
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

/// Self-contained event encoding (used by WAL records).
pub fn encode_event(w: &mut Writer, e: &TelemetryEvent) {
    w.varint(e.id.0);
    w.i64(e.timestamp.0);
    w.u8(kind_code(e));
    w.u8(level_code(e));
    w.opt_str(e.service.as_deref());
    w.opt_str(e.environment.as_deref());
    w.opt_str(e.trace_id.as_ref().map(TraceId::as_str));
    w.opt_str(e.span_id.as_ref().map(SpanId::as_str));
    w.str(e.message());
    encode_body(w, e);
}

pub fn decode_event(r: &mut Reader) -> Result<TelemetryEvent, DecodeError> {
    let head = Head {
        id: EventId(r.varint()?),
        timestamp: Timestamp(r.i64()?),
        kind: r.u8()?,
        level: r.u8()?,
        service: r.opt_string()?,
        environment: r.opt_string()?,
        trace_id: r.opt_string()?.map(TraceId::from_stored),
        span_id: r.opt_string()?.map(SpanId::from_stored),
        message: r.string()?,
    };
    decode_with_body(head, r)
}

/// Encode a batch: codec version, count, events.
pub fn encode_batch(events: &[TelemetryEvent]) -> Vec<u8> {
    let mut w = Writer::with_capacity(events.len() * 256);
    w.u8(CODEC_VERSION);
    w.varint(events.len() as u64);
    for e in events {
        encode_event(&mut w, e);
    }
    w.into_inner()
}

pub fn decode_batch(bytes: &[u8]) -> Result<Vec<TelemetryEvent>, DecodeError> {
    let mut r = Reader::new(bytes);
    let version = r.u8()?;
    if version != CODEC_VERSION {
        return Err(DecodeError::UnsupportedVersion(version as u16));
    }
    let n = r.len()?;
    let mut events = Vec::with_capacity(n);
    for _ in 0..n {
        events.push(decode_event(&mut r)?);
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
            prop_assert_eq!(decode_batch(&bytes).unwrap(), events);
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
    }
}
