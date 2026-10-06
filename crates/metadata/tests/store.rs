use metadata::*;
use serde_json::json;

fn alert() -> AlertInput {
    AlertInput {
        name: "Payment error spike".into(),
        query: r#"level = "Error" and service = "payments""#.into(),
        op: ">".into(),
        threshold: 100.0,
        window_secs: 300,
        interval_secs: 60,
        webhook_url: Some("http://localhost:9999/hook".into()),
        enabled: true,
    }
}

#[test]
fn migrations_are_idempotent_and_persistent() {
    let d = tempfile::tempdir().unwrap();
    let path = d.path().join("metadata").join("metadata.db");
    {
        let m = Metadata::open(&path).unwrap();
        assert_eq!(m.schema_version().unwrap(), 1);
        m.set_setting("a", "1").unwrap();
        m.create_dashboard("Prod", None).unwrap();
    }
    let m = Metadata::open(&path).unwrap();
    assert_eq!(m.schema_version().unwrap(), 1);
    assert_eq!(m.get_setting("a").unwrap().as_deref(), Some("1"));
    assert_eq!(m.list_dashboards().unwrap().len(), 1);
}

#[test]
fn settings_upsert() {
    let m = Metadata::open_in_memory().unwrap();
    assert_eq!(m.get_setting("x").unwrap(), None);
    m.set_setting("x", "1").unwrap();
    m.set_setting("x", "2").unwrap();
    assert_eq!(m.get_setting("x").unwrap().as_deref(), Some("2"));
}

#[test]
fn dashboard_and_panel_lifecycle() {
    let m = Metadata::open_in_memory().unwrap();
    let d = m.create_dashboard("Payments", Some("Main board")).unwrap();
    assert!(d.panels.is_empty());
    let p1 = m
        .add_panel(
            d.id,
            &PanelInput {
                title: "Errors".into(),
                kind: PanelKind::LogCount,
                config: json!({"query": "level = Error"}),
                position: None,
                width: None,
                height: None,
            },
        )
        .unwrap();
    let p2 = m
        .add_panel(
            d.id,
            &PanelInput {
                title: "Latency".into(),
                kind: PanelKind::MetricLine,
                config: json!({"metric": "http.duration", "agg": "avg"}),
                position: None,
                width: Some(99),
                height: None,
            },
        )
        .unwrap();
    assert_eq!((p1.position, p2.position), (0, 1));
    assert_eq!(p2.width, 12, "width clamped to grid");

    let renamed = m.update_dashboard(d.id, "Payments (prod)", None).unwrap();
    assert_eq!(renamed.name, "Payments (prod)");
    assert_eq!(renamed.panels.len(), 2);

    let updated = m
        .update_panel(
            d.id,
            p1.id,
            &PanelInput {
                title: "Errors 5m".into(),
                kind: PanelKind::SingleStat,
                config: json!({"query": "level = Error", "window": "5m"}),
                position: Some(5),
                width: None,
                height: None,
            },
        )
        .unwrap();
    assert_eq!(updated.kind, PanelKind::SingleStat);
    assert_eq!(updated.config["window"], "5m");
    assert_eq!(m.get_dashboard(d.id).unwrap().panels[1].id, p1.id, "ordered by position");

    m.delete_panel(d.id, p2.id).unwrap();
    assert!(matches!(m.delete_panel(d.id, p2.id), Err(MetadataError::NotFound)));
    m.delete_dashboard(d.id).unwrap();
    assert!(matches!(m.get_dashboard(d.id), Err(MetadataError::NotFound)));
    // Panels cascade with the dashboard (foreign keys are enforced).
    assert!(matches!(m.delete_panel(d.id, p1.id), Err(MetadataError::NotFound)));
}

#[test]
fn panel_on_missing_dashboard_is_not_found() {
    let m = Metadata::open_in_memory().unwrap();
    let r = m.add_panel(
        42,
        &PanelInput {
            title: "x".into(),
            kind: PanelKind::LogCount,
            config: json!({}),
            position: None,
            width: None,
            height: None,
        },
    );
    assert!(matches!(r, Err(MetadataError::NotFound)));
}

#[test]
fn validation() {
    let m = Metadata::open_in_memory().unwrap();
    assert!(matches!(m.create_dashboard("  ", None), Err(MetadataError::Invalid(_))));
    let mut a = alert();
    a.op = "~".into();
    assert!(matches!(m.create_alert(&a), Err(MetadataError::Invalid(_))));
    let mut a = alert();
    a.interval_secs = 1;
    assert!(matches!(m.create_alert(&a), Err(MetadataError::Invalid(_))));
    let mut a = alert();
    a.webhook_url = Some("ftp://x".into());
    assert!(matches!(m.create_alert(&a), Err(MetadataError::Invalid(_))));
    assert!(matches!(m.create_saved_query("q", "bogus", ""), Err(MetadataError::Invalid(_))));
    assert!(matches!(m.create_api_key("k", "p", "h", "root"), Err(MetadataError::Invalid(_))));
}

