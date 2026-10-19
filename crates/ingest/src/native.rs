//! Native event format: JSON (single object, array, or `{"events": [...]}`)
//! and NDJSON (one object per line).
//!
//! Field names follow docs/api.md and accept common aliases, including
//! Serilog's compact format (CLEF: `@t`, `@l`, `@m`, `@mt`, `@x`, ...).
//! Unknown top-level fields become structured properties, so nothing a
//! producer sends is silently dropped.

use std::fmt;
use std::sync::Arc;

use serde::de::{DeserializeSeed, Deserializer, IgnoredAny, MapAccess, SeqAccess, Visitor};
use serde_json::{Map, Value as Json};
use telemetry::*;

use crate::jsontree::{self, Ctx};
use crate::limits::{self, Budget, Limits};
use crate::{IngestError, render_template};

pub enum Format {
    Json,
    NdJson,
}

impl Format {
    pub fn from_content_type(ct: Option<&str>) -> Result<Format, IngestError> {
        let ct = ct.unwrap_or("application/json");
        let mime = ct.split(';').next().unwrap_or("").trim().to_ascii_lowercase();
        match mime.as_str() {
            "" | "application/json" | "text/json" => Ok(Format::Json),
            "application/x-ndjson"
            | "application/ndjson"
            | "application/jsonl"
            | "application/x-jsonlines"
            | "application/vnd.serilog.clef" => Ok(Format::NdJson),
            other => Err(IngestError::UnsupportedContentType(other.to_string())),
        }
    }
}

/// Parse a request body into log events, within `limits`. Batches are
/// all-or-nothing: one invalid event, or any limit passed, rejects the
/// request, with the event's index in the error.
///
/// Events are converted one at a time as they are parsed, so the working
/// set is the batch built so far plus one event's JSON. The number of
/// events is counted before parsing (lines, or a structural scan of a JSON
/// array or of `{"events": [...]}` spelt plainly), so a batch with too many
/// is refused at once and the batch is allocated exactly once. Any other
/// envelope (an escaped or repeated key) is streamed too, and counted as
/// its events are read.
pub fn parse(body: &[u8], format: Format, now: Timestamp, limits: Limits) -> Result<Vec<TelemetryEvent>, IngestError> {
    parse_in(body, format, now, &mut Budget::new(limits))
}

fn parse_in(
    body: &[u8],
    format: Format,
    now: Timestamp,
    budget: &mut Budget,
) -> Result<Vec<TelemetryEvent>, IngestError> {
    let limits = budget.limits();
    let mut b = Batch { out: Vec::new(), now, empty: Fields::shared_empty() };
    match format {
        Format::Json => {
            // `{"events": [...]}` is streamed like a bare array.
            let array = jsontree::envelope(body).unwrap_or(body);
            match jsontree::count_array_elements(array) {
                Some(n) => b.array(array, n, &mut *budget)?,
                None => b.object(body, &mut *budget)?,
            }
        }
        Format::NdJson => {
            let text =
                std::str::from_utf8(body).map_err(|_| IngestError::Malformed("body is not valid UTF-8".into()))?;
            let lines = || text.lines().map(str::trim).enumerate().filter(|(_, l)| !l.is_empty());
            b.reserve(lines().count(), &mut *budget)?;
            for (i, line) in lines() {
                let before = budget.used();
                let mut ctx = Ctx::new(&mut *budget, limits.max_depth);
                let v = jsontree::from_slice(line.as_bytes(), &mut ctx).map_err(|e| match e {
                    IngestError::Malformed(m) => IngestError::Malformed(format!("line {}: {m}", i + 1)),
                    other => other,
                })?;
                let containers = ctx.containers;
                let index = b.out.len();
                b.convert(v, index, before, containers, &mut *budget)?;
            }
        }
    }
    if b.out.is_empty() {
        return Err(IngestError::Malformed("no events in request".into()));
    }
    Ok(b.out)
}

/// The events of one request as they are built.
struct Batch {
    out: Vec<TelemetryEvent>,
    now: Timestamp,
    /// Shared by every event without a resource.
    empty: Arc<Fields>,
}

impl Batch {
    /// Room for exactly `n` events, refused when there are too many or
    /// their fixed size alone is over the byte limit.
    fn reserve(&mut self, n: usize, budget: &mut Budget) -> Result<(), IngestError> {
        budget.check_events(n)?;
        limits::reserve(&mut self.out, n, budget)
    }

