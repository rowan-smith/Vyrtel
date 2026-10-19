//! Heap held by decoded OTLP structures, by capacity. The OTLP/JSON
//! decoder charges each structure as it builds it; mapping charges a
//! record's size before turning it into an event; and tests hold the
//! protobuf pre-scan ([`super::scan`]) to these figures.

use super::proto::*;
use any_value::Value as AV;

fn vec<T>(v: &Vec<T>) -> usize {
    v.capacity() * size_of::<T>()
}

pub fn any_value(v: &AnyValue) -> usize {
    match &v.value {
        Some(AV::StringValue(s)) => s.capacity(),
        Some(AV::ArrayValue(a)) => vec(&a.values) + a.values.iter().map(any_value).sum::<usize>(),
        Some(AV::KvlistValue(l)) => key_values(&l.values),
        Some(AV::BytesValue(b)) => b.capacity(),
        Some(AV::BoolValue(_) | AV::IntValue(_) | AV::DoubleValue(_)) | None => 0,
    }
}

pub fn key_value(kv: &KeyValue) -> usize {
    kv.key.capacity() + kv.value.as_ref().map_or(0, any_value)
}

pub fn key_values(kvs: &Vec<KeyValue>) -> usize {
    vec(kvs) + kvs.iter().map(key_value).sum::<usize>()
}

pub fn resource(r: &Option<Resource>) -> usize {
    r.as_ref().map_or(0, |r| key_values(&r.attributes))
}

pub fn scope(s: &Option<InstrumentationScope>) -> usize {
    s.as_ref().map_or(0, |s| s.name.capacity() + s.version.capacity() + key_values(&s.attributes))
}

pub fn log_record(r: &LogRecord) -> usize {
    r.severity_text.capacity()
        + r.body.as_ref().map_or(0, any_value)
        + key_values(&r.attributes)
        + r.trace_id.capacity()
        + r.span_id.capacity()
        + r.event_name.capacity()
}

pub fn span_event(e: &SpanEvent) -> usize {
    e.name.capacity() + key_values(&e.attributes)
}

pub fn span_link(l: &SpanLink) -> usize {
    l.trace_id.capacity() + l.span_id.capacity() + key_values(&l.attributes)
}

pub fn span(s: &Span) -> usize {
    s.trace_id.capacity()
        + s.span_id.capacity()
        + s.trace_state.capacity()
        + s.parent_span_id.capacity()
        + s.name.capacity()
        + key_values(&s.attributes)
        + vec(&s.events)
        + s.events.iter().map(span_event).sum::<usize>()
        + vec(&s.links)
        + s.links.iter().map(span_link).sum::<usize>()
        + s.status.as_ref().map_or(0, |st| st.message.capacity())
}

pub fn number_point(p: &NumberDataPoint) -> usize {
    key_values(&p.attributes)
}

pub fn histogram_point(p: &HistogramDataPoint) -> usize {
    key_values(&p.attributes) + vec(&p.bucket_counts) + vec(&p.explicit_bounds)
}

pub fn exponential_point(p: &ExponentialHistogramDataPoint) -> usize {
    key_values(&p.attributes)
}

pub fn summary_point(p: &SummaryDataPoint) -> usize {
    key_values(&p.attributes)
}

/// A metric without its data points' own heap, which is charged point by
/// point.
pub fn metric_head(m: &Metric) -> usize {
    m.name.capacity() + m.description.capacity() + m.unit.capacity()
}

pub fn metric(m: &Metric) -> usize {
    use metric::Data;
    metric_head(m)
        + match &m.data {
            Some(Data::Gauge(g)) => vec(&g.data_points) + g.data_points.iter().map(number_point).sum::<usize>(),
            Some(Data::Sum(s)) => vec(&s.data_points) + s.data_points.iter().map(number_point).sum::<usize>(),
            Some(Data::Histogram(h)) => vec(&h.data_points) + h.data_points.iter().map(histogram_point).sum::<usize>(),
            Some(Data::ExponentialHistogram(h)) => {
                vec(&h.data_points) + h.data_points.iter().map(exponential_point).sum::<usize>()
            }
            Some(Data::Summary(s)) => vec(&s.data_points) + s.data_points.iter().map(summary_point).sum::<usize>(),
            None => 0,
        }
}

pub fn logs(r: &ExportLogsServiceRequest) -> usize {
    vec(&r.resource_logs)
        + r.resource_logs
            .iter()
            .map(|rl| {
                resource(&rl.resource)
                    + vec(&rl.scope_logs)
                    + rl.scope_logs
                        .iter()
                        .map(|sl| {
                            scope(&sl.scope)
                                + vec(&sl.log_records)
                                + sl.log_records.iter().map(log_record).sum::<usize>()
                        })
                        .sum::<usize>()
            })
            .sum::<usize>()
}

pub fn traces(r: &ExportTraceServiceRequest) -> usize {
    vec(&r.resource_spans)
        + r.resource_spans
            .iter()
            .map(|rs| {
                resource(&rs.resource)
                    + vec(&rs.scope_spans)
                    + rs.scope_spans
                        .iter()
                        .map(|ss| scope(&ss.scope) + vec(&ss.spans) + ss.spans.iter().map(span).sum::<usize>())
                        .sum::<usize>()
            })
            .sum::<usize>()
}

pub fn metrics(r: &ExportMetricsServiceRequest) -> usize {
    vec(&r.resource_metrics)
        + r.resource_metrics
            .iter()
            .map(|rm| {
                resource(&rm.resource)
                    + vec(&rm.scope_metrics)
                    + rm.scope_metrics
                        .iter()
                        .map(|sm| scope(&sm.scope) + vec(&sm.metrics) + sm.metrics.iter().map(metric).sum::<usize>())
                        .sum::<usize>()
            })
            .sum::<usize>()
}
