//! A pass over an OTLP protobuf body before it is decoded. It counts the
//! records (log records, spans, metric data points), measures how deeply
//! attribute values nest, and estimates the heap `prost` will allocate for
//! the decoded structures, so a body over any [`Limits`] is refused before
//! any of it is decoded. It allocates nothing and reads every byte once.
//!
//! Field numbers follow `proto.rs`. Fields `proto.rs` does not declare are
//! skipped by `prost` without allocating, so they are skipped here too.
//! The estimate is an upper bound: strings and bytes at least 8 (`Vec`'s
//! smallest allocation), repeated fields at twice their element count
//! (`Vec` doubling, which also covers `prost` merging repeated occurrences
//! of a message field into one vector).

use super::proto::*;
use crate::IngestError;
use crate::limits::Limits;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Estimate {
    pub records: usize,
    pub bytes: usize,
}

type R = Result<(), IngestError>;

fn malformed(what: &str) -> IngestError {
    IngestError::Malformed(format!("invalid OTLP payload: {what}"))
}

/// One field's payload.
enum Wire<'a> {
    Scalar,
    Len(&'a [u8]),
}

fn varint(buf: &mut &[u8]) -> Result<u64, IngestError> {
    let mut v = 0u64;
    for i in 0..10 {
        let (&b, rest) = buf.split_first().ok_or_else(|| malformed("truncated varint"))?;
        *buf = rest;
        v |= u64::from(b & 0x7f) << (7 * i);
        if b < 0x80 {
            return Ok(v);
        }
    }
    Err(malformed("varint too long"))
}

fn take<'a>(buf: &mut &'a [u8], n: usize) -> Result<&'a [u8], IngestError> {
    if buf.len() < n {
        return Err(malformed("truncated field"));
    }
    let (head, rest) = buf.split_at(n);
    *buf = rest;
    Ok(head)
}

/// Skip one field of wire type `wt` whose key has been read. Groups
/// (deprecated, but accepted by `prost`) are skipped to their end.
fn skip<'a>(buf: &mut &'a [u8], field: u64, wt: u64, groups: usize) -> Result<Wire<'a>, IngestError> {
    Ok(match wt {
        0 => {
            varint(buf)?;
            Wire::Scalar
        }
        1 => {
            take(buf, 8)?;
            Wire::Scalar
        }
        5 => {
            take(buf, 4)?;
            Wire::Scalar
        }
        2 => {
            let n = usize::try_from(varint(buf)?).map_err(|_| malformed("length too large"))?;
            Wire::Len(take(buf, n)?)
        }
        3 => {
            if groups >= 64 {
                return Err(malformed("groups nested too deeply"));
            }
            loop {
                let key = varint(buf)?;
                let (f, w) = (key >> 3, key & 7);
                if w == 4 {
                    if f != field {
                        return Err(malformed("mismatched end group"));
                    }
                    break;
                }
                skip(buf, f, w, groups + 1)?;
            }
            Wire::Scalar
        }
        _ => return Err(malformed("invalid wire type")),
    })
}

