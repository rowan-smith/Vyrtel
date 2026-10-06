use chrono::{Duration, Utc};
use event::{Event, EventType, LogLevel};
use serde_json::json;
use uuid::Uuid;

use crate::state::AppState;

pub async fn seed_sample_data(state: &AppState) -> anyhow::Result<()> {
    let now = Utc::now();
    let mut events = Vec::new();

    let services = ["api", "billing", "database", "worker"];
    let environments = ["production", "staging"];

    // Shared traces
    let traces = [
        ("trace-orders-001", "api", "GET /orders/42", 487_000_000u64),
        ("trace-pay-002", "billing", "POST /payments", 312_000_000u64),
        ("trace-job-003", "worker", "process.batch", 1_200_000_000u64),
    ];

    for (i, (trace_id, root_service, root_op, root_dur)) in traces.iter().enumerate() {
        let root_span = Uuid::new_v4().to_string()[..16].to_string();
        let child1 = Uuid::new_v4().to_string()[..16].to_string();
        let child2 = Uuid::new_v4().to_string()[..16].to_string();
        let base = now - Duration::minutes(5 + i as i64 * 3);

        events.push(span(
            base,
            root_service,
            root_op,
            trace_id,
            &root_span,
            None,
            *root_dur,
        ));
        events.push(span(
            base + Duration::milliseconds(5),
            "api",
            "authenticate",
            trace_id,
            &child1,
            Some(&root_span),
            12_000_000,
        ));
        events.push(span(
            base + Duration::milliseconds(20),
            "database",
            "database.select_orders",
            trace_id,
            &child2,
            Some(&root_span),
            83_000_000,
        ));

        if *root_service == "billing" || i == 0 {
            let child3 = Uuid::new_v4().to_string()[..16].to_string();
            let child4 = Uuid::new_v4().to_string()[..16].to_string();
            events.push(span(
                base + Duration::milliseconds(100),
                "billing",
                "billing.request",
                trace_id,
                &child3,
                Some(&root_span),
                301_000_000,
            ));
            events.push(span(
                base + Duration::milliseconds(120),
                "billing",
                "stripe.request",
                trace_id,
                &child4,
                Some(&child3),
                287_000_000,
            ));
        }

        // Related logs
        events.push(log_event(
            base + Duration::milliseconds(25),
            LogLevel::Information,
            "api",
            "production",
            "Request started",
            Some(trace_id),
            Some(&root_span),
            json!({ "http.method": "GET", "http.route": "/orders/{id}" }),
        ));
        events.push(log_event(
            base + Duration::milliseconds(400),
            if i == 1 {
                LogLevel::Error
            } else {
                LogLevel::Information
            },
            "billing",
            "production",
            if i == 1 {
                "Payment failed for customer 42"
            } else {
                "Payment authorized"
            },
            Some(trace_id),
            None,
            json!({ "customerId": 42, "provider": "stripe", "attempt": 3 }),
        ));
        // Attach stacktrace on the payment failure log
        if let Some(last) = events.last_mut() {
            if last.level == Some(LogLevel::Error) {
                last.stacktrace = Some(sample_stacktrace("billing", "PaymentException"));
            }
        }
    }

    // Assorted logs across services
    let samples = [
        (LogLevel::Information, "api", "POST /orders completed in 82 ms", json!({"http.status_code": 200, "duration_ms": 82}), None),
        (LogLevel::Warning, "database", "Query exceeded expected duration", json!({"query": "SELECT * FROM orders", "duration_ms": 420}), None),
        (LogLevel::Error, "billing", "Payment failed for customer 42", json!({"customerId": 42, "provider": "stripe", "attempt": 3}), Some(sample_stacktrace("billing", "PaymentException"))),
        (LogLevel::Debug, "worker", "Dequeued job batch", json!({"batchSize": 50}), None),
        (LogLevel::Trace, "api", "Auth token validated", json!({"userId": "u-100"}), None),
        (LogLevel::Fatal, "worker", "Unhandled panic in job runner", json!({"job": "reconcile"}), Some(sample_stacktrace("worker", "panic"))),
        (LogLevel::Information, "api", "Health check ok", json!({"success": true}), None),
        (LogLevel::Warning, "api", "Rate limit approaching", json!({"clientIp": "10.0.0.8", "remaining": 12}), None),
        (LogLevel::Error, "database", "Connection pool exhausted", json!({"pool": "primary", "waiting": 17}), Some(sample_stacktrace("database", "PoolExhaustedException"))),
        (LogLevel::Information, "billing", "Refund issued", json!({"customerId": 99, "amount": 19.99}), None),
    ];

    for (i, (level, service, message, attrs, stacktrace)) in samples.iter().enumerate() {
        let env = environments[i % environments.len()];
        let mut event = log_event(
            now - Duration::seconds(30 + i as i64 * 17),
            *level,
            service,
            env,
            message,
            None,
            None,
            attrs.clone(),
        );
        event.stacktrace = stacktrace.clone();
        events.push(event);
    }

    // More volume for dashboards
    for i in 0..80 {
        let service = services[i % services.len()];
        let level = match i % 7 {
            0 => LogLevel::Error,
            1 => LogLevel::Warning,
            2 => LogLevel::Debug,
            _ => LogLevel::Information,
        };
        let mut event = log_event(
            now - Duration::minutes((i % 50) as i64) - Duration::seconds(i as i64),
            level,
            service,
            "production",
            &format!("Synthetic event {i}"),
            None,
            None,
            json!({ "index": i, "machine.name": format!("crusher-{:02}", (i % 4) + 1) }),
        );
        if matches!(level, LogLevel::Error | LogLevel::Fatal) {
            event.stacktrace = Some(sample_stacktrace(service, "SyntheticError"));
        }
        events.push(event);
    }

    // Metrics time series
    let metric_names = [
        ("http.server.request_duration_ms", "ms", "api"),
        ("http.server.requests", "1", "api"),
        ("orders.created", "1", "api"),
        ("payments.failed", "1", "billing"),
        ("db.query.duration_ms", "ms", "database"),
        ("worker.queue.depth", "1", "worker"),
        ("process.cpu.utilization", "1", "api"),
        ("process.memory.working_set_bytes", "By", "api"),
    ];
    for (mi, (name, unit, service)) in metric_names.iter().enumerate() {
        let mi = mi as i64;
        for step in 0..36i64 {
            let ts = now - Duration::minutes(step * 5);
            let base = match *name {
                "http.server.request_duration_ms" => 40.0 + ((step + mi) % 7) as f64 * 12.0,
                "http.server.requests" => 80.0 + ((step * 3 + mi) % 40) as f64,
                "orders.created" => 5.0 + ((step + mi) % 8) as f64,
                "payments.failed" => (if step % 5 == 0 { 3.0 } else { 0.0 }) + (step % 2) as f64,
                "db.query.duration_ms" => 15.0 + ((step * 2) % 9) as f64 * 8.0,
                "worker.queue.depth" => 10.0 + ((step + 3) % 20) as f64,
                "process.cpu.utilization" => 0.15 + ((step % 10) as f64) * 0.04,
                "process.memory.working_set_bytes" => 180_000_000.0 + step as f64 * 250_000.0,
                _ => step as f64,
            };
            events.push(metric_event(
                ts,
                name,
                base,
                unit,
                service,
                "production",
                json!({ "host": format!("crusher-{:02}", (mi % 4) + 1) }),
            ));
        }
    }

    state.ingest.try_enqueue_batch(events)?;

    // Alert rules (only if none exist yet)
    if state.metadata.count_alert_rules().await? == 0 {
        let _ = state
            .metadata
            .create_alert_rule(
                "High error rate",
                "level = error | count",
                metadata::AlertOperator::Gte,
                5.0,
            )
            .await?;
        let _ = state
            .metadata
            .create_alert_rule(
                "Payment failures",
                r#"service = "billing" and level = error | count"#,
                metadata::AlertOperator::Gt,
                0.0,
            )
            .await?;
        let _ = state
            .metadata
            .create_alert_rule(
                "Fatal events",
                "level = fatal | count",
                metadata::AlertOperator::Gte,
                1.0,
            )
            .await?;
        // Run one evaluation immediately so seed shows firing state
        let _ = crate::api::evaluate_alerts(state).await;
    }

    seed_sample_dashboards(state).await?;

    Ok(())
}

