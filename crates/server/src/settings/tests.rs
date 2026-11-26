use std::sync::Arc;

use super::*;
use crate::config::{ByteSize, Durability};

fn save(group: &str, revision: u64, values: &[(&str, Value)]) -> Edit {
    let values = values.iter().map(|(k, v)| (k.to_string(), Some(v.clone()))).collect();
    Edit::Save { group: group.into(), revision, values, confirm: false }
}

fn confirmed(edit: Edit) -> Edit {
    match edit {
        Edit::Save { group, revision, values, .. } => Edit::Save { group, revision, values, confirm: true },
        other => other,
    }
}

fn rows(m: &Metadata) -> Vec<(String, String)> {
    m.config_settings().unwrap()
}

/// TC: every one of the 37 configuration-file fields is either an
/// admin-managed setting or a deployment input, never both or neither.
#[test]
fn every_configuration_field_has_one_owner() {
    let json = serde_json::to_value(Config::default()).unwrap();
    let mut all: BTreeSet<String> = BTreeSet::new();
    for (section, fields) in json.as_object().unwrap() {
        for name in fields.as_object().unwrap().keys() {
            all.insert(format!("{section}.{name}"));
        }
    }
    // Never serialized, but a configuration-file field.
    all.insert("auth.admin_password".into());
    assert_eq!(all.len(), 37, "{all:?}");
    let managed: BTreeSet<String> = FIELDS.iter().map(|f| f.key.to_string()).collect();
    let deployment: BTreeSet<String> = DEPLOYMENT.iter().map(|k| k.to_string()).collect();
    assert_eq!(managed.len(), FIELDS.len(), "no duplicates");
    assert_eq!(managed.len(), 32);
    assert!(managed.is_disjoint(&deployment));
    assert_eq!(&managed | &deployment, all);
    for f in FIELDS {
        assert!(GROUPS.contains(&f.group()), "{}", f.key);
        assert_eq!(f.apply, Apply::Restart, "{}: no field is applied live yet", f.key);
    }
    for g in GROUPS {
        assert!(FIELDS.iter().any(|f| f.group() == *g), "{g} has fields");
    }
}

/// Defaults round-trip through their canonical stored form.
#[test]
fn defaults_roundtrip_through_storage() {
    let defaults = Config::default();
    let mut rows = Vec::new();
    for f in FIELDS {
        let v = get(&defaults, f.key);
        assert!(!v.is_null(), "{}", f.key);
        let mut c = Config::default();
        set(&mut c, f.key, &v).unwrap();
        assert_eq!(get(&c, f.key), v, "{}", f.key);
        rows.push((value_key(f.key), v.to_string()));
    }
    let stored = Stored::parse(&rows).unwrap();
    assert_eq!(stored.values.len(), FIELDS.len());
    let applied = stored.apply(&Config::default()).unwrap();
    assert_eq!(serde_json::to_value(&applied).unwrap(), serde_json::to_value(&defaults).unwrap());
}

/// Values are accepted in every form the file accepts, stored in one.
#[test]
fn values_are_canonical() {
    let mut c = Config::default();
    for (key, given, canonical) in [
        ("query.timeout", json!(90), json!("90s")),
        ("query.timeout", json!("1m30s"), json!("90s")),
        ("storage.max_memory", json!("1GB"), json!(1u64 << 30)),
        ("storage.max_memory", json!(268435456), json!(268435456)),
        ("retention.logs", json!("forever"), json!("forever")),
        ("retention.logs", json!(0), json!("forever")),
        ("retention.traces", json!("14d"), json!("2w")),
        ("storage.durability", json!("strict"), json!("strict")),
        ("alerts.enabled", json!(false), json!(false)),
        ("log.level", json!("storage=debug,info"), json!("storage=debug,info")),
    ] {
        set(&mut c, key, &given).unwrap_or_else(|e| panic!("{key} {given}: {e}"));
        assert_eq!(get(&c, key), canonical, "{key} {given}");
    }
    assert_eq!(c.storage.durability, Durability::Strict);
}

