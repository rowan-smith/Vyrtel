//! OTLP/HTTP (logs, traces, metrics) → common telemetry model.
//!
//! Mapping rules (docs/api.md has the user-facing version):
//! * `service.name` / `deployment.environment[.name]` resource attributes
//!   populate `service` / `environment`; the resource is kept intact.
//! * Log severity number wins over severity text; body strings become the
//!   message, structured bodies are kept under the `body` property.
//! * `exception.*` log attributes become the structured exception.
//! * Every data point of a metric is one event.
//! * Unknown attributes are preserved as-is.
//!
//! Records that cannot be represented (e.g. spans without a trace id) are
//! counted as rejected and reported through OTLP partial success.

pub mod json;
pub mod proto;

use base64::Engine as _;
use prost::Message;
use telemetry::*;

use crate::IngestError;
use proto::any_value::Value as AV;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Encoding {
    Protobuf,
    Json,
}

impl Encoding {
    pub fn from_content_type(ct: Option<&str>) -> Result<Encoding, IngestError> {
        let mime = ct.unwrap_or("application/x-protobuf").split(';').next().unwrap_or("").trim().to_ascii_lowercase();
        match mime.as_str() {
            "application/x-protobuf" | "application/protobuf" | "application/octet-stream" => Ok(Encoding::Protobuf),
            "application/json" => Ok(Encoding::Json),
            other => Err(IngestError::UnsupportedContentType(other.to_string())),
        }
    }

    pub fn content_type(self) -> &'static str {
        match self {
            Encoding::Protobuf => "application/x-protobuf",
            Encoding::Json => "application/json",
        }
    }
}

#[derive(Debug, Default)]
pub struct Mapped {
    pub events: Vec<TelemetryEvent>,
    /// Records dropped because they could not be represented.
    pub rejected: i64,
    pub error: Option<String>,
}

impl Mapped {
    fn reject(&mut self, why: &str) {
        self.rejected += 1;
        if self.error.is_none() {
            self.error = Some(why.to_string());
        }
    }
}

/// Encode an OTLP export response (identical shape for all three signals).
pub fn encode_response(enc: Encoding, rejected: i64, error: Option<&str>) -> Vec<u8> {
    let partial = (rejected > 0 || error.is_some())
        .then(|| proto::PartialSuccess { rejected, error_message: error.unwrap_or_default().to_string() });
    match enc {
        Encoding::Protobuf => proto::ExportLogsServiceResponse { partial_success: partial }.encode_to_vec(),
        Encoding::Json => match partial {
            None => b"{}".to_vec(),
            Some(p) => serde_json::json!({
                "partialSuccess": {
                    "rejected": p.rejected.to_string(),
                    "errorMessage": p.error_message,
                }
            })
            .to_string()
            .into_bytes(),
        },
    }
}

fn malformed(e: impl std::fmt::Display) -> IngestError {
    IngestError::Malformed(format!("invalid OTLP payload: {e}"))
}

pub fn any_value(v: Option<&proto::AnyValue>) -> Value {
    match v.and_then(|v| v.value.as_ref()) {
        None => Value::Null,
        Some(AV::StringValue(s)) => Value::String(s.clone()),
        Some(AV::BoolValue(b)) => Value::Bool(*b),
        Some(AV::IntValue(i)) => Value::Int(*i),
        Some(AV::DoubleValue(d)) => Value::Float(*d),
        Some(AV::ArrayValue(a)) => Value::Array(a.values.iter().map(|x| any_value(Some(x))).collect()),
        Some(AV::KvlistValue(kv)) => Value::Object(fields(&kv.values)),
        Some(AV::BytesValue(b)) => Value::String(base64::engine::general_purpose::STANDARD.encode(b)),
    }
}

pub fn fields(kvs: &[proto::KeyValue]) -> Fields {
    let mut f = Fields::with_capacity(kvs.len());
    for kv in kvs {
        f.insert(kv.key.clone(), any_value(kv.value.as_ref()));
    }
    f
}

struct ResourceInfo {
    resource: Fields,
    service: Option<String>,
    environment: Option<String>,
}

fn resource_info(r: Option<&proto::Resource>) -> ResourceInfo {
    let resource = r.map(|r| fields(&r.attributes)).unwrap_or_default();
    let s = |k: &str| resource.get(k).and_then(Value::as_str).filter(|s| !s.is_empty()).map(String::from);
    ResourceInfo {
        service: s("service.name"),
        environment: s("deployment.environment.name").or_else(|| s("deployment.environment")),
        resource,
    }
}

fn add_scope(attrs: &mut Fields, scope: Option<&proto::InstrumentationScope>) {
    if let Some(sc) = scope
        && !sc.name.is_empty()
        && attrs.get("otel.scope.name").is_none()
    {
        attrs.insert("otel.scope.name", Value::String(sc.name.clone()));
    }
}

fn ts(nanos: u64) -> Option<Timestamp> {
    (nanos != 0).then(|| Timestamp(nanos.min(i64::MAX as u64) as i64))
}

