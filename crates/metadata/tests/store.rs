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
fn reopening_keeps_everything() {
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

/// Every file in `dir` with its bytes.
fn files(dir: &std::path::Path) -> Vec<(std::ffi::OsString, Vec<u8>)> {
    let mut out: Vec<_> =
        std::fs::read_dir(dir).unwrap().flatten().map(|e| (e.file_name(), std::fs::read(e.path()).unwrap())).collect();
    out.sort();
    out
}

#[test]
fn inspect_reads_the_identity_without_changing_anything() {
    let d = tempfile::tempdir().unwrap();
    let path = d.path().join("metadata.db");
    {
        let m = Metadata::open(&path).unwrap();
        m.create_dashboard("Prod", None).unwrap();
    }
    let before = files(d.path());
    assert_eq!(Metadata::inspect(&path).unwrap(), Identity::Current);
    assert_eq!(files(d.path()), before, "no -wal, -shm or journal file appears");

    // An empty file is an empty database.
    std::fs::write(&path, b"").unwrap();
    assert_eq!(Metadata::inspect(&path).unwrap(), Identity::Empty);
    // Paths with URI metacharacters still name the file (`?` is not allowed
    // in Windows file names).
    let odd = d.path().join(if cfg!(windows) { "a #b%c.db" } else { "a #b?c%d.db" });
    drop(Metadata::open(&odd).unwrap());
    assert_eq!(Metadata::inspect(&odd).unwrap(), Identity::Current);
    assert!(Metadata::inspect(&d.path().join("absent.db")).is_err(), "a missing file is not created");
    assert!(!d.path().join("absent.db").exists());
}

/// Open must refuse `path` as `ForeignLayout`, naming `found`, with every
/// byte of the database as it was.
fn assert_refused_unchanged(dir: &std::path::Path, path: &std::path::Path, found: &str) {
    let before = files(dir);
    match Metadata::inspect(path).unwrap() {
        Identity::Foreign(f) => assert!(f.starts_with(found), "{f}"),
        other => panic!("expected Foreign, got {other:?}"),
    }
    match Metadata::open(path) {
        Err(e @ MetadataError::ForeignLayout { .. }) => {
            let text = e.to_string();
            assert!(text.starts_with(&format!("the metadata database has {found}")), "{text}");
            assert!(
                text.ends_with(&format!(
                    ", but this Vyrtel reads layout {} (schema version 1) only",
                    telemetry::LAYOUT
                )),
                "{text}"
            );
        }
        Err(e) => panic!("expected ForeignLayout, got {e}"),
        Ok(_) => panic!("a database of another layout must not be opened"),
    }
    assert_eq!(files(dir), before, "nothing written, not even a journal mode");
}

/// FR-AC3: a database of another layout, or without an identity, is
/// refused before anything is written.
#[test]
fn other_layouts_are_refused_before_anything_is_written() {
    let d = tempfile::tempdir().unwrap();
    let path = d.path().join("metadata.db");

    // As 0.1.1 left it: schema version 1 in schema_migrations, no identity.
    {
        let c = rusqlite::Connection::open(&path).unwrap();
        c.execute_batch(
            "CREATE TABLE schema_migrations (version INTEGER PRIMARY KEY, applied_at INTEGER NOT NULL);
             INSERT INTO schema_migrations VALUES (1, 0);
             CREATE TABLE settings (key TEXT PRIMARY KEY, value TEXT NOT NULL, updated_at INTEGER NOT NULL);",
        )
        .unwrap();
    }
    assert_refused_unchanged(d.path(), &path, "no layout identity (it has schema_migrations");

    // Some other SQLite database.
    std::fs::remove_file(&path).unwrap();
    rusqlite::Connection::open(&path).unwrap().execute_batch("CREATE TABLE notes (body TEXT);").unwrap();
    assert_refused_unchanged(d.path(), &path, "no layout identity (it is not a Vyrtel metadata database)");

    // A database of another layout, and one of this layout but another
    // schema version.
    std::fs::remove_file(&path).unwrap();
    drop(Metadata::open(&path).unwrap());
    let set = |sql: &str| rusqlite::Connection::open(&path).unwrap().execute_batch(sql).unwrap();
    set("UPDATE vyrtel_layout SET layout = '0.1.9'");
    assert_refused_unchanged(d.path(), &path, "layout 0.1.9");
    set(&format!("UPDATE vyrtel_layout SET layout = '{}', schema_version = 2", telemetry::LAYOUT));
    assert_refused_unchanged(d.path(), &path, &format!("layout {} schema version 2", telemetry::LAYOUT));
    set("DELETE FROM vyrtel_layout");
    assert_refused_unchanged(d.path(), &path, "no layout identity (vyrtel_layout is empty)");
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
    let k = m.create_api_key("ci", "vyr_abcd", "hash-1", "ingest").unwrap();
    assert_eq!(m.active_api_keys().unwrap(), vec![(k.id, "hash-1".to_string(), "ingest".to_string())]);
    m.touch_api_key(k.id).unwrap();
    assert!(m.list_api_keys().unwrap()[0].last_used_at.is_some());
    m.revoke_api_key(k.id).unwrap();
    assert_eq!(m.active_api_keys().unwrap(), vec![]);
    assert_eq!(m.count_api_keys().unwrap(), 0);
    assert!(matches!(m.revoke_api_key(k.id), Err(MetadataError::NotFound)));
}

// AUTH-PERF-03: last-used writes are batched, and never touch revoked keys.
#[test]
fn touching_api_keys_in_a_batch_skips_revoked_keys() {
    let d = tempfile::tempdir().unwrap();
    let path = d.path().join("metadata.db");
    let m = Metadata::open(&path).unwrap();
    let a = m.create_api_key("a", "vyr_a", "hash-a", "ingest").unwrap();
    let b = m.create_api_key("b", "vyr_b", "hash-b", "admin").unwrap();
    let gone = m.create_api_key("gone", "vyr_g", "hash-g", "ingest").unwrap();
    m.revoke_api_key(gone.id).unwrap();
    assert_eq!(m.touch_api_keys(&[a.id, b.id, gone.id, 999]).unwrap(), 2);
    assert!(m.list_api_keys().unwrap().iter().all(|k| k.last_used_at.is_some()));
    assert_eq!(m.touch_api_keys(&[]).unwrap(), 0);
    drop(m);
    let c = rusqlite::Connection::open(&path).unwrap();
    let revoked_used: Option<i64> =
        c.query_row("SELECT last_used_at FROM api_keys WHERE id = ?1", [gone.id], |r| r.get(0)).unwrap();
    assert_eq!(revoked_used, None, "a revoked key's row is left alone");
}

// AUTH-250: the first ingest-scoped key latches ingestion protection in its
// own transaction; admin keys, revokes and reopening never clear it.
#[test]
fn the_first_ingest_key_latches_protection_for_good() {
    let d = tempfile::tempdir().unwrap();
    let path = d.path().join("metadata.db");
    let m = Metadata::open(&path).unwrap();
    assert!(!m.ingest_latched().unwrap(), "a fresh store is not latched");
    let admin = m.create_api_key("automation", "vyr_a", "hash-a", "admin").unwrap();
    m.revoke_api_key(admin.id).unwrap();
    assert!(!m.ingest_latched().unwrap(), "admin keys never latch");
    assert_eq!(m.get_setting(INGEST_PROTECTION).unwrap(), None);

    let first = m.create_api_key("ci", "vyr_i", "hash-i", "ingest").unwrap();
    assert_eq!(m.get_setting(INGEST_PROTECTION).unwrap().as_deref(), Some(INGEST_LATCHED));
    let c = rusqlite::Connection::open(&path).unwrap();
    let latched_at = || -> i64 {
        c.query_row("SELECT updated_at FROM settings WHERE key = ?1", [INGEST_PROTECTION], |r| r.get(0)).unwrap()
    };
    let at = latched_at();
    std::thread::sleep(std::time::Duration::from_millis(5));
    m.create_api_key("more", "vyr_m", "hash-m", "ingest").unwrap();
    m.revoke_api_key(first.id).unwrap();
    for k in m.list_api_keys().unwrap() {
        m.revoke_api_key(k.id).unwrap();
    }
    assert_eq!(m.count_api_keys().unwrap(), 0);
    assert!(m.ingest_latched().unwrap(), "revoking every key keeps the latch");
    assert_eq!(latched_at(), at, "later ingest keys keep the first latch time");
    drop((m, c));
    assert!(Metadata::open(&path).unwrap().ingest_latched().unwrap(), "and so does reopening");
}

// AUTH-250: a create that fails stores neither the key nor the latch; a
// failed latch write rolls the key back with it.
#[test]
fn a_failed_ingest_key_create_latches_nothing() {
    let d = tempfile::tempdir().unwrap();
    let path = d.path().join("metadata.db");
    let m = Metadata::open(&path).unwrap();
    m.create_api_key("a", "vyr_a", "same-hash", "admin").unwrap();
    assert!(m.create_api_key("dup", "vyr_d", "same-hash", "ingest").is_err(), "the key insert fails");
    assert!(matches!(m.create_api_key(" ", "vyr_x", "hash-x", "ingest"), Err(MetadataError::Invalid(_))));
    assert!(!m.ingest_latched().unwrap());

    let c = rusqlite::Connection::open(&path).unwrap();
    c.execute_batch("CREATE TRIGGER no_latch BEFORE INSERT ON settings BEGIN SELECT RAISE(ABORT, 'injected'); END")
        .unwrap();
    assert!(m.create_api_key("ci", "vyr_i", "hash-i", "ingest").is_err(), "the latch insert fails");
    assert_eq!(m.list_api_keys().unwrap().len(), 1, "the ingest key was rolled back with its latch");
    c.execute_batch("DROP TRIGGER no_latch").unwrap();
    assert!(!m.ingest_latched().unwrap());
}

// AUTH-250: a store with ingest-key history but no latch can only have had
// the row removed (every key is stored with it): refused, never read as
// open, and nothing is written.
#[test]
fn ingest_key_history_without_a_latch_is_refused() {
    for revoked in [false, true] {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("metadata.db");
        let m = Metadata::open(&path).unwrap();
        let k = m.create_api_key("ci", "vyr_i", "hash-i", "ingest").unwrap();
        if revoked {
            m.revoke_api_key(k.id).unwrap();
        }
        drop(m);
        let c = rusqlite::Connection::open(&path).unwrap();
        c.execute("DELETE FROM settings WHERE key = ?1", [INGEST_PROTECTION]).unwrap();
        drop(c);
        let m = Metadata::open(&path).unwrap();
        let e = m.ingest_latched().unwrap_err().to_string();
        assert!(e.contains("is missing although ingest-scoped keys were created"), "revoked={revoked}: {e}");
        assert!(e.contains("set the value to 'latched'"), "{e}");
        assert_eq!(m.get_setting(INGEST_PROTECTION).unwrap(), None, "nothing written");
    }
}

// AUTH-250: a latch value this build does not write is refused with what to
// do, never read as unlatched, and left as it is.
#[test]
fn an_unrecognised_latch_is_refused() {
    for value in ["", "open", "LATCHED", "0", "\u{0}"] {
        let m = Metadata::open_in_memory().unwrap();
        m.set_setting(INGEST_PROTECTION, value).unwrap();
        let e = m.ingest_latched().unwrap_err().to_string();
        assert!(e.contains("ingestion protection setting (ingest_protection)"), "{e}");
        assert!(e.contains("Restore the metadata database from a backup, or set the value to 'latched'"), "{e}");
        assert_eq!(m.get_setting(INGEST_PROTECTION).unwrap().as_deref(), Some(value), "unchanged");
    }
}

// AUTH-250: the ingestion mode is stored, defaults to automatic, survives
// reopening, never touches the latch, and an unknown value fails closed.
#[test]
fn the_ingest_auth_mode_is_stored_and_read_strictly() {
    let d = tempfile::tempdir().unwrap();
    let path = d.path().join("metadata.db");
    let m = Metadata::open(&path).unwrap();
    assert_eq!(m.ingest_auth_mode().unwrap(), IngestAuthMode::Automatic, "fresh: automatic");
    for mode in [IngestAuthMode::Open, IngestAuthMode::Required, IngestAuthMode::Automatic, IngestAuthMode::Open] {
        m.set_ingest_auth_mode(mode).unwrap();
        assert_eq!(m.ingest_auth_mode().unwrap(), mode);
        assert_eq!(IngestAuthMode::parse(mode.as_str()), Some(mode));
    }
    assert!(!m.ingest_latched().unwrap(), "setting a mode latches nothing");
    m.create_api_key("ci", "vyr_i", "hash-i", "ingest").unwrap();
    m.set_ingest_auth_mode(IngestAuthMode::Automatic).unwrap();
    assert!(m.ingest_latched().unwrap(), "and clears nothing");
    m.set_ingest_auth_mode(IngestAuthMode::Open).unwrap();
    drop(m);
    let m = Metadata::open(&path).unwrap();
    assert_eq!(m.ingest_auth_mode().unwrap(), IngestAuthMode::Open, "survives reopening");

    for value in ["", "OPEN", "opened", " open", "auto"] {
        m.set_setting(INGEST_AUTH_MODE, value).unwrap();
        let e = m.ingest_auth_mode().unwrap_err().to_string();
        assert!(e.contains("ingestion mode setting (ingest_auth_mode)"), "{e}");
        assert!(e.contains("set the value to 'automatic', 'required' or 'open'"), "{e}");
    }
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
fn rotating_a_password_ends_only_that_users_sessions() {
    let m = Metadata::open_in_memory().unwrap();
    let admin = m.upsert_user("admin", "old").unwrap();
    let other = m.upsert_user("ops", "hash").unwrap();
    m.create_session("a1", admin, 60_000).unwrap();
    m.create_session("a2", admin, 60_000).unwrap();
    m.create_session("o1", other, 60_000).unwrap();
    assert_eq!(m.rotate_password("admin", "new").unwrap(), (admin, 2));
    assert_eq!(m.find_user("admin").unwrap().unwrap().password_hash, "new");
    assert!(m.find_session("a1").unwrap().is_none() && m.find_session("a2").unwrap().is_none());
    assert_eq!(m.find_session("o1").unwrap().unwrap().username, "ops", "other users keep theirs");
    // A user that does not exist yet is created, with nothing to end.
    let (id, ended) = m.rotate_password("new-admin", "h").unwrap();
    assert_eq!((m.find_user("new-admin").unwrap().unwrap().id, ended), (id, 0));
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