/// TC: unknown, mistyped, negative and overflowing values are refused with
/// the field's name, and change nothing.
#[test]
fn bad_values_are_refused() {
    for (key, value) in [
        ("query.timeout", json!("soon")),
        ("query.timeout", json!(-1)),
        ("query.timeout", json!("18446744073709551615w")),
        ("query.max_concurrent", json!(-1)),
        ("query.max_concurrent", json!(2.5)),
        ("query.max_concurrent", json!("4")),
        ("query.max_concurrent", json!(1e30)),
        ("query.max_results", json!(u64::MAX as f64 * 4.0)),
        ("storage.max_memory", json!("12XB")),
        ("storage.durability", json!("maybe")),
        ("alerts.enabled", json!("yes")),
        ("log.format", json!("xml")),
        ("retention.logs", json!(true)),
        ("server.max_connections", Value::Null),
    ] {
        let mut c = Config::default();
        let before = serde_json::to_value(&c).unwrap();
        let e = set(&mut c, key, &value).unwrap_err();
        assert!(e.starts_with(key), "{key} {value}: {e}");
        assert_eq!(serde_json::to_value(&c).unwrap(), before, "{key}: unchanged");
    }
    for key in ["server.bind", "storage.path", "auth.admin_password", "auth.admin_username", "nope.x"] {
        assert!(set(&mut Config::default(), key, &json!("x")).is_err(), "{key}");
    }
}

/// Setting a field never touches the password or the data directory.
#[test]
fn setting_keeps_bootstrap_inputs() {
    let mut c = Config::default();
    c.auth.admin_password = Some("correct horse".into());
    c.storage.path = "/srv/vyrtel".into();
    set(&mut c, "auth.session_ttl", &json!("1d")).unwrap();
    set(&mut c, "storage.compaction", &json!(false)).unwrap();
    assert_eq!(c.auth.admin_password.as_deref(), Some("correct horse"));
    assert_eq!(c.storage.path, PathBuf::from("/srv/vyrtel"));
}

#[test]
fn saving_needs_the_current_revision() {
    let base = Config::default();
    let stored = Stored::default();
    let p = plan(&base, &base, &stored, &save("query", 0, &[("timeout", json!("5s"))])).unwrap();
    assert_eq!(p.stored.revision("query"), 1);
    assert_eq!(p.stored.values["query.timeout"], json!("5s"));
    assert_eq!(p.fields, ["query.timeout"]);
    assert_eq!(p.next.query.timeout, std::time::Duration::from_secs(5));
    let e = plan(&base, &base, &p.stored, &save("query", 0, &[("timeout", json!("6s"))])).unwrap_err();
    assert!(matches!(e, SettingsError::Stale { expected: 0, current: 1, .. }), "{e}");
    // Groups are independent.
    plan(&base, &base, &p.stored, &save("log", 0, &[("level", json!("debug"))])).unwrap();
}

/// TC: the complete result is validated, across fields and groups.
#[test]
fn the_whole_configuration_is_validated() {
    let base = Config::default();
    let stored = Stored::default();
    for (group, name, value, want) in [
        ("storage", "max_memory", json!("64MB"), "ingest budget too small"),
        ("query", "max_concurrent", json!(0), "query.max_concurrent must be positive"),
        ("server", "connection_buffer", json!(1), "server.connection_buffer must be between"),
        ("server", "header_timeout", json!("2h"), "server.header_timeout must be positive and at most 1h"),
        ("ingest", "max_request_size", json!(10), "at least 1KB"),
        ("log", "level", json!("[[["), "not a valid log filter"),
    ] {
        let e = plan(&base, &base, &stored, &save(group, 0, &[(name, value)])).unwrap_err();
        assert!(matches!(e, SettingsError::Invalid(_)) && e.to_string().contains(want), "{group}.{name}: {e}");
    }
    // Fixed together, in one group: memory budget and explicit admission.
    let mut ok = save("ingest", 0, &[("max_concurrent", json!(1)), ("max_request_size", json!("256KB"))]);
    plan(&base, &base, &stored, &ok).unwrap();
    // Unknown fields and groups, and deployment inputs, are not settings.
    ok = save("query", 0, &[("max_memroy", json!(1))]);
    assert!(matches!(plan(&base, &base, &stored, &ok), Err(SettingsError::Unknown(_))));
    ok = save("server", 0, &[("bind", json!("0.0.0.0:1"))]);
    let e = plan(&base, &base, &stored, &ok).unwrap_err();
    assert!(e.to_string().contains("deployment input"), "{e}");
    ok = save("nope", 0, &[("x", json!(1))]);
    assert!(matches!(plan(&base, &base, &stored, &ok), Err(SettingsError::UnknownGroup(_))));
    ok = Edit::Save { group: "query".into(), revision: 0, values: BTreeMap::new(), confirm: false };
    assert!(matches!(plan(&base, &base, &stored, &ok), Err(SettingsError::Invalid(_))));
}

