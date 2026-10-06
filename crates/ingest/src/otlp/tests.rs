use super::proto::*;
use super::*;

const NOW: Timestamp = Timestamp(42);

fn kv(k: &str, v: any_value::Value) -> KeyValue {
    KeyValue { key: k.into(), value: Some(AnyValue { value: Some(v) }) }
}

fn s(v: &str) -> any_value::Value {
    any_value::Value::StringValue(v.into())
}

fn resource() -> Option<Resource> {
    Some(Resource {
        attributes: vec![
            kv("service.name", s("checkout")),
            kv("deployment.environment.name", s("production")),
            kv("host.name", s("web-1")),
        ],
    })
}

#[test]
fn protobuf_logs_map_to_events() {
    let req = ExportLogsServiceRequest {
        resource_logs: vec![ResourceLogs {
            resource: resource(),
            scope_logs: vec![ScopeLogs {
                scope: Some(InstrumentationScope { name: "Checkout.Api".into(), ..Default::default() }),
                log_records: vec![LogRecord {
                    time_unix_nano: 1_700_000_000_000_000_000,
                    observed_time_unix_nano: 1_700_000_000_100_000_000,
                    severity_number: 17,
                    severity_text: "Error".into(),
                    body: Some(AnyValue { value: Some(s("Payment failed")) }),
                    attributes: vec![
                        kv("customerId", any_value::Value::IntValue(481)),
                        kv("exception.type", s("TimeoutException")),
                        kv("exception.stacktrace", s("at Pay()")),
                        kv("{OriginalFormat}", s("Payment failed for {customerId}")),
                        kv(
                            "tags",
                            any_value::Value::ArrayValue(ArrayValue { values: vec![AnyValue { value: Some(s("a")) }] }),
                        ),
                        kv(
                            "http",
                            any_value::Value::KvlistValue(KeyValueList {
                                values: vec![kv("status", any_value::Value::IntValue(500))],
                            }),
                        ),
                    ],
                    trace_id: vec![0xab; 16],
                    span_id: vec![0xcd; 8],
                    ..Default::default()
                }],
            }],
        }],
    };
    let m = decode_logs(&req.encode_to_vec(), Encoding::Protobuf, NOW).unwrap();
    assert_eq!(m.rejected, 0);
    let e = &m.events[0];
    assert_eq!(e.timestamp, Timestamp(1_700_000_000_000_000_000));
    assert_eq!(e.observed_timestamp, Timestamp(1_700_000_000_100_000_000));
    assert_eq!(e.level(), Some(Level::Error));
    assert_eq!(e.message(), "Payment failed");
    assert_eq!(e.service.as_deref(), Some("checkout"));
    assert_eq!(e.environment.as_deref(), Some("production"));
    assert_eq!(e.trace_id.as_ref().unwrap().as_str(), "ab".repeat(16));
    assert_eq!(e.span_id.as_ref().unwrap().as_str(), "cd".repeat(8));
    assert_eq!(e.resource.get("host.name"), Some(&Value::from("web-1")));
    assert_eq!(e.attributes.get("customerId"), Some(&Value::Int(481)));
    assert_eq!(e.attributes.get("otel.scope.name"), Some(&Value::from("Checkout.Api")));
    assert_eq!(e.attributes.lookup_path("http.status"), Some(&Value::Int(500)));
    assert!(e.attributes.get("exception.type").is_none());
    let log = e.as_log().unwrap();
    assert_eq!(log.message_template.as_deref(), Some("Payment failed for {customerId}"));
    let x = log.exception.as_ref().unwrap();
    assert_eq!(x.kind.as_deref(), Some("TimeoutException"));
    assert_eq!(x.stack_trace.as_deref(), Some("at Pay()"));
}

#[test]
fn json_logs_follow_otlp_json_rules() {
    // Example shaped after opentelemetry-proto/examples/logs.json.
    let body = br#"{
      "resourceLogs": [{
        "resource": {"attributes": [{"key": "service.name", "value": {"stringValue": "my.service"}}]},
        "scopeLogs": [{
          "scope": {"name": "my.library", "version": "1.0.0"},
          "logRecords": [{
            "timeUnixNano": "1544712660300000000",
            "observedTimeUnixNano": "1544712660300000000",
            "severityNumber": 10,
            "severityText": "Information",
            "traceId": "5B8EFFF798038103D269B633813FC60C",
            "spanId": "EEE19B7EC3C1B174",
            "body": {"stringValue": "Example log record"},
            "attributes": [
              {"key": "string.attribute", "value": {"stringValue": "some string"}},
              {"key": "boolean.attribute", "value": {"boolValue": true}},
              {"key": "int.attribute", "value": {"intValue": "10"}},
              {"key": "double.attribute", "value": {"doubleValue": 637.704}},
              {"key": "array.attribute", "value": {"arrayValue": {"values": [{"stringValue": "many"}, {"stringValue": "values"}]}}},
              {"key": "map.attribute", "value": {"kvlistValue": {"values": [{"key": "some.map.key", "value": {"stringValue": "some value"}}]}}}
            ]
          }]
        }]
      }]
    }"#;
    let m = decode_logs(body, Encoding::Json, NOW).unwrap();
    let e = &m.events[0];
    assert_eq!(e.timestamp, Timestamp(1_544_712_660_300_000_000));
    assert_eq!(e.level(), Some(Level::Information));
    assert_eq!(e.message(), "Example log record");
    assert_eq!(e.service.as_deref(), Some("my.service"));
    assert_eq!(e.trace_id.as_ref().unwrap().as_str(), "5b8efff798038103d269b633813fc60c");
    assert_eq!(e.attributes.get("int.attribute"), Some(&Value::Int(10)));
    assert_eq!(e.attributes.get("double.attribute"), Some(&Value::Float(637.704)));
    assert_eq!(e.attributes.get("boolean.attribute"), Some(&Value::Bool(true)));
    assert_eq!(e.attributes.lookup_path("map.attribute.some.map.key"), Some(&Value::from("some value")));
}