async fn seed_sample_dashboards(state: &AppState) -> anyhow::Result<()> {
    if state.metadata.count_dashboards().await? > 0 {
        return Ok(());
    }

    use metadata::{Visualization, WidgetPosition};

    // Overview — volume + errors at a glance
    let overview = state.metadata.create_dashboard("Overview").await?;
    state
        .metadata
        .create_widget(
            &overview.id,
            "Events",
            " | count",
            Visualization::Number,
            WidgetPosition {
                x: 0,
                y: 0,
                w: 1,
                h: 1,
            },
        )
        .await?;
    state
        .metadata
        .create_widget(
            &overview.id,
            "Errors",
            "level = error | count",
            Visualization::Number,
            WidgetPosition {
                x: 1,
                y: 0,
                w: 1,
                h: 1,
            },
        )
        .await?;
    state
        .metadata
        .create_widget(
            &overview.id,
            "Warnings",
            "level = warning | count",
            Visualization::Number,
            WidgetPosition {
                x: 2,
                y: 0,
                w: 1,
                h: 1,
            },
        )
        .await?;
    state
        .metadata
        .create_widget(
            &overview.id,
            "Event volume",
            " | count by time(5m)",
            Visualization::Line,
            WidgetPosition {
                x: 0,
                y: 1,
                w: 2,
                h: 1,
            },
        )
        .await?;
    state
        .metadata
        .create_widget(
            &overview.id,
            "By service",
            " | count by service",
            Visualization::Bar,
            WidgetPosition {
                x: 2,
                y: 1,
                w: 1,
                h: 1,
            },
        )
        .await?;

    // Errors — focus on failures
    let errors = state.metadata.create_dashboard("Errors").await?;
    state
        .metadata
        .create_widget(
            &errors.id,
            "Error count",
            "level = error | count",
            Visualization::Number,
            WidgetPosition {
                x: 0,
                y: 0,
                w: 1,
                h: 1,
            },
        )
        .await?;
    state
        .metadata
        .create_widget(
            &errors.id,
            "Fatal count",
            "level = fatal | count",
            Visualization::Number,
            WidgetPosition {
                x: 1,
                y: 0,
                w: 1,
                h: 1,
            },
        )
        .await?;
    state
        .metadata
        .create_widget(
            &errors.id,
            "Billing errors",
            r#"service = "billing" and level = error | count"#,
            Visualization::Number,
            WidgetPosition {
                x: 2,
                y: 0,
                w: 1,
                h: 1,
            },
        )
        .await?;
    state
        .metadata
        .create_widget(
            &errors.id,
            "Errors over time",
            "level = error | count by time(5m)",
            Visualization::Line,
            WidgetPosition {
                x: 0,
                y: 1,
                w: 2,
                h: 1,
            },
        )
        .await?;
    state
        .metadata
        .create_widget(
            &errors.id,
            "Errors by service",
            "level = error | count by service",
            Visualization::Bar,
            WidgetPosition {
                x: 2,
                y: 1,
                w: 1,
                h: 1,
            },
        )
        .await?;

    // Services — per-service health
    let services = state.metadata.create_dashboard("Services").await?;
    state
        .metadata
        .create_widget(
            &services.id,
            "API events",
            r#"service = "api" | count"#,
            Visualization::Number,
            WidgetPosition {
                x: 0,
                y: 0,
                w: 1,
                h: 1,
            },
        )
        .await?;
    state
        .metadata
        .create_widget(
            &services.id,
            "Billing events",
            r#"service = "billing" | count"#,
            Visualization::Number,
            WidgetPosition {
                x: 1,
                y: 0,
                w: 1,
                h: 1,
            },
        )
        .await?;
    state
        .metadata
        .create_widget(
            &services.id,
            "Worker events",
            r#"service = "worker" | count"#,
            Visualization::Number,
            WidgetPosition {
                x: 2,
                y: 0,
                w: 1,
                h: 1,
            },
        )
        .await?;
    state
        .metadata
        .create_widget(
            &services.id,
            "API volume",
            r#"service = "api" | count by time(5m)"#,
            Visualization::Line,
            WidgetPosition {
                x: 0,
                y: 1,
                w: 2,
                h: 1,
            },
        )
        .await?;
    state
        .metadata
        .create_widget(
            &services.id,
            "Events by environment",
            " | count by environment",
            Visualization::Bar,
            WidgetPosition {
                x: 2,
                y: 1,
                w: 1,
                h: 1,
            },
        )
        .await?;

    Ok(())
}