/// A field the deployment sets wins and cannot be saved; one it does not
/// set can.
#[test]
fn deployment_values_win_and_are_not_editable() {
    let mut config = Config::from_toml("[query]\ntimeout = \"7s\"").unwrap();
    config.apply_env(|k| (k == "VYRTEL_LOG_LEVEL").then(|| "warn".to_string())).unwrap();
    let base = deployment_base(&config);
    assert_eq!(base.query.timeout, std::time::Duration::from_secs(7));
    assert_eq!(base.origin.get("query.timeout"), Source::File);
    assert_eq!(base.origin.get("log.level"), Source::Environment);
    let e = plan(&base, &base, &Stored::default(), &save("query", 0, &[("timeout", json!("1s"))])).unwrap_err();
    assert_eq!(
        e.to_string(),
        "query.timeout is set by the configuration file, which takes precedence over saved settings; remove it \
         there to manage it in Settings"
    );
    let e = plan(&base, &base, &Stored::default(), &save("log", 0, &[("level", json!("debug"))])).unwrap_err();
    assert!(matches!(e, SettingsError::Deployment { by: "environment", .. }), "{e}");
    plan(&base, &base, &Stored::default(), &save("query", 0, &[("max_results", json!(5))])).unwrap();

    // A saved value the deployment later sets is overridden, and reported.
    let rows = [(value_key("query.timeout"), "\"1s\"".to_string()), (value_key("query.max_results"), "5".into())];
    let (effective, stored) = load(&rows, &config).unwrap();
    assert_eq!(effective.query.timeout, std::time::Duration::from_secs(7));
    assert_eq!(effective.query.max_results, 5);
    assert_eq!(effective.origin.get("query.max_results"), Source::Settings);
    assert_eq!(stored.overridden(&base), vec![("query.timeout".to_string(), Source::File)]);
}

#[test]
fn dangerous_changes_need_confirmation() {
    let base = Config::default();
    let stored = Stored::default();
    for (group, name, value) in [
        ("retention", "logs", json!("1d")),
        ("storage", "durability", json!("strict")),
        ("storage", "fsync_interval", json!("10s")),
        ("storage", "compaction", json!(false)),
    ] {
        let edit = save(group, 0, &[(name, value)]);
        match plan(&base, &base, &stored, &edit) {
            Err(SettingsError::ConfirmationRequired(fields)) => assert_eq!(fields, [format!("{group}.{name}")]),
            other => panic!("{group}.{name}: {other:?}"),
        }
        plan(&base, &base, &stored, &confirmed(edit)).unwrap();
    }
    // Saving a dangerous field at the value it already has needs none.
    plan(&base, &base, &stored, &save("retention", 0, &[("logs", json!("30d"))])).unwrap();
}

