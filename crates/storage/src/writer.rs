use chrono::{DateTime, Utc};
use event::{Event, EventType, LogLevel};
use serde_json::{Map, Value};
use tantivy::schema::{OwnedValue, TantivyDocument, Value as _};
use tantivy::{DateTime as TantivyDateTime, IndexWriter, Term};
use thiserror::Error;
use tracing::warn;

use crate::schema::EventSchema;

#[derive(Debug, Error)]
pub enum WriterError {
    #[error("tantivy error: {0}")]
    Tantivy(#[from] tantivy::TantivyError),
}

pub struct EventWriter {
    schema: EventSchema,
}

impl EventWriter {
    pub fn new(schema: EventSchema) -> Self {
        Self { schema }
    }

    pub fn add_events(
        &self,
        writer: &mut IndexWriter,
        events: &[Event],
    ) -> Result<(), WriterError> {
        for event in events {
            writer.add_document(self.to_document(event))?;
        }
        Ok(())
    }

    pub fn commit(&self, writer: &mut IndexWriter) -> Result<u64, WriterError> {
        Ok(writer.commit()?)
    }

    pub fn delete_before(
        &self,
        writer: &mut IndexWriter,
        cutoff: DateTime<Utc>,
    ) -> Result<(), WriterError> {
        let query = crate::search::timestamp_range(
            std::ops::Bound::Unbounded,
            std::ops::Bound::Excluded(cutoff.timestamp_nanos_opt().unwrap_or(0)),
        );
        writer.delete_query(query)?;
        Ok(())
    }

    pub fn to_document(&self, event: &Event) -> TantivyDocument {
        let f = &self.schema.fields;
        let mut doc = TantivyDocument::default();

        doc.add_text(f.id, event.id.to_string());
        // Truncate to microseconds: the doc store can't hold more, and the fast field must
        // hold exactly the same value for cursors to line up.
        doc.add_date(
            f.timestamp,
            TantivyDateTime::from_timestamp_micros(event.timestamp.timestamp_micros()),
        );
        doc.add_text(f.event_type, event.event_type.as_str());

        if let Some(level) = event.level {
            doc.add_text(f.level, level.as_str());
        }
        if let Some(ref service) = event.service {
            doc.add_text(f.service, service);
        }
        if let Some(ref environment) = event.environment {
            doc.add_text(f.environment, environment);
        }
        if let Some(ref trace_id) = event.trace_id {
            doc.add_text(f.trace_id, trace_id);
        }
        if let Some(ref span_id) = event.span_id {
            doc.add_text(f.span_id, span_id);
        }
        if let Some(ref parent_span_id) = event.parent_span_id {
            doc.add_text(f.parent_span_id, parent_span_id);
        }
        if let Some(ref message) = event.message {
            doc.add_text(f.message, message);
        }
        if let Some(ref message_template) = event.message_template {
            doc.add_text(f.message_template, message_template);
        }
        if let Some(ref stacktrace) = event.stacktrace {
            doc.add_text(f.stacktrace, stacktrace);
        }
        if let Some(duration_ns) = event.duration_ns {
            doc.add_u64(f.duration_ns, duration_ns);
        }

        if !event.attributes.is_empty() {
            let owned = serde_json_to_owned_map(&event.attributes);
            doc.add_object(f.attributes, owned);
        }

        doc
    }

    pub fn from_document(&self, doc: &TantivyDocument) -> Option<Event> {
        let f = &self.schema.fields;

        let id = doc
            .get_first(f.id)
            .and_then(|v| v.as_str())
            .and_then(|s| uuid::Uuid::parse_str(s).ok())?;

        let timestamp = doc.get_first(f.timestamp).and_then(|v| {
            let dt = v.as_datetime()?;
            DateTime::from_timestamp(
                dt.into_timestamp_secs(),
                (dt.into_timestamp_nanos() % 1_000_000_000) as u32,
            )
        })?;

        let event_type = doc
            .get_first(f.event_type)
            .and_then(|v| v.as_str())
            .and_then(EventType::parse)
            .unwrap_or(EventType::Log);

        let level = doc
            .get_first(f.level)
            .and_then(|v| v.as_str())
            .and_then(LogLevel::parse);

        let text = |field| {
            doc.get_first(field)
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
        };

        let duration_ns = doc.get_first(f.duration_ns).and_then(|v| v.as_u64());

        let attributes = match doc.get_first(f.attributes) {
            Some(v) => match owned_value_to_json(v) {
                Value::Object(map) => map,
                other => {
                    warn!("unexpected attributes shape: {other}");
                    Map::new()
                }
            },
            None => Map::new(),
        };

        Some(Event {
            id,
            timestamp,
            event_type,
            level,
            message: text(f.message),
            message_template: text(f.message_template),
            service: text(f.service),
            environment: text(f.environment),
            trace_id: text(f.trace_id),
            span_id: text(f.span_id),
            parent_span_id: text(f.parent_span_id),
            duration_ns,
            stacktrace: text(f.stacktrace),
            attributes,
        })
    }

    #[allow(dead_code)]
    pub fn delete_by_id(&self, writer: &mut IndexWriter, id: &str) -> Result<(), WriterError> {
        let term = Term::from_field_text(self.schema.fields.id, id);
        writer.delete_term(term);
        Ok(())
    }
}

fn serde_json_to_owned_map(map: &Map<String, Value>) -> std::collections::BTreeMap<String, OwnedValue> {
    map.iter()
        .map(|(k, v)| (k.clone(), json_to_owned(v)))
        .collect()
}

fn json_to_owned(value: &Value) -> OwnedValue {
    match value {
        Value::Null => OwnedValue::Null,
        Value::Bool(b) => OwnedValue::Bool(*b),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                OwnedValue::I64(i)
            } else if let Some(u) = n.as_u64() {
                OwnedValue::U64(u)
            } else if let Some(f) = n.as_f64() {
                OwnedValue::F64(f)
            } else {
                OwnedValue::Str(n.to_string())
            }
        }
        Value::String(s) => OwnedValue::Str(s.clone()),
        Value::Array(arr) => OwnedValue::Array(arr.iter().map(json_to_owned).collect()),
        Value::Object(obj) => OwnedValue::Object(
            obj.iter()
                .map(|(k, v)| (k.clone(), json_to_owned(v)))
                .collect(),
        ),
    }
}

fn owned_value_to_json(value: &OwnedValue) -> Value {
    match value {
        OwnedValue::Null => Value::Null,
        OwnedValue::Str(s) => Value::String(s.clone()),
        OwnedValue::U64(n) => Value::Number((*n).into()),
        OwnedValue::I64(n) => Value::Number((*n).into()),
        OwnedValue::F64(n) => serde_json::Number::from_f64(*n)
            .map(Value::Number)
            .unwrap_or(Value::Null),
        OwnedValue::Bool(b) => Value::Bool(*b),
        OwnedValue::Date(dt) => Value::Number(dt.into_timestamp_secs().into()),
        OwnedValue::Facet(f) => Value::String(f.to_string()),
        OwnedValue::Bytes(b) => Value::String(format!("bytes:{}", b.len())),
        OwnedValue::Array(arr) => Value::Array(arr.iter().map(owned_value_to_json).collect()),
        OwnedValue::Object(obj) => {
            let mut map = Map::new();
            for (k, v) in obj {
                map.insert(k.clone(), owned_value_to_json(v));
            }
            Value::Object(map)
        }
        OwnedValue::IpAddr(ip) => Value::String(ip.to_string()),
        OwnedValue::PreTokStr(s) => Value::String(s.text.clone()),
    }
}