    /// Convert one event's JSON, whose tree took the budget from `before`
    /// to where it is now. Conversion moves strings but allocates every
    /// container again while the tree is still held, so that is charged
    /// first; the tree is credited back once it is gone.
    fn convert(
        &mut self,
        v: Json,
        index: usize,
        before: usize,
        containers: usize,
        budget: &mut Budget,
    ) -> Result<(), IngestError> {
        budget.event()?;
        let tree = budget.used() - before;
        budget.charge(containers)?;
        let e = match v {
            Json::Object(o) => event_from_object(o, self.now, &self.empty)
                .map_err(|message| IngestError::InvalidEvent { index, message })?,
            _ => return Err(IngestError::InvalidEvent { index, message: "expected a JSON object".into() }),
        };
        budget.release(tree + containers);
        let resource = if Arc::ptr_eq(&e.resource, &self.empty) { 0 } else { limits::shared_fields(&e.resource) };
        budget.charge(e.heap_size() + resource)?;
        limits::push(&mut self.out, e, budget)
    }

    /// A JSON array of `n` events, parsed one element at a time.
    fn array(&mut self, body: &[u8], n: usize, budget: &mut Budget) -> Result<(), IngestError> {
        self.reserve(n, budget)?;
        let mut de = serde_json::Deserializer::from_slice(body);
        let parsed =
            de.deserialize_seq(Elements { batch: self, budget: &mut *budget, refused: None }).and_then(|()| de.end());
        parsed.map_err(|e| budget.take_failure().unwrap_or_else(|| jsontree::syntax_error(&e, budget)))
    }

    /// A top-level object that is not a plainly spelt envelope. It is
    /// first streamed as `{"events": [...]}` (see [`Envelope`]), so that a
    /// batch with too many events is refused as soon as the count passes
    /// the limit and the whole tree is never built. Whether it is an
    /// envelope is only known at its last key, so an error found while
    /// streaming is kept until then and returned only if every key was
    /// `events`. Otherwise (another key, a limit reached inside one
    /// element, invalid JSON) the object is read again whole, within the
    /// same limits, which decides what it is: an ordinary event with an
    /// `events` property is stored like any other, wherever its keys are.
    fn object(&mut self, body: &[u8], budget: &mut Budget) -> Result<(), IngestError> {
        let mut de = serde_json::Deserializer::from_slice(body);
        let streamed =
            de.deserialize_map(Envelope { batch: self, budget: &mut *budget }).and_then(|r| de.end().map(|()| r));
        match streamed {
            Ok(None) => return Ok(()),
            Ok(Some(e)) => return Err(e),
            Err(_) => {}
        }
        self.out = Vec::new();
        budget.reset();
        self.whole(body, budget)
    }

    /// A single event, or an envelope with an invalid event, held whole.
    fn whole(&mut self, body: &[u8], budget: &mut Budget) -> Result<(), IngestError> {
        let max_depth = budget.limits().max_depth;
        let before = budget.used();
        // The envelope puts its events two levels down.
        let mut ctx = Ctx::new(budget, max_depth + 2);
        let v = jsontree::from_slice(body, &mut ctx)?;
        let containers = ctx.containers;
        match v {
            Json::Object(mut o) if o.len() == 1 && o.get("events").is_some_and(Json::is_array) => {
                let Some(Json::Array(items)) = o.remove("events") else { unreachable!() };
                self.reserve(items.len(), budget)?;
                // The whole tree is held until the last event is converted.
                let tree = budget.used() - before;
                budget.charge(containers)?;
                for (index, item) in items.into_iter().enumerate() {
                    let at = budget.used();
                    self.convert(item, index, at, 0, budget)?;
                }
                budget.release(tree + containers);
                Ok(())
            }
            obj @ Json::Object(_) => {
                if jsontree::depth(&obj).is_some_and(|d| d > max_depth) {
                    return Err(IngestError::TooDeep { limit: max_depth });
                }
                self.reserve(1, budget)?;
                self.convert(obj, 0, before, containers, budget)
            }
            _ => Err(IngestError::Malformed("expected a JSON object or array of objects".into())),
        }
    }
}