/// TC: pending values can be cancelled back to what is running.
#[test]
fn cancelling_returns_to_the_running_values() {
    let running = Config::default();
    let mut stored = Stored::default();
    stored =
        plan(&running, &running, &stored, &save("query", 0, &[("timeout", json!("5s")), ("max_results", json!(9))]))
            .unwrap()
            .stored;
    let doc = document(&running, &running, &stored, Vec::new());
    assert_eq!(doc["restartRequired"], true);
    let cancel = Edit::Cancel { group: "query".into(), revision: 0 };
    assert!(matches!(plan(&running, &running, &stored, &cancel), Err(SettingsError::Stale { .. })));
    let cancel = Edit::Cancel { group: "query".into(), revision: 1 };
    let p = plan(&running, &running, &stored, &cancel).unwrap();
    assert!(p.stored.values.is_empty(), "defaults are not stored");
    assert_eq!(p.stored.revision("query"), 2);
    assert_eq!(document(&running, &running, &p.stored, Vec::new())["restartRequired"], false);

    // Running a saved value: cancelling a later change keeps it saved.
    let mut started = Config::default();
    set(&mut started, "query.timeout", &json!("5s")).unwrap();
    let p = plan(&running, &started, &p.stored, &save("query", 2, &[("timeout", json!("8s"))])).unwrap();
    let p = plan(&running, &started, &p.stored, &Edit::Cancel { group: "query".into(), revision: 3 }).unwrap();
    assert_eq!(p.stored.values["query.timeout"], json!("5s"));
}

#[test]
fn imports_replace_everything_and_refuse_what_is_not_a_setting() {
    let base = Config::default();
    let mut stored =
        plan(&base, &base, &Stored::default(), &save("query", 0, &[("timeout", json!("5s"))])).unwrap().stored;
    stored = plan(&base, &base, &stored, &save("log", 0, &[("level", json!("debug"))])).unwrap().stored;
    let doc = export(&stored);
    assert_eq!(doc["format"], EXPORT_FORMAT);
    assert_eq!(doc["settings"], json!({ "log.level": "debug", "query.timeout": "5s" }));

    let mut values = import_values(&doc).unwrap();
    values.remove("log.level");
    values.insert("alerts.enabled".into(), json!(false));
    let revisions: BTreeMap<String, u64> = stored.revisions.clone();
    let edit = Edit::Import { revisions: Some(revisions.clone()), values: values.clone(), confirm: false };
    let p = plan(&base, &base, &stored, &edit).unwrap();
    assert_eq!(p.stored.values.keys().collect::<Vec<_>>(), ["alerts.enabled", "query.timeout"]);
    assert_eq!(p.groups, ["alerts", "log"], "only changed groups move");
    // Stale, and missing revisions count as 0.
    let edit = Edit::Import { revisions: Some(BTreeMap::new()), values: values.clone(), confirm: false };
    assert!(matches!(plan(&base, &base, &stored, &edit), Err(SettingsError::Stale { .. })));

    for key in ["storage.path", "server.bind", "auth.admin_password", "auth.admin_username", "ingest_protection"] {
        let mut v = values.clone();
        v.insert(key.into(), json!("x"));
        let edit = Edit::Import { revisions: Some(revisions.clone()), values: v, confirm: false };
        assert!(matches!(plan(&base, &base, &stored, &edit), Err(SettingsError::Unknown(_))), "{key}");
    }
    for bad in [
        json!({}),
        json!({ "format": "other", "version": 1, "settings": {} }),
        json!({ "format": EXPORT_FORMAT, "version": 2, "settings": {} }),
    ] {
        assert!(import_values(&bad).is_err(), "{bad}");
    }
}