fn metric_event(
    ts: chrono::DateTime<Utc>,
    name: &str,
    value: f64,
    unit: &str,
    service: &str,
    environment: &str,
    attributes: serde_json::Value,
) -> Event {
    let mut attrs = match attributes {
        serde_json::Value::Object(m) => m,
        _ => serde_json::Map::new(),
    };
    attrs.insert("value".into(), json!(value));
    attrs.insert("unit".into(), json!(unit));
    Event {
        id: Uuid::new_v4(),
        timestamp: ts,
        event_type: EventType::Metric,
        level: None,
        message: Some(name.to_string()),
        message_template: None,
        service: Some(service.to_string()),
        environment: Some(environment.to_string()),
        trace_id: None,
        span_id: None,
        parent_span_id: None,
        duration_ns: None,
        stacktrace: None,
        attributes: attrs,
    }
}

fn sample_stacktrace(service: &str, exception: &str) -> String {
    format!(
        "{exception}: operation failed\n  at {service}.handlers.process (src/{service}/handlers.rs:142:18)\n  at {service}.pipeline.run (src/{service}/pipeline.rs:87:9)\n  at {service}.main (src/{service}/main.rs:24:5)"
    )
}

fn log_event(
    ts: chrono::DateTime<Utc>,
    level: LogLevel,
    service: &str,
    environment: &str,
    message: &str,
    trace_id: Option<&str>,
    span_id: Option<&str>,
    attributes: serde_json::Value,
) -> Event {
    let attrs = match attributes {
        serde_json::Value::Object(m) => m,
        _ => serde_json::Map::new(),
    };
    Event {
        id: Uuid::new_v4(),
        timestamp: ts,
        event_type: EventType::Log,
        level: Some(level),
        message: Some(message.to_string()),
        message_template: None,
        service: Some(service.to_string()),
        environment: Some(environment.to_string()),
        trace_id: trace_id.map(|s| s.to_string()),
        span_id: span_id.map(|s| s.to_string()),
        parent_span_id: None,
        duration_ns: None,
        stacktrace: None,
        attributes: attrs,
    }
}

fn span(
    ts: chrono::DateTime<Utc>,
    service: &str,
    name: &str,
    trace_id: &str,
    span_id: &str,
    parent: Option<&str>,
    duration_ns: u64,
) -> Event {
    Event {
        id: Uuid::new_v4(),
        timestamp: ts,
        event_type: EventType::Span,
        level: None,
        message: Some(name.to_string()),
        message_template: None,
        service: Some(service.to_string()),
        environment: Some("production".into()),
        trace_id: Some(trace_id.to_string()),
        span_id: Some(span_id.to_string()),
        parent_span_id: parent.map(|s| s.to_string()),
        duration_ns: Some(duration_ns),
        stacktrace: None,
        attributes: serde_json::Map::new(),
    }
}