/// Visit every field of a message.
fn each(mut buf: &[u8], mut f: impl FnMut(u64, Wire<'_>) -> R) -> R {
    while !buf.is_empty() {
        let key = varint(&mut buf)?;
        let (field, wt) = (key >> 3, key & 7);
        if field == 0 {
            return Err(malformed("field number 0"));
        }
        let wire = skip(&mut buf, field, wt, 0)?;
        f(field, wire)?;
    }
    Ok(())
}

struct Scan {
    limits: Limits,
    est: Estimate,
}

impl Scan {
    fn add(&mut self, bytes: usize) -> R {
        self.est.bytes = self.est.bytes.saturating_add(bytes);
        if self.est.bytes > self.limits.max_decoded_bytes {
            return Err(IngestError::TooLarge { limit_bytes: self.limits.max_decoded_bytes });
        }
        Ok(())
    }

    fn record(&mut self) -> R {
        self.est.records += 1;
        if self.est.records > self.limits.max_events {
            return Err(IngestError::TooManyEvents { limit: self.limits.max_events });
        }
        Ok(())
    }

    fn string(&mut self, b: &[u8]) -> R {
        self.add(if b.is_empty() { 0 } else { b.len().max(8) })
    }

    fn repeated<T>(&mut self, n: usize) -> R {
        self.add(if n == 0 { 0 } else { n.saturating_mul(2).max(4).saturating_mul(size_of::<T>()) })
    }

    /// Fields of `buf` numbered `field` (repeated `T`), each scanned by `f`.
    fn children<T>(&mut self, buf: &[u8], field: u64, mut f: impl FnMut(&mut Self, &[u8]) -> R) -> R {
        let mut n = 0;
        each(buf, |num, w| match w {
            Wire::Len(b) if num == field => {
                n += 1;
                f(self, b)
            }
            _ => Ok(()),
        })?;
        self.repeated::<T>(n)
    }

    fn any_value(&mut self, buf: &[u8], depth: usize) -> R {
        if depth > self.limits.max_depth {
            return Err(IngestError::TooDeep { limit: self.limits.max_depth });
        }
        each(buf, |num, w| match (num, w) {
            (1 | 7, Wire::Len(b)) => self.string(b),
            (5, Wire::Len(b)) => self.children::<AnyValue>(b, 1, |s, v| s.any_value(v, depth + 1)),
            (6, Wire::Len(b)) => self.children::<KeyValue>(b, 1, |s, kv| s.key_value(kv, depth + 1)),
            _ => Ok(()),
        })
    }

    fn key_value(&mut self, buf: &[u8], depth: usize) -> R {
        each(buf, |num, w| match (num, w) {
            (1, Wire::Len(b)) => self.string(b),
            (2, Wire::Len(b)) => self.any_value(b, depth),
            _ => Ok(()),
        })
    }

    /// Repeated `KeyValue` attributes numbered `field` in `buf`.
    fn attributes(&mut self, buf: &[u8], field: u64) -> R {
        self.children::<KeyValue>(buf, field, |s, kv| s.key_value(kv, 0))
    }

    fn resource(&mut self, buf: &[u8]) -> R {
        self.attributes(buf, 1)
    }

    fn scope(&mut self, buf: &[u8]) -> R {
        each(buf, |num, w| match (num, w) {
            (1 | 2, Wire::Len(b)) => self.string(b),
            _ => Ok(()),
        })?;
        self.attributes(buf, 3)
    }

    /// The fields of a message that are strings or bytes.
    fn strings(&mut self, buf: &[u8], fields: &[u64]) -> R {
        each(buf, |num, w| match w {
            Wire::Len(b) if fields.contains(&num) => self.string(b),
            _ => Ok(()),
        })
    }

    fn log_record(&mut self, buf: &[u8]) -> R {
        self.record()?;
        self.strings(buf, &[3, 9, 10, 12])?;
        each(buf, |num, w| match (num, w) {
            (5, Wire::Len(b)) => self.any_value(b, 0),
            _ => Ok(()),
        })?;
        self.attributes(buf, 6)
    }

    fn span(&mut self, buf: &[u8]) -> R {
        self.record()?;
        self.strings(buf, &[1, 2, 3, 4, 5])?;
        self.attributes(buf, 9)?;
        self.children::<SpanEvent>(buf, 11, |s, e| {
            s.strings(e, &[2])?;
            s.attributes(e, 3)
        })?;
        self.children::<SpanLink>(buf, 13, |s, l| {
            s.strings(l, &[1, 2])?;
            s.attributes(l, 4)
        })?;
        each(buf, |num, w| match (num, w) {
            (15, Wire::Len(b)) => self.strings(b, &[2]),
            _ => Ok(()),
        })
    }

    /// Repeated fixed64 / double values numbered `field`, packed or not.
    fn fixed64s(&mut self, buf: &[u8], field: u64) -> R {
        let mut n = 0;
        each(buf, |num, w| {
            if num == field {
                n += match w {
                    Wire::Len(b) => b.len() / 8,
                    Wire::Scalar => 1,
                };
            }
            Ok(())
        })?;
        self.repeated::<u64>(n)
    }

    fn data_points<T>(&mut self, buf: &[u8], point: fn(&mut Self, &[u8]) -> R) -> R {
        self.children::<T>(buf, 1, |s, p| {
            s.record()?;
            point(s, p)
        })
    }

    fn metric(&mut self, buf: &[u8]) -> R {
        self.strings(buf, &[1, 2, 3])?;
        each(buf, |num, w| match (num, w) {
            (5 | 7, Wire::Len(b)) => self.data_points::<NumberDataPoint>(b, |s, p| s.attributes(p, 7)),
            (9, Wire::Len(b)) => self.data_points::<HistogramDataPoint>(b, |s, p| {
                s.attributes(p, 9)?;
                s.fixed64s(p, 6)?;
                s.fixed64s(p, 7)
            }),
            (10, Wire::Len(b)) => self.data_points::<ExponentialHistogramDataPoint>(b, |s, p| s.attributes(p, 1)),
            (11, Wire::Len(b)) => self.data_points::<SummaryDataPoint>(b, |s, p| s.attributes(p, 7)),
            _ => Ok(()),
        })
    }

    /// `Export*ServiceRequest` → `Resource*` (resource 1, scopes 2) →
    /// `Scope*` (scope 1, records 2), with `record` scanning each record.
    fn request<Res, Sc, Rec>(&mut self, buf: &[u8], record: fn(&mut Self, &[u8]) -> R) -> R {
        self.children::<Res>(buf, 1, |s, res| {
            each(res, |num, w| match (num, w) {
                (1, Wire::Len(b)) => s.resource(b),
                _ => Ok(()),
            })?;
            s.children::<Sc>(res, 2, |s, sc| {
                each(sc, |num, w| match (num, w) {
                    (1, Wire::Len(b)) => s.scope(b),
                    _ => Ok(()),
                })?;
                s.children::<Rec>(sc, 2, record)
            })
        })
    }
}

fn run(limits: Limits, f: impl FnOnce(&mut Scan) -> R) -> Result<Estimate, IngestError> {
    let mut s = Scan { limits, est: Estimate::default() };
    f(&mut s)?;
    Ok(s.est)
}

pub fn logs(body: &[u8], limits: Limits) -> Result<Estimate, IngestError> {
    run(limits, |s| s.request::<ResourceLogs, ScopeLogs, LogRecord>(body, Scan::log_record))
}

pub fn traces(body: &[u8], limits: Limits) -> Result<Estimate, IngestError> {
    run(limits, |s| s.request::<ResourceSpans, ScopeSpans, Span>(body, Scan::span))
}

pub fn metrics(body: &[u8], limits: Limits) -> Result<Estimate, IngestError> {
    // Metrics are not records; their data points are, counted inside.
    run(limits, |s| s.request::<ResourceMetrics, ScopeMetrics, Metric>(body, Scan::metric))
}