/// TC: what is stored but unusable stops the start with how to fix it;
/// it is never read as a default.
#[test]
fn unusable_saved_settings_fail_actionably() {
    let config = Config::default();
    for (row, value, want) in [
        ("config.query.timeout", "\"soon\"", "saved setting query.timeout"),
        ("config.query.timeout", "not json", "saved setting query.timeout (\"not json\") is unreadable"),
        ("config.query.nope", "1", "\"query.nope\", which this Vyrtel does not know"),
        ("config.revision.query", "x", "revision of query is \"x\""),
        ("config.revision.nope", "1", "unknown group \"nope\""),
        ("config.storage.durability", "\"off\"", "saved setting storage.durability"),
    ] {
        let e = load(&[(row.to_string(), value.to_string())], &config).unwrap_err();
        assert!(e.contains(want), "{row}: {e}");
        assert!(e.contains("vyrtel settings reset") || e.contains("unknown group"), "{row}: {e}");
    }
    // Each value fine on its own, invalid together.
    let rows = [(value_key("storage.max_memory"), (64u64 << 20).to_string())];
    let e = load(&rows, &config).unwrap_err();
    assert!(e.starts_with("saved settings do not form a valid configuration: ingest budget too small"), "{e}");
    assert!(e.contains("vyrtel settings show"), "{e}");
    // Damaged change records are diagnostics: replaced, not fatal.
    let (_, stored) = load(&[(CHANGES_KEY.to_string(), "[".to_string())], &config).unwrap();
    assert!(stored.changes.is_empty());
}

/// TC: commits are atomic with their revision and record; records are
/// bounded; a stale commit (another writer) writes nothing.
#[test]
fn commits_are_atomic_and_records_bounded() {
    let m = Metadata::open_in_memory().unwrap();
    let base = Config::default();
    let mut stored = Stored::default();
    for i in 0..(MAX_CHANGES as u64 + 5) {
        let p = plan(&base, &base, &stored, &save("query", i, &[("max_results", json!(i + 1))])).unwrap();
        stored = commit(&m, p, "user:admin", "save").unwrap();
    }
    let read = Stored::parse(&rows(&m)).unwrap();
    assert_eq!(read, stored);
    assert_eq!(read.revision("query"), MAX_CHANGES as u64 + 5);
    assert_eq!(read.changes.len(), MAX_CHANGES);
    let last = read.changes.last().unwrap();
    assert_eq!((last.actor.as_str(), last.outcome.as_str()), ("user:admin", "committed"));
    assert_eq!(last.fields, ["query.max_results"]);
    assert_eq!(last.revisions["query"], MAX_CHANGES as u64 + 5);

    // Two plans from the same state: the second commit conflicts.
    let a = plan(&base, &base, &stored, &save("query", stored.revision("query"), &[("timeout", json!("1s"))])).unwrap();
    let b = plan(&base, &base, &stored, &save("query", stored.revision("query"), &[("timeout", json!("2s"))])).unwrap();
    commit(&m, a, "cli", "save").unwrap();
    let before = rows(&m);
    assert!(matches!(commit(&m, b, "cli", "save"), Err(SettingsError::Stale { .. })));
    assert_eq!(rows(&m), before, "nothing of the stale commit was written");
}

/// Records and documents never carry values, passwords or the latch.
#[test]
fn documents_are_redacted() {
    let mut config = Config::default();
    config.auth.admin_password = Some("correct horse".into());
    config.origin.set("auth.admin_password", Source::Environment);
    let base = deployment_base(&config);
    let stored =
        plan(&base, &config, &Stored::default(), &save("query", 0, &[("timeout", json!("5s"))])).unwrap().stored;
    let doc = document(&base, &config, &stored, stored.changes.clone()).to_string();
    assert!(!doc.contains("correct horse"), "{doc}");
    assert!(!doc.contains("ingest_protection") && !doc.contains("latch"), "{doc}");
    let d: Value = serde_json::from_str(&doc).unwrap();
    let pw = d["deployment"].as_array().unwrap().iter().find(|e| e["key"] == "auth.admin_password").unwrap();
    assert_eq!((&pw["set"], &pw["source"], &pw["value"]), (&json!(true), &json!("environment"), &Value::Null));
    let query = d["groups"].as_array().unwrap().iter().find(|g| g["name"] == "query").unwrap();
    let timeout = query["fields"].as_array().unwrap().iter().find(|f| f["name"] == "timeout").unwrap();
    assert_eq!(timeout["effective"], "30s");
    assert_eq!(timeout["configured"], "5s");
    assert_eq!(timeout["pending"], "5s");
    assert_eq!(timeout["source"], "default");
    assert_eq!(timeout["apply"], "restart");
    assert_eq!(query["revision"], 1);
    let change = serde_json::to_string(&Change {
        at: 0,
        actor: "user:admin".into(),
        action: "save".into(),
        groups: vec!["query".into()],
        revisions: BTreeMap::new(),
        fields: vec!["query.timeout".into()],
        outcome: "committed".into(),
        apply: Apply::Restart,
    })
    .unwrap();
    assert!(!change.contains("5s"));
}