#[test]
fn alert_state_transitions_and_history() {
    let m = Metadata::open_in_memory().unwrap();
    let a = m.create_alert(&alert()).unwrap();
    assert_eq!(a.state.status, AlertStatus::Ok);

    let prev = m.record_alert_evaluation(a.id, AlertStatus::Ok, Some(3.0), None).unwrap();
    assert_eq!(prev, AlertStatus::Ok);
    let prev = m.record_alert_evaluation(a.id, AlertStatus::Firing, Some(150.0), None).unwrap();
    assert_eq!(prev, AlertStatus::Ok);
    let prev = m.record_alert_evaluation(a.id, AlertStatus::Error, None, Some("bad query")).unwrap();
    assert_eq!(prev, AlertStatus::Firing);

    let got = m.get_alert(a.id).unwrap();
    assert_eq!(got.state.status, AlertStatus::Error);
    assert_eq!(got.state.last_error.as_deref(), Some("bad query"));
    assert!(got.state.last_transition_at.is_some());

    let h = m.alert_history(a.id, 10).unwrap();
    assert_eq!(h.len(), 2, "only real transitions are recorded");
    assert_eq!((h[0].from.as_str(), h[0].to.as_str()), ("FIRING", "ERROR"));

    m.record_alert_notification(a.id, Some("timeout")).unwrap();
    assert_eq!(m.get_alert(a.id).unwrap().state.last_notification_error.as_deref(), Some("timeout"));

    let mut changed = alert();
    changed.threshold = 5.0;
    changed.enabled = false;
    let u = m.update_alert(a.id, &changed).unwrap();
    assert_eq!(u.def.threshold, 5.0);
    assert!(!u.def.enabled);
    assert_eq!(m.list_alerts().unwrap().len(), 1);
    m.delete_alert(a.id).unwrap();
    assert!(m.alert_history(a.id, 10).unwrap().is_empty());
}

#[test]
fn api_keys_store_hashes_only() {
    let m = Metadata::open_in_memory().unwrap();
    let k = m.create_api_key("ci", "obs_abcd", "hash-1", "ingest").unwrap();
    assert_eq!(m.find_api_key("hash-1").unwrap(), Some((k.id, "ingest".to_string())));
    assert_eq!(m.find_api_key("nope").unwrap(), None);
    m.touch_api_key(k.id).unwrap();
    assert!(m.list_api_keys().unwrap()[0].last_used_at.is_some());
    m.revoke_api_key(k.id).unwrap();
    assert_eq!(m.find_api_key("hash-1").unwrap(), None);
    assert_eq!(m.count_api_keys().unwrap(), 0);
    assert!(matches!(m.revoke_api_key(k.id), Err(MetadataError::NotFound)));
}

#[test]
fn users_and_sessions() {
    let m = Metadata::open_in_memory().unwrap();
    let uid = m.upsert_user("admin", "hash").unwrap();
    assert_eq!(m.upsert_user("admin", "hash2").unwrap(), uid);
    assert_eq!(m.find_user("admin").unwrap().unwrap().password_hash, "hash2");
    m.create_session("s1", uid, 60_000).unwrap();
    m.create_session("s2", uid, -1).unwrap();
    assert_eq!(m.find_session("s1").unwrap().unwrap().username, "admin");
    assert!(m.find_session("s2").unwrap().is_none(), "expired");
    assert_eq!(m.purge_expired_sessions().unwrap(), 1);
    m.delete_session("s1").unwrap();
    assert!(m.find_session("s1").unwrap().is_none());
}

#[test]
fn query_stats_accumulate() {
    let m = Metadata::open_in_memory().unwrap();
    let row = StoredQueryStats {
        signal: "logs".into(),
        field: "customerId".into(),
        index: "bloom".into(),
        queries: 2,
        bytes_read: 100,
        segments_scanned: 3,
        segments_skipped: 7,
        total_ms: 4.0,
        last_seen: 1,
    };
    m.add_query_stats(std::slice::from_ref(&row)).unwrap();
    m.add_query_stats(&[row]).unwrap();
    let s = m.query_stats().unwrap();
    assert_eq!(s.len(), 1);
    assert_eq!(s[0].queries, 4);
    assert_eq!(s[0].segments_skipped, 14);
    assert_eq!(s[0].total_ms, 8.0);
}

#[test]
fn saved_queries() {
    let m = Metadata::open_in_memory().unwrap();
    let q = m.create_saved_query("Errors", "logs", "level = Error").unwrap();
    assert_eq!(m.list_saved_queries().unwrap(), vec![q.clone()]);
    m.delete_saved_query(q.id).unwrap();
    assert!(m.list_saved_queries().unwrap().is_empty());
}