/// The entries of a top-level object, read as `{"events": [...]}`: every
/// key must decode to `events` (escapes included; a repeated key replaces
/// the earlier array, as in any JSON object) and its value is streamed by
/// [`Elements`]. The result is the error the last array was refused for,
/// if it was. A key other than `events`, or an array that could not be
/// streamed to its end, stops it with an error and the object is read
/// whole instead.
struct Envelope<'a> {
    batch: &'a mut Batch,
    budget: &'a mut Budget,
}

impl<'de> Visitor<'de> for Envelope<'_> {
    type Value = Option<IngestError>;

    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("an object of events")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Option<IngestError>, A::Error> {
        let (mut found, mut refused) = (false, None);
        while let Some(key) = map.next_key::<String>()? {
            if key != "events" {
                return Err(serde::de::Error::custom("not an envelope"));
            }
            self.batch.out = Vec::new();
            self.budget.reset();
            refused = None;
            let array = Elements { batch: &mut *self.batch, budget: &mut *self.budget, refused: Some(&mut refused) };
            map.next_value_seed(array)?;
            found = true;
        }
        if found { Ok(refused) } else { Err(serde::de::Error::custom("not an envelope")) }
    }
}

/// The elements of a JSON array, each converted as soon as it has been
/// parsed. A top-level array is a batch for certain, so the first error is
/// returned at once. An envelope's array may turn out to be an event's
/// property instead (see [`Envelope`]): with `refused`, an error found
/// between elements (too many events, too much decoded data, an invalid
/// event) is kept there, the events built so far are dropped, and the rest
/// of the array is read without building anything.
struct Elements<'a> {
    batch: &'a mut Batch,
    budget: &'a mut Budget,
    refused: Option<&'a mut Option<IngestError>>,
}

impl<'de> DeserializeSeed<'de> for Elements<'_> {
    type Value = ();

    fn deserialize<D: Deserializer<'de>>(self, de: D) -> Result<(), D::Error> {
        de.deserialize_seq(self)
    }
}

impl<'de> Visitor<'de> for Elements<'_> {
    type Value = ();

    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("an array of events")
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<(), A::Error> {
        let limits = self.budget.limits();
        loop {
            let before = self.budget.used();
            let mut ctx = Ctx::new(&mut *self.budget, limits.max_depth);
            let Some(v) = seq.next_element_seed(ctx.seed(0))? else { return Ok(()) };
            let containers = ctx.containers;
            let index = self.batch.out.len();
            let Err(e) = self.batch.convert(v, index, before, containers, self.budget) else { continue };
            let Some(refused) = self.refused else { return Err(self.budget.fail(e)) };
            *refused = Some(e);
            self.batch.out = Vec::new();
            self.budget.reset();
            while seq.next_element::<IgnoredAny>()?.is_some() {}
            return Ok(());
        }
    }
}

fn take(o: &mut Map<String, Json>, keys: &[&str]) -> Option<Json> {
    for k in keys {
        if let Some(v) = o.remove(*k)
            && !v.is_null()
        {
            return Some(v);
        }
    }
    None
}

fn text(v: Json) -> String {
    match v {
        Json::String(s) => s,
        other => other.to_string(),
    }
}

fn fields_of(v: Json, what: &str) -> Result<Fields, String> {
    match Value::from(v) {
        Value::Object(f) => Ok(f),
        other => Err(format!("'{what}' must be an object, not {}", other.type_name())),
    }
}

fn exception_from(v: Json) -> Option<Exception> {
    match v {
        Json::String(s) if !s.trim().is_empty() => {
            // A .NET/Java style stack trace: "Namespace.Type: message\n   at ..."
            let first = s.lines().next().unwrap_or("");
            let (kind, message) = match first.split_once(": ") {
                Some((k, m)) if !k.contains(' ') && !k.is_empty() => (Some(k.to_string()), Some(m.to_string())),
                _ => (None, None),
            };
            Some(Exception { kind, message, stack_trace: Some(s) })
        }
        Json::Object(mut o) => {
            let e = Exception {
                kind: take(&mut o, &["type", "Type", "kind", "class", "name"]).map(text),
                message: take(&mut o, &["message", "Message"]).map(text),
                stack_trace: take(&mut o, &["stackTrace", "stacktrace", "stack_trace", "StackTrace", "stack"])
                    .map(text),
            };
            (!e.is_empty()).then_some(e)
        }
        _ => None,
    }
}