#[test]
fn structured_body_and_missing_times() {
    let body = br#"{"resourceLogs":[{"scopeLogs":[{"logRecords":[{"body":{"kvlistValue":{"values":[{"key":"message","value":{"stringValue":"hi"}},{"key":"n","value":{"intValue":3}}]}},"severityText":"WARN"}]}]}]}"#;
    let m = decode_logs(body, Encoding::Json, NOW).unwrap();
    let e = &m.events[0];
    assert_eq!(e.message(), "hi");
    assert_eq!(e.timestamp, NOW);
    assert_eq!(e.level(), Some(Level::Warning));
    assert_eq!(e.attributes.lookup_path("body.n"), Some(&Value::Int(3)));
}

#[test]
fn spans_map_and_invalid_spans_are_rejected() {
    let req = ExportTraceServiceRequest {
        resource_spans: vec![ResourceSpans {
            resource: resource(),
            scope_spans: vec![ScopeSpans {
                scope: None,
                spans: vec![
                    Span {
                        trace_id: vec![1; 16],
                        span_id: vec![2; 8],
                        parent_span_id: vec![3; 8],
                        name: "GET /pay".into(),
                        kind: 2,
                        start_time_unix_nano: 1_000_000_000,
                        end_time_unix_nano: 1_250_000_000,
                        attributes: vec![kv("http.route", s("/pay"))],
                        events: vec![proto::SpanEvent {
                            time_unix_nano: 1_100_000_000,
                            name: "exception".into(),
                            attributes: vec![],
                        }],
                        links: vec![],
                        status: Some(Status { message: "boom".into(), code: 2 }),
                        ..Default::default()
                    },
                    Span { name: "no ids".into(), ..Default::default() },
                ],
            }],
        }],
    };
    let m = decode_traces(&req.encode_to_vec(), Encoding::Protobuf, NOW).unwrap();
    assert_eq!(m.events.len(), 1);
    assert_eq!(m.rejected, 1);
    let e = &m.events[0];
    let sp = e.as_span().unwrap();
    assert_eq!(e.timestamp, Timestamp(1_000_000_000));
    assert_eq!(sp.duration_nanos, 250_000_000);
    assert_eq!(sp.duration_ms(), 250.0);
    assert_eq!(sp.kind, SpanKind::Server);
    assert_eq!(sp.status.code, StatusCode::Error);
    assert_eq!(sp.status.message.as_deref(), Some("boom"));
    assert_eq!(sp.parent_span_id.as_ref().unwrap().as_str(), "0303030303030303");
    assert_eq!(sp.events[0].name, "exception");
    assert_eq!(e.service.as_deref(), Some("checkout"));
}

#[test]
fn json_spans_accept_enum_names() {
    let body = br#"{"resourceSpans":[{"scopeSpans":[{"spans":[{"traceId":"0102030405060708090a0b0c0d0e0f10","spanId":"0102030405060708","name":"op","kind":"SPAN_KIND_CLIENT","startTimeUnixNano":"10","endTimeUnixNano":"30","status":{"code":"STATUS_CODE_OK"}}]}]}]}"#;
    let m = decode_traces(body, Encoding::Json, NOW).unwrap();
    let sp = m.events[0].as_span().unwrap();
    assert_eq!(sp.kind, SpanKind::Client);
    assert_eq!(sp.status.code, StatusCode::Ok);
    assert_eq!(sp.duration_nanos, 20);
}