/// The server's writer: commits, then publishes; refusals are recorded
/// (bounded) and publish nothing.
#[tokio::test]
async fn the_server_publishes_only_what_it_committed() {
    let m = Arc::new(Metadata::open_in_memory().unwrap());
    let config = Config::default();
    let s = Settings::new(&config, Stored::default());
    s.change(&m, &config, "user:admin".into(), save("query", 0, &[("timeout", json!("5s"))])).await.unwrap();
    assert_eq!(s.stored().revision("query"), 1);
    assert_eq!(Stored::parse(&rows(&m)).unwrap(), s.stored());
    for _ in 0..(MAX_CHANGES + 10) {
        let e = s.change(&m, &config, "api-key:3".into(), save("query", 0, &[("timeout", json!("6s"))])).await;
        assert!(matches!(e, Err(SettingsError::Stale { .. })));
    }
    assert_eq!(s.stored().values["query.timeout"], json!("5s"));
    let doc = s.document(&config);
    let changes = doc["changes"].as_array().unwrap();
    assert_eq!(changes.len(), MAX_CHANGES);
    assert_eq!(changes.last().unwrap()["outcome"], "conflict");
    assert_eq!(changes.last().unwrap()["actor"], "api-key:3");

    // A failing store publishes nothing.
    let closed = Arc::new(Metadata::open_in_memory().unwrap());
    let before = s.stored();
    let mut cfg = config.clone();
    cfg.storage.max_memory = ByteSize(1 << 30);
    // Another writer moved the revision in the database behind this one.
    closed.commit_config_settings(&[], &[(revision_key("log"), Some("4".into()))]).unwrap();
    let e = s.change(&closed, &cfg, "user:admin".into(), save("log", 0, &[("level", json!("debug"))])).await;
    assert!(matches!(e, Err(SettingsError::Stale { .. })), "{e:?}");
    assert_eq!(s.stored(), before);
}

#[test]
fn cli_values() {
    assert_eq!(parse_cli_value("30"), json!(30));
    assert_eq!(parse_cli_value("30s"), json!("30s"));
    assert_eq!(parse_cli_value("true"), json!(true));
    assert_eq!(parse_cli_value(" debug "), json!("debug"));
    assert_eq!(parse_cli_value("-1"), json!("-1"));
}

/// A value set in code (an embedder or a test) with no recorded source is
/// kept over a saved one, like a deployment value; values that came from
/// settings go back to their defaults, so loading twice gives one result.
#[test]
fn values_set_in_code_are_kept_and_loading_twice_is_stable() {
    let mut config = Config::default();
    config.ingest.max_concurrent = 8;
    config.server.shutdown_timeout = std::time::Duration::from_millis(300);
    let rows =
        [(value_key("ingest.max_concurrent"), "2".to_string()), (value_key("query.max_results"), "5".to_string())];
    let (once, _) = load(&rows, &config).unwrap();
    assert_eq!(once.ingest.max_concurrent, 8);
    assert_eq!(once.origin.get("ingest.max_concurrent"), Source::Caller);
    assert_eq!(once.server.shutdown_timeout, std::time::Duration::from_millis(300));
    assert_eq!(once.query.max_results, 5);
    let (twice, _) = load(&rows[1..], &once).unwrap();
    assert_eq!(twice.query.max_results, 5);
    let (without, _) = load(&[], &once).unwrap();
    assert_eq!(without.query.max_results, 1000, "a removed saved value no longer applies");
    let base = deployment_base(&config);
    let e = plan(&base, &base, &Stored::default(), &save("ingest", 0, &[("max_concurrent", json!(1))])).unwrap_err();
    assert!(matches!(e, SettingsError::Deployment { .. }), "{e}");
}