fn event_from_object(mut o: Map<String, Json>, now: Timestamp, empty: &Arc<Fields>) -> Result<TelemetryEvent, String> {
    let timestamp = match take(&mut o, &["timestamp", "@t", "time", "ts", "Timestamp"]) {
        None => now,
        Some(v) => parse_json_timestamp(&v).ok_or_else(|| format!("invalid timestamp {v}"))?,
    };
    let level_raw = take(&mut o, &["level", "@l", "severity", "severityText", "Level"]).map(text);
    let message = take(&mut o, &["message", "@m", "msg", "Message"]).map(text);
    let message_template = take(&mut o, &["messageTemplate", "@mt", "template", "MessageTemplate"]).map(text);
    let service = take(&mut o, &["service", "serviceName", "service.name", "Service"]).map(text);
    let environment = take(&mut o, &["environment", "env", "Environment"]).map(text);
    let trace_id = take(&mut o, &["traceId", "trace_id", "@tr", "TraceId"]).map(text);
    let span_id = take(&mut o, &["spanId", "span_id", "@sp", "SpanId"]).map(text);
    let exception = take(&mut o, &["exception", "@x", "Exception", "error"]).and_then(exception_from);

    let mut attributes = Fields::new();
    for key in ["properties", "attributes", "Properties"] {
        if let Some(v) = o.remove(key)
            && !v.is_null()
        {
            for (k, v) in fields_of(v, key)?.into_vec() {
                attributes.insert(k, v);
            }
        }
    }
    let resource = match o.remove("resource") {
        Some(Json::Null) | None => Fields::new(),
        Some(v) => fields_of(v, "resource")?,
    };
    let resource = if resource.is_empty() { empty.clone() } else { Arc::new(resource) };
    // Everything else is a property (CLEF puts properties at top level).
    for (k, v) in o {
        attributes.insert(k, Value::from(v));
    }

    let level = match level_raw.as_deref() {
        None => Level::Information,
        Some(raw) => match Level::parse(raw) {
            Some(l) => l,
            None => {
                // Keep the producer's original spelling rather than lose it.
                attributes.insert("severityText", Value::from(raw));
                Level::Information
            }
        },
    };

    let service = service.or_else(|| resource.get("service.name").and_then(Value::as_str).map(String::from));
    let environment = environment.or_else(|| {
        resource
            .get("deployment.environment.name")
            .or_else(|| resource.get("deployment.environment"))
            .and_then(Value::as_str)
            .map(String::from)
    });

    let message = match (message, &message_template) {
        (Some(m), _) => m,
        (None, Some(t)) => render_template(t, &attributes),
        (None, None) => {
            exception.as_ref().and_then(|e| e.message.clone().or_else(|| e.kind.clone())).unwrap_or_default()
        }
    };
    Ok(TelemetryEvent {
        id: EventId(0),
        timestamp,
        observed_timestamp: now,
        service: service.filter(|s| !s.is_empty()),
        environment: environment.filter(|s| !s.is_empty()),
        trace_id: trace_id.and_then(|t| TraceId::new(&t)),
        span_id: span_id.and_then(|s| SpanId::new(&s)),
        resource,
        attributes,
        payload: TelemetryPayload::Log(LogEvent { level, message, message_template, exception }),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: Timestamp = Timestamp(1_000);
    const U: Limits = Limits::UNBOUNDED;

    fn one(json: &str) -> TelemetryEvent {
        let mut v = parse(json.as_bytes(), Format::Json, NOW, U).unwrap();
        assert_eq!(v.len(), 1);
        v.pop().unwrap()
    }

    #[test]
    fn spec_example_event() {
        let e = one(r#"{
              "timestamp": "2026-10-06T10:20:13.485Z",
              "level": "Error",
              "service": "payments",
              "environment": "production",
              "message": "Payment failed",
              "traceId": "abc123",
              "properties": {"customerId": 481, "paymentProvider": "stripe", "amount": 71.50},
              "exception": {"type": "TimeoutException", "message": "Payment provider timed out", "stackTrace": "..."}
            }"#);
        assert_eq!(e.timestamp, Timestamp::parse_rfc3339("2026-10-06T10:20:13.485Z").unwrap());
        assert_eq!(e.observed_timestamp, NOW);
        assert_eq!(e.level(), Some(Level::Error));
        assert_eq!(e.service.as_deref(), Some("payments"));
        assert_eq!(e.environment.as_deref(), Some("production"));
        assert_eq!(e.message(), "Payment failed");
        assert_eq!(e.trace_id.as_ref().unwrap().as_str(), "abc123");
        assert_eq!(e.attributes.get("customerId"), Some(&Value::Int(481)));
        assert_eq!(e.attributes.get("amount"), Some(&Value::Float(71.5)));
        let x = e.as_log().unwrap().exception.as_ref().unwrap();
        assert_eq!(x.kind.as_deref(), Some("TimeoutException"));
        assert_eq!(x.stack_trace.as_deref(), Some("..."));
    }

    #[test]
    fn clef_compatibility() {
        let e = one(
            r#"{"@t":"2026-01-01T00:00:00Z","@mt":"User {UserId} logged in","@l":"Warning","UserId":7,"@x":"System.InvalidOperationException: boom\n   at X.Y()"}"#,
        );
        assert_eq!(e.message(), "User 7 logged in");
        assert_eq!(e.as_log().unwrap().message_template.as_deref(), Some("User {UserId} logged in"));
        assert_eq!(e.level(), Some(Level::Warning));
        assert_eq!(e.attributes.get("UserId"), Some(&Value::Int(7)));
        let x = e.as_log().unwrap().exception.as_ref().unwrap();
        assert_eq!(x.kind.as_deref(), Some("System.InvalidOperationException"));
        assert_eq!(x.message.as_deref(), Some("boom"));
    }

    #[test]
    fn defaults_and_unknown_levels() {
        let e = one(r#"{"message": "hi"}"#);
        assert_eq!(e.timestamp, NOW);
        assert_eq!(e.level(), Some(Level::Information));
        let e = one(r#"{"message": "hi", "level": "notice-ish"}"#);
        assert_eq!(e.level(), Some(Level::Information));
        assert_eq!(e.attributes.get("severityText"), Some(&Value::from("notice-ish")));
    }

    #[test]
    fn batches_and_ndjson() {
        let v = parse(br#"[{"message":"a"},{"message":"b"}]"#, Format::Json, NOW, U).unwrap();
        assert_eq!(v.len(), 2);
        let v = parse(br#"{"events":[{"message":"a"}]}"#, Format::Json, NOW, U).unwrap();
        assert_eq!(v[0].message(), "a");
        let v = parse(b"{\"message\":\"a\"}\n\n{\"message\":\"b\"}\r\n", Format::NdJson, NOW, U).unwrap();
        assert_eq!(v.iter().map(|e| e.message()).collect::<Vec<_>>(), ["a", "b"]);
    }

    #[test]
    fn errors() {
        assert!(matches!(parse(b"not json", Format::Json, NOW, U), Err(IngestError::Malformed(_))));
        assert!(matches!(parse(b"[]", Format::Json, NOW, U), Err(IngestError::Malformed(_))));
        assert!(matches!(parse(b"42", Format::Json, NOW, U), Err(IngestError::Malformed(_))));
        assert_eq!(
            parse(br#"[{"message":"ok"}, 5]"#, Format::Json, NOW, U),
            Err(IngestError::InvalidEvent { index: 1, message: "expected a JSON object".into() })
        );
        assert!(matches!(
            parse(br#"{"timestamp": "yesterday"}"#, Format::Json, NOW, U),
            Err(IngestError::InvalidEvent { index: 0, .. })
        ));
        assert!(matches!(
            parse(br#"{"properties": [1]}"#, Format::Json, NOW, U),
            Err(IngestError::InvalidEvent { index: 0, .. })
        ));
        let e = parse(b"{\"a\":1}\n{oops", Format::NdJson, NOW, U).unwrap_err();
        assert!(e.to_string().starts_with("line 2"));
        assert!(matches!(Format::from_content_type(Some("text/csv")), Err(IngestError::UnsupportedContentType(_))));
        assert!(matches!(Format::from_content_type(Some("application/x-ndjson; charset=utf-8")), Ok(Format::NdJson)));
    }

    #[test]
    fn unknown_fields_become_properties_and_resource_fills_service() {
        let e =
            one(r#"{"message":"m","region":"eu","resource":{"service.name":"api","deployment.environment":"prod"}}"#);
        assert_eq!(e.attributes.get("region"), Some(&Value::from("eu")));
        assert_eq!(e.service.as_deref(), Some("api"));
        assert_eq!(e.environment.as_deref(), Some("prod"));
        assert_eq!(e.resource.get("service.name"), Some(&Value::from("api")));
    }

    #[test]
    fn messy_types_are_kept() {
        let v = parse(
            br#"[{"message":"a","x":1},{"message":"b","x":"one"},{"message":"c","x":[1,{"y":null}]}]"#,
            Format::Json,
            NOW,
            U,
        )
        .unwrap();
        assert_eq!(v[0].attributes.get("x"), Some(&Value::Int(1)));
        assert_eq!(v[1].attributes.get("x"), Some(&Value::from("one")));
        assert!(matches!(v[2].attributes.get("x"), Some(Value::Array(_))));
    }

    /// `key` as a JSON string with the characters at `at` written as
    /// `\u` escapes.
    fn escaped(key: &str, at: &[usize]) -> String {
        let chars: String = key
            .chars()
            .enumerate()
            .map(|(i, c)| if at.contains(&i) { format!("{}u{:04x}", '\\', c as u32) } else { c.to_string() })
            .collect();
        format!("\"{chars}\"")
    }

    /// `n` empty events in an envelope whose key is `key` (raw JSON).
    fn envelope_of(key: &str, n: usize) -> Vec<u8> {
        format!(r#"{{{key}:[{}]}}"#, vec!["{}"; n].join(",")).into_bytes()
    }

    fn peak_of(body: &[u8], limits: Limits) -> (Result<usize, IngestError>, usize) {
        let mut budget = Budget::new(limits);
        let r = parse_in(body, Format::Json, NOW, &mut budget).map(|v| v.len());
        (r, budget.peak())
    }

    // AC2: an envelope is refused as soon as its count passes the limit,
    // however its key is spelt, and its whole tree is never built.
    #[test]
    fn every_envelope_spelling_is_counted_while_it_streams() {
        const N: usize = 40_000;
        let limits = Limits { max_events: 1000, ..U };
        let plain = peak_of(&envelope_of(r#""events""#, N), limits);
        assert_eq!(plain.0, Err(IngestError::TooManyEvents { limit: 1000 }));
        assert_eq!(plain.1, 0, "a plainly spelt envelope is counted before parsing");
        assert_eq!(escaped("events", &[2]), format!("\"ev{}u0065nts\"", '\\'));
        for key in [escaped("events", &[2]), escaped("events", &[0, 1, 2, 3, 4, 5])] {
            let key = key.as_str();
            let (r, peak) = peak_of(&envelope_of(key, N), limits);
            assert_eq!(r, Err(IngestError::TooManyEvents { limit: 1000 }), "{key}");
            // About 1,000 events; the tree alone would be 40,000 values.
            assert!(peak < N * jsontree::NODE / 4, "{key}: peak {peak}");
            assert!(peak > 0);
        }
        // Repeated keys: the last array counts, as in any JSON object.
        let repeated =
            format!(r#"{{"events":[{{"message":"a"}}],{}:[{}]}}"#, escaped("events", &[2]), vec!["{}"; N].join(","));
        assert_eq!(peak_of(repeated.as_bytes(), limits).0, Err(IngestError::TooManyEvents { limit: 1000 }));
        let repeated = br#"{"events":[{"message":"a"},{"message":"b"}],"events":[{"message":"c"}]}"#;
        let v = parse(repeated, Format::Json, NOW, U).unwrap();
        assert_eq!(v.iter().map(|e| e.message()).collect::<Vec<_>>(), ["c"]);
        // Within the limit, escaped envelopes are stored like plain ones.
        assert_eq!(peak_of(&envelope_of(&escaped("events", &[2]), 1000), limits).0, Ok(1000));
    }

    // An ordinary event with an `events` property is one event, whatever
    // the order of its keys, even when its property has more items than a
    // batch may have events: limits reached while it was being read as an
    // envelope count only if it is one.
    #[test]
    fn an_events_property_is_not_mistaken_for_a_batch() {
        let one = Limits { max_events: 1, ..U };
        let steps = r#""events":[{"message":"Step A"},{"message":"Step B"}]"#;
        for body in [
            format!(r#"{{{steps},"message":"Workflow completed"}}"#),
            format!(r#"{{"message":"Workflow completed",{steps}}}"#),
            format!(r#"{{"events":[1,2,3],"events":[{{"message":"x"}}],{steps},"message":"Workflow completed"}}"#),
        ] {
            let v = parse(body.as_bytes(), Format::Json, NOW, one).unwrap_or_else(|e| panic!("{body}: {e}"));
            assert_eq!(v.len(), 1, "{body}");
            assert_eq!(v[0].message(), "Workflow completed");
            assert!(matches!(v[0].attributes.get("events"), Some(Value::Array(a)) if a.len() == 2), "{body}");
        }
        // Too much decoded data as events, but not as one event's property.
        let wide = format!(r#"{{"events":[{}],"message":"m"}}"#, vec!["{}"; 100].join(","));
        let small = Limits { max_decoded_bytes: 20 << 10, ..U };
        assert_eq!(parse(wide.as_bytes(), Format::Json, NOW, small).map(|v| v.len()), Ok(1));
        let envelope = format!(r#"{{"events":[{}]}}"#, vec!["{}"; 100].join(","));
        let (r, _) = peak_of(envelope.as_bytes(), small);
        assert_eq!(r, Err(IngestError::TooLarge { limit_bytes: 20 << 10 }));
    }

    // A repeated `events` key: the last array decides, whether an earlier
    // one was over the limit or the last one is.
    #[test]
    fn a_repeated_envelope_key_is_decided_by_its_last_array() {
        let two = Limits { max_events: 2, ..U };
        let many = [r#"{"message":"x"}"#; 5].join(",");
        let replaced = format!(r#"{{"events":[{many}],"events":[{{"message":"c"}}]}}"#);
        let v = parse(replaced.as_bytes(), Format::Json, NOW, two).unwrap();
        assert_eq!(v.iter().map(|e| e.message()).collect::<Vec<_>>(), ["c"]);
        let over = format!(r#"{{"events":[{{"message":"c"}}],"events":[{many}]}}"#);
        assert_eq!(peak_of(over.as_bytes(), two).0, Err(IngestError::TooManyEvents { limit: 2 }));
        // An invalid event in the envelope that stands is reported with its
        // index, an invalid one in a replaced array is not.
        let invalid = br#"{"events":[{"message":"a"},5],"events":[{"message":"b"},{"message":"c"},7]}"#;
        assert_eq!(
            parse(invalid, Format::Json, NOW, U),
            Err(IngestError::InvalidEvent { index: 2, message: "expected a JSON object".into() })
        );
        let fixed = br#"{"events":[{"message":"a"},5],"events":[{"message":"b"}]}"#;
        assert_eq!(parse(fixed, Format::Json, NOW, U).map(|v| v.len()), Ok(1));
    }

    #[test]
    fn envelopes_and_lookalikes() {
        // Streamed like an array, whatever the whitespace.
        let v = parse(
            b" { \"events\" :
 [{\"message\":\"a\"}, {\"message\":\"b\"}] } ",
            Format::Json,
            NOW,
            U,
        )
        .unwrap();
        assert_eq!(v.iter().map(|e| e.message()).collect::<Vec<_>>(), ["a", "b"]);
        // Another key makes it one event with an `events` property.
        let v = parse(br#"{"events":[1],"message":"m"}"#, Format::Json, NOW, U).unwrap();
        assert_eq!((v.len(), v[0].message()), (1, "m"));
        assert!(matches!(v[0].attributes.get("events"), Some(Value::Array(_))));
        // An escaped key is still the envelope.
        let body = format!(r#"{{{}:[{{"message":"a"}}]}}"#, escaped("events", &[0]));
        let v = parse(body.as_bytes(), Format::Json, NOW, U).unwrap();
        assert_eq!(v[0].message(), "a");
        assert_eq!(
            parse(br#"{"events":[{"message":"a"}, 5]}"#, Format::Json, NOW, U),
            Err(IngestError::InvalidEvent { index: 1, message: "expected a JSON object".into() })
        );
        assert!(matches!(parse(br#"{"events":[]}"#, Format::Json, NOW, U), Err(IngestError::Malformed(_))));
        assert!(matches!(parse(br#"{"events":[{}] "#, Format::Json, NOW, U), Err(IngestError::Malformed(_))));
    }

    #[test]
    fn numeric_timestamps() {
        let e = one(r#"{"message":"m","timestamp":1791282013485}"#);
        assert_eq!(e.timestamp, Timestamp::from_millis(1_791_282_013_485));
    }
}