#[test]
fn metrics_each_point_is_an_event() {
    let req = ExportMetricsServiceRequest {
        resource_metrics: vec![ResourceMetrics {
            resource: resource(),
            scope_metrics: vec![ScopeMetrics {
                scope: None,
                metrics: vec![
                    Metric {
                        name: "http.requests".into(),
                        unit: "1".into(),
                        data: Some(metric::Data::Sum(Sum {
                            data_points: vec![
                                NumberDataPoint {
                                    time_unix_nano: 5,
                                    value: Some(number_data_point::Value::AsInt(7)),
                                    attributes: vec![kv("route", s("/a"))],
                                    ..Default::default()
                                },
                                NumberDataPoint {
                                    time_unix_nano: 6,
                                    value: Some(number_data_point::Value::AsDouble(1.5)),
                                    ..Default::default()
                                },
                            ],
                            aggregation_temporality: 1,
                            is_monotonic: true,
                        })),
                        ..Default::default()
                    },
                    Metric {
                        name: "latency".into(),
                        data: Some(metric::Data::Histogram(proto::Histogram {
                            data_points: vec![HistogramDataPoint {
                                time_unix_nano: 9,
                                count: 4,
                                sum: Some(10.0),
                                bucket_counts: vec![1, 3],
                                explicit_bounds: vec![5.0],
                                min: Some(1.0),
                                max: Some(4.0),
                                ..Default::default()
                            }],
                            aggregation_temporality: 2,
                        })),
                        ..Default::default()
                    },
                    Metric { name: "empty".into(), ..Default::default() },
                ],
            }],
        }],
    };
    let m = decode_metrics(&req.encode_to_vec(), Encoding::Protobuf, NOW).unwrap();
    assert_eq!(m.events.len(), 3);
    assert_eq!(m.rejected, 1);
    let first = m.events[0].as_metric().unwrap();
    assert_eq!(first.name, "http.requests");
    assert_eq!(first.kind, MetricKind::Sum);
    assert!(first.monotonic);
    assert_eq!(first.temporality, Temporality::Delta);
    assert_eq!(first.value, MetricValue::Number(7.0));
    assert_eq!(m.events[0].attributes.get("route"), Some(&Value::from("/a")));
    let h = m.events[2].as_metric().unwrap();
    match &h.value {
        MetricValue::Histogram(h) => {
            assert_eq!((h.count, h.sum, h.min, h.max), (4, Some(10.0), Some(1.0), Some(4.0)));
            assert_eq!(h.bucket_counts, vec![1, 3]);
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn json_metrics() {
    let body = br#"{"resourceMetrics":[{"scopeMetrics":[{"metrics":[
        {"name":"cpu","gauge":{"dataPoints":[{"timeUnixNano":"100","asDouble":0.5}]}},
        {"name":"lat","histogram":{"aggregationTemporality":2,"dataPoints":[{"timeUnixNano":"100","count":"3","sum":6,"bucketCounts":["1","2"],"explicitBounds":[1]}]}},
        {"name":"q","summary":{"dataPoints":[{"timeUnixNano":"100","count":"2","sum":4}]}}
    ]}]}]}"#;
    let m = decode_metrics(body, Encoding::Json, NOW).unwrap();
    assert_eq!(m.events.len(), 3);
    assert_eq!(m.events[0].as_metric().unwrap().value, MetricValue::Number(0.5));
    match &m.events[1].as_metric().unwrap().value {
        MetricValue::Histogram(h) => assert_eq!((h.count, h.bucket_counts.clone()), (3, vec![1, 2])),
        other => panic!("{other:?}"),
    }
}

#[test]
fn malformed_payloads_error() {
    assert!(matches!(decode_logs(b"\xff\xff\xff", Encoding::Protobuf, NOW), Err(IngestError::Malformed(_))));
    assert!(matches!(decode_logs(b"[]", Encoding::Json, NOW), Err(IngestError::Malformed(_))));
    assert!(matches!(
        decode_traces(br#"{"resourceSpans":[{"scopeSpans":[{"spans":[{"traceId":"zz"}]}]}]}"#, Encoding::Json, NOW),
        Err(IngestError::Malformed(_))
    ));
    assert!(matches!(Encoding::from_content_type(Some("text/plain")), Err(IngestError::UnsupportedContentType(_))));
    // An empty request is valid OTLP.
    assert!(decode_logs(b"", Encoding::Protobuf, NOW).unwrap().events.is_empty());
    assert!(decode_logs(b"{}", Encoding::Json, NOW).unwrap().events.is_empty());
}

#[test]
fn responses() {
    assert!(encode_response(Encoding::Protobuf, 0, None).is_empty());
    assert_eq!(encode_response(Encoding::Json, 0, None), b"{}");
    let r = encode_response(Encoding::Json, 2, Some("bad"));
    let v: serde_json::Value = serde_json::from_slice(&r).unwrap();
    assert_eq!(v["partialSuccess"]["rejected"], "2");
    let pb = PartialSuccess::decode(
        ExportLogsServiceResponse::decode(encode_response(Encoding::Protobuf, 2, Some("bad")).as_slice())
            .unwrap()
            .partial_success
            .unwrap()
            .encode_to_vec()
            .as_slice(),
    )
    .unwrap();
    assert_eq!(pb.rejected, 2);
}