fn take_str(f: &mut Fields, key: &str) -> Option<String> {
    match f.remove(key) {
        Some(Value::String(s)) => Some(s),
        Some(Value::Null) | None => None,
        Some(other) => Some(other.to_display_string()),
    }
}

pub fn decode_logs(body: &[u8], enc: Encoding, now: Timestamp) -> Result<Mapped, IngestError> {
    let req = match enc {
        Encoding::Protobuf => proto::ExportLogsServiceRequest::decode(body).map_err(malformed)?,
        Encoding::Json => json::logs(body).map_err(malformed)?,
    };
    let mut out = Mapped::default();
    for rl in &req.resource_logs {
        let res = resource_info(rl.resource.as_ref());
        for sl in &rl.scope_logs {
            for lr in &sl.log_records {
                let mut attributes = fields(&lr.attributes);
                add_scope(&mut attributes, sl.scope.as_ref());
                if !lr.event_name.is_empty() {
                    attributes.insert("event.name", Value::String(lr.event_name.clone()));
                }
                let level = Level::from_otlp_severity(lr.severity_number)
                    .or_else(|| Level::parse(&lr.severity_text))
                    .unwrap_or(Level::Information);
                if !lr.severity_text.is_empty() && Level::parse(&lr.severity_text) != Some(level) {
                    attributes.insert("severityText", Value::String(lr.severity_text.clone()));
                }
                let exception = Exception {
                    kind: take_str(&mut attributes, "exception.type"),
                    message: take_str(&mut attributes, "exception.message"),
                    stack_trace: take_str(&mut attributes, "exception.stacktrace"),
                };
                let template = take_str(&mut attributes, "{OriginalFormat}")
                    .or_else(|| take_str(&mut attributes, "message_template.text"));
                let message = match any_value(lr.body.as_ref()) {
                    Value::String(s) => s,
                    Value::Null => exception.message.clone().unwrap_or_default(),
                    Value::Object(o) => {
                        let m = o.get("message").map(Value::to_display_string);
                        let text = serde_json::to_string(&o).unwrap_or_default();
                        attributes.insert("body", Value::Object(o));
                        m.unwrap_or(text)
                    }
                    other => other.to_display_string(),
                };
                let timestamp = ts(lr.time_unix_nano).or_else(|| ts(lr.observed_time_unix_nano)).unwrap_or(now);
                out.events.push(TelemetryEvent {
                    id: EventId(0),
                    timestamp,
                    observed_timestamp: ts(lr.observed_time_unix_nano).unwrap_or(now),
                    service: res.service.clone(),
                    environment: res.environment.clone(),
                    trace_id: TraceId::from_bytes(&lr.trace_id),
                    span_id: SpanId::from_bytes(&lr.span_id),
                    resource: res.resource.clone(),
                    attributes,
                    payload: TelemetryPayload::Log(LogEvent {
                        level,
                        message,
                        message_template: template,
                        exception: (!exception.is_empty()).then_some(exception),
                    }),
                });
            }
        }
    }
    Ok(out)
}

pub fn decode_traces(body: &[u8], enc: Encoding, now: Timestamp) -> Result<Mapped, IngestError> {
    let req = match enc {
        Encoding::Protobuf => proto::ExportTraceServiceRequest::decode(body).map_err(malformed)?,
        Encoding::Json => json::traces(body).map_err(malformed)?,
    };
    let mut out = Mapped::default();
    for rs in &req.resource_spans {
        let res = resource_info(rs.resource.as_ref());
        for ss in &rs.scope_spans {
            for sp in &ss.spans {
                let (Some(trace_id), Some(span_id)) =
                    (TraceId::from_bytes(&sp.trace_id), SpanId::from_bytes(&sp.span_id))
                else {
                    out.reject("span without trace id or span id");
                    continue;
                };
                let mut attributes = fields(&sp.attributes);
                add_scope(&mut attributes, ss.scope.as_ref());
                let start = ts(sp.start_time_unix_nano).unwrap_or(now);
                let duration_nanos = sp.end_time_unix_nano.saturating_sub(sp.start_time_unix_nano);
                let status = sp.status.as_ref().map_or_else(SpanStatus::default, |s| SpanStatus {
                    code: StatusCode::from_code(s.code.clamp(0, 2) as u8),
                    message: (!s.message.is_empty()).then(|| s.message.clone()),
                });
                out.events.push(TelemetryEvent {
                    id: EventId(0),
                    timestamp: start,
                    observed_timestamp: now,
                    service: res.service.clone(),
                    environment: res.environment.clone(),
                    trace_id: Some(trace_id),
                    span_id: Some(span_id),
                    resource: res.resource.clone(),
                    attributes,
                    payload: TelemetryPayload::Span(SpanEvent {
                        parent_span_id: SpanId::from_bytes(&sp.parent_span_id),
                        name: sp.name.clone(),
                        kind: SpanKind::from_code(sp.kind.clamp(0, 5) as u8),
                        duration_nanos,
                        status,
                        events: sp
                            .events
                            .iter()
                            .map(|e| SpanEventItem {
                                timestamp: ts(e.time_unix_nano).unwrap_or(start),
                                name: e.name.clone(),
                                attributes: fields(&e.attributes),
                            })
                            .collect(),
                        links: sp
                            .links
                            .iter()
                            .filter_map(|l| {
                                Some(SpanLink {
                                    trace_id: TraceId::from_bytes(&l.trace_id)?,
                                    span_id: SpanId::from_bytes(&l.span_id)?,
                                    attributes: fields(&l.attributes),
                                })
                            })
                            .collect(),
                    }),
                });
            }
        }
    }
    Ok(out)
}

fn temporality(t: i32) -> Temporality {
    Temporality::from_code(t.clamp(0, 2) as u8)
}

pub fn decode_metrics(body: &[u8], enc: Encoding, now: Timestamp) -> Result<Mapped, IngestError> {
    use proto::metric::Data;
    let req = match enc {
        Encoding::Protobuf => proto::ExportMetricsServiceRequest::decode(body).map_err(malformed)?,
        Encoding::Json => json::metrics(body).map_err(malformed)?,
    };
    let mut out = Mapped::default();
    for rm in &req.resource_metrics {
        let res = resource_info(rm.resource.as_ref());
        for sm in &rm.scope_metrics {
            for m in &sm.metrics {
                if m.data.is_none() {
                    out.reject("metric without data");
                    continue;
                }
                let mut push = |time: u64,
                                attrs: &[proto::KeyValue],
                                kind: MetricKind,
                                temporality: Temporality,
                                monotonic: bool,
                                value: MetricValue| {
                    let mut attributes = fields(attrs);
                    add_scope(&mut attributes, sm.scope.as_ref());
                    out.events.push(TelemetryEvent {
                        id: EventId(0),
                        timestamp: ts(time).unwrap_or(now),
                        observed_timestamp: now,
                        service: res.service.clone(),
                        environment: res.environment.clone(),
                        trace_id: None,
                        span_id: None,
                        resource: res.resource.clone(),
                        attributes,
                        payload: TelemetryPayload::Metric(MetricEvent {
                            name: m.name.clone(),
                            description: (!m.description.is_empty()).then(|| m.description.clone()),
                            unit: (!m.unit.is_empty()).then(|| m.unit.clone()),
                            kind,
                            temporality,
                            monotonic,
                            value,
                        }),
                    });
                };
                let number = |p: &proto::NumberDataPoint| match p.value {
                    Some(proto::number_data_point::Value::AsDouble(d)) => d,
                    Some(proto::number_data_point::Value::AsInt(i)) => i as f64,
                    None => f64::NAN,
                };
                match &m.data {
                    Some(Data::Gauge(g)) => {
                        for p in &g.data_points {
                            push(
                                p.time_unix_nano,
                                &p.attributes,
                                MetricKind::Gauge,
                                Temporality::Unspecified,
                                false,
                                MetricValue::Number(number(p)),
                            );
                        }
                    }
                    Some(Data::Sum(s)) => {
                        for p in &s.data_points {
                            push(
                                p.time_unix_nano,
                                &p.attributes,
                                MetricKind::Sum,
                                temporality(s.aggregation_temporality),
                                s.is_monotonic,
                                MetricValue::Number(number(p)),
                            );
                        }
                    }
                    Some(Data::Histogram(h)) => {
                        for p in &h.data_points {
                            push(
                                p.time_unix_nano,
                                &p.attributes,
                                MetricKind::Histogram,
                                temporality(h.aggregation_temporality),
                                false,
                                MetricValue::Histogram(Histogram {
                                    count: p.count,
                                    sum: p.sum,
                                    min: p.min,
                                    max: p.max,
                                    bucket_counts: p.bucket_counts.clone(),
                                    explicit_bounds: p.explicit_bounds.clone(),
                                }),
                            );
                        }
                    }
                    // Exponential histograms and summaries keep count/sum
                    // (and min/max where present); bucket detail is dropped.
                    Some(Data::ExponentialHistogram(h)) => {
                        for p in &h.data_points {
                            push(
                                p.time_unix_nano,
                                &p.attributes,
                                MetricKind::Histogram,
                                temporality(h.aggregation_temporality),
                                false,
                                MetricValue::Histogram(Histogram {
                                    count: p.count,
                                    sum: p.sum,
                                    min: p.min,
                                    max: p.max,
                                    ..Default::default()
                                }),
                            );
                        }
                    }
                    Some(Data::Summary(s)) => {
                        for p in &s.data_points {
                            push(
                                p.time_unix_nano,
                                &p.attributes,
                                MetricKind::Histogram,
                                Temporality::Cumulative,
                                false,
                                MetricValue::Histogram(Histogram {
                                    count: p.count,
                                    sum: Some(p.sum),
                                    ..Default::default()
                                }),
                            );
                        }
                    }
                    None => {}
                }
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests;
