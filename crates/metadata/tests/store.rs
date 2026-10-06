use metadata::{AlertOperator, AlertStatus, MetadataStore, Visualization, WidgetPosition};

async fn open() -> (tempfile::TempDir, MetadataStore) {
    let dir = tempfile::tempdir().unwrap();
    let store = MetadataStore::open(&dir.path().join("metadata.db")).await.unwrap();
    (dir, store)
}

fn pos(x: i32, y: i32) -> WidgetPosition {
    WidgetPosition { x, y, w: 2, h: 1 }
}

// --- accounts & auth --------------------------------------------------------

#[tokio::test]
async fn fresh_database_has_default_admin() {
    let (_dir, store) = open().await;
    let accounts = store.list_accounts().await.unwrap();
    assert_eq!(accounts.len(), 1);
    assert_eq!(accounts[0].username, "admin");
    assert_eq!(accounts[0].display_name, "Administrator");
    assert!(store.verify_login("admin", "admin").await.unwrap().is_some());
}

#[tokio::test]
async fn reopening_does_not_duplicate_admin_or_lose_data() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("nested").join("metadata.db");
    {
        let store = MetadataStore::open(&path).await.unwrap();
        store.create_saved_filter("errors", "level = error").await.unwrap();
    }
    let store = MetadataStore::open(&path).await.unwrap();
    assert_eq!(store.list_accounts().await.unwrap().len(), 1);
    assert_eq!(store.list_saved_filters().await.unwrap().len(), 1);
}

#[tokio::test]
async fn login_checks_password() {
    let (_dir, store) = open().await;
    let created = store.create_account("alice", "s3cret", "Alice").await.unwrap();
    let ok = store.verify_login("alice", "s3cret").await.unwrap().unwrap();
    assert_eq!(ok.id, created.id);
    assert!(store.verify_login("alice", "wrong").await.unwrap().is_none());
    assert!(store.verify_login("alice", "").await.unwrap().is_none());
    assert!(store.verify_login("nobody", "s3cret").await.unwrap().is_none());
    // Usernames are case-sensitive.
    assert!(store.verify_login("Alice", "s3cret").await.unwrap().is_none());
}

#[tokio::test]
async fn passwords_are_salted_and_not_stored_in_plaintext() {
    let (_dir, store) = open().await;
    store.create_account("a", "same-password", "A").await.unwrap();
    store.create_account("b", "same-password", "B").await.unwrap();
    let a = store.find_account_by_username("a").await.unwrap().unwrap();
    let b = store.find_account_by_username("b").await.unwrap().unwrap();
    assert!(!a.password_hash.contains("same-password"));
    assert_ne!(a.password_hash, b.password_hash, "identical passwords must hash differently");
}

#[tokio::test]
async fn duplicate_username_is_rejected() {
    let (_dir, store) = open().await;
    store.create_account("bob", "pw12", "Bob").await.unwrap();
    assert!(store.create_account("bob", "other", "Bob 2").await.is_err());
}

#[tokio::test]
async fn accounts_are_listed_by_username() {
    let (_dir, store) = open().await;
    store.create_account("zed", "pw12", "Z").await.unwrap();
    store.create_account("bob", "pw12", "B").await.unwrap();
    let names: Vec<_> = store.list_accounts().await.unwrap().into_iter().map(|a| a.username).collect();
    assert_eq!(names, ["admin", "bob", "zed"]);
}

#[tokio::test]
async fn get_account_by_id() {
    let (_dir, store) = open().await;
    let created = store.create_account("carol", "pw12", "Carol").await.unwrap();
    let got = store.get_account(&created.id).await.unwrap().unwrap();
    assert_eq!(got.username, "carol");
    assert!(store.get_account("missing").await.unwrap().is_none());
}

#[tokio::test]
async fn session_lifecycle() {
    let (_dir, store) = open().await;
    let account = store.create_account("dave", "pw12", "Dave").await.unwrap();
    let session = store.create_session(&account.id, 1).await.unwrap();
    assert_eq!(session.account_id, account.id);

    let resolved = store.get_session_account(&session.token).await.unwrap().unwrap();
    assert_eq!(resolved.id, account.id);

    store.delete_session(&session.token).await.unwrap();
    assert!(store.get_session_account(&session.token).await.unwrap().is_none());
}

#[tokio::test]
async fn expired_sessions_are_rejected() {
    let (_dir, store) = open().await;
    let account = store.create_account("erin", "pw12", "Erin").await.unwrap();
    let session = store.create_session(&account.id, -1).await.unwrap();
    assert!(store.get_session_account(&session.token).await.unwrap().is_none());
}

#[tokio::test]
async fn unknown_session_token_is_rejected() {
    let (_dir, store) = open().await;
    assert!(store.get_session_account("not-a-token").await.unwrap().is_none());
    assert!(store.get_session_account("").await.unwrap().is_none());
}

#[tokio::test]
async fn api_key_lifecycle() {
    let (_dir, store) = open().await;
    let account = store.create_account("frank", "pw12", "Frank").await.unwrap();
    let created = store.create_api_key(&account.id, "ci").await.unwrap();

    assert!(created.key.starts_with("obs_"));
    assert_eq!(created.info.key_prefix.len(), 12);
    assert!(created.key.starts_with(&created.info.key_prefix));

    let owner = store.find_account_by_api_key(&created.key).await.unwrap().unwrap();
    assert_eq!(owner.id, account.id);
    assert!(store.find_account_by_api_key("obs_wrong").await.unwrap().is_none());
    assert!(store.find_account_by_api_key(&created.info.key_prefix).await.unwrap().is_none());

    let keys = store.list_api_keys(&account.id).await.unwrap();
    assert_eq!(keys.len(), 1);
    assert_eq!(keys[0].name, "ci");

    store.delete_api_key(&created.info.id).await.unwrap();
    assert!(store.find_account_by_api_key(&created.key).await.unwrap().is_none());
    assert!(store.list_api_keys(&account.id).await.unwrap().is_empty());
}

#[tokio::test]
async fn api_keys_are_listed_per_account_and_globally() {
    let (_dir, store) = open().await;
    let a = store.create_account("a", "pw12", "A").await.unwrap();
    let b = store.create_account("b", "pw12", "B").await.unwrap();
    let k1 = store.create_api_key(&a.id, "one").await.unwrap();
    let k2 = store.create_api_key(&b.id, "two").await.unwrap();
    assert_ne!(k1.key, k2.key);
    assert_eq!(store.list_api_keys(&a.id).await.unwrap().len(), 1);
    assert_eq!(store.list_api_keys(&b.id).await.unwrap().len(), 1);
    assert_eq!(store.list_all_api_keys().await.unwrap().len(), 2);
}

// --- settings ---------------------------------------------------------------

#[tokio::test]
async fn retention_setting() {
    let (_dir, store) = open().await;
    assert_eq!(store.get_retention_days().await.unwrap(), None, "unset by default");

    store.set_retention_days(Some(7)).await.unwrap();
    assert_eq!(store.get_retention_days().await.unwrap(), Some(7));
    assert_eq!(store.get_settings().await.unwrap().retention_days, Some(7));

    store.set_retention_days(Some(90)).await.unwrap();
    assert_eq!(store.get_retention_days().await.unwrap(), Some(90));

    store.set_retention_days(None).await.unwrap();
    assert_eq!(store.get_retention_days().await.unwrap(), None);
}

// --- saved filters ----------------------------------------------------------

#[tokio::test]
async fn saved_filters_crud() {
    let (_dir, store) = open().await;
    let b = store.create_saved_filter("b-errors", "level = error").await.unwrap();
    store.create_saved_filter("a-slow", "duration_ns > 1000").await.unwrap();

    let list = store.list_saved_filters().await.unwrap();
    let names: Vec<_> = list.iter().map(|f| f.name.as_str()).collect();
    assert_eq!(names, ["a-slow", "b-errors"]);
    assert_eq!(list[1].query, "level = error");

    store.delete_saved_filter(&b.id).await.unwrap();
    assert_eq!(store.list_saved_filters().await.unwrap().len(), 1);
    // Deleting something that doesn't exist is not an error.
    store.delete_saved_filter("missing").await.unwrap();
}

// --- dashboards -------------------------------------------------------------

#[tokio::test]
async fn dashboards_crud() {
    let (_dir, store) = open().await;
    assert_eq!(store.count_dashboards().await.unwrap(), 0);
    let d = store.create_dashboard("Ops").await.unwrap();
    store.create_dashboard("Billing").await.unwrap();
    assert_eq!(store.count_dashboards().await.unwrap(), 2);

    let names: Vec<_> = store.list_dashboards().await.unwrap().into_iter().map(|d| d.name).collect();
    assert_eq!(names, ["Billing", "Ops"]);

    store.rename_dashboard(&d.id, "Operations").await.unwrap();
    assert_eq!(store.get_dashboard(&d.id).await.unwrap().unwrap().name, "Operations");

    store.delete_dashboard(&d.id).await.unwrap();
    assert!(store.get_dashboard(&d.id).await.unwrap().is_none());
    assert_eq!(store.count_dashboards().await.unwrap(), 1);
}

#[tokio::test]
async fn widgets_crud_and_ordering() {
    let (_dir, store) = open().await;
    let d = store.create_dashboard("Ops").await.unwrap();
    let w1 = store
        .create_widget(&d.id, "Errors", "level = error | count", Visualization::Number, pos(2, 1))
        .await
        .unwrap();
    store
        .create_widget(&d.id, "By service", "| count by service", Visualization::Bar, pos(0, 1))
        .await
        .unwrap();
    store
        .create_widget(&d.id, "Top", "| count by time(5m)", Visualization::Line, pos(5, 0))
        .await
        .unwrap();

    let titles: Vec<_> = store.list_widgets(&d.id).await.unwrap().into_iter().map(|w| w.title).collect();
    assert_eq!(titles, ["Top", "By service", "Errors"], "ordered by row, then column");

    store
        .update_widget(&w1.id, "Fatal", "level = fatal | count", Visualization::Table, pos(0, 9))
        .await
        .unwrap();
    let widgets = store.list_widgets(&d.id).await.unwrap();
    let updated = widgets.iter().find(|w| w.id == w1.id).unwrap();
    assert_eq!(updated.title, "Fatal");
    assert_eq!(updated.query, "level = fatal | count");
    assert!(matches!(updated.visualization, Visualization::Table));
    assert_eq!((updated.position.x, updated.position.y), (0, 9));

    store.delete_widget(&w1.id).await.unwrap();
    assert_eq!(store.list_widgets(&d.id).await.unwrap().len(), 2);
}

#[tokio::test]
async fn deleting_dashboard_deletes_its_widgets_only() {
    let (_dir, store) = open().await;
    let d1 = store.create_dashboard("One").await.unwrap();
    let d2 = store.create_dashboard("Two").await.unwrap();
    store.create_widget(&d1.id, "a", "| count", Visualization::Number, pos(0, 0)).await.unwrap();
    store.create_widget(&d2.id, "b", "| count", Visualization::Number, pos(0, 0)).await.unwrap();

    store.delete_dashboard(&d1.id).await.unwrap();
    assert!(store.list_widgets(&d1.id).await.unwrap().is_empty());
    assert_eq!(store.list_widgets(&d2.id).await.unwrap().len(), 1);
}

#[test]
fn visualization_string_round_trip() {
    for v in [Visualization::Number, Visualization::Line, Visualization::Bar, Visualization::Table] {
        let s = v.as_str();
        assert_eq!(Visualization::parse(s).unwrap().as_str(), s);
        assert_eq!(serde_json::to_value(&v).unwrap(), serde_json::Value::String(s.into()));
    }
    assert!(Visualization::parse("pie").is_none());
}

// --- alerts -----------------------------------------------------------------

#[test]
fn alert_operator_parse_and_compare() {
    use AlertOperator::*;
    for (s, sym, op) in [("gt", ">", Gt), ("gte", ">=", Gte), ("lt", "<", Lt), ("lte", "<=", Lte), ("eq", "=", Eq)] {
        assert_eq!(AlertOperator::parse(s), Some(op));
        assert_eq!(AlertOperator::parse(sym), Some(op));
        assert_eq!(op.as_str(), s);
    }
    assert_eq!(AlertOperator::parse("!="), None);

    assert!(Gt.compare(5.0, 4.0) && !Gt.compare(4.0, 4.0));
    assert!(Gte.compare(4.0, 4.0) && !Gte.compare(3.9, 4.0));
    assert!(Lt.compare(3.0, 4.0) && !Lt.compare(4.0, 4.0));
    assert!(Lte.compare(4.0, 4.0) && !Lte.compare(4.1, 4.0));
    assert!(Eq.compare(4.0, 4.0) && !Eq.compare(4.0001, 4.0));
}

#[test]
fn alert_status_parse() {
    assert_eq!(AlertStatus::parse("ok"), Some(AlertStatus::Ok));
    assert_eq!(AlertStatus::parse("firing"), Some(AlertStatus::Firing));
    assert_eq!(AlertStatus::parse("FIRING"), None);
    assert_eq!(AlertStatus::Firing.as_str(), "firing");
}

#[tokio::test]
async fn alert_rule_lifecycle() {
    let (_dir, store) = open().await;
    let rule = store
        .create_alert_rule("Errors", "level = error | count", AlertOperator::Gt, 10.0)
        .await
        .unwrap();
    assert!(rule.enabled);
    assert_eq!(store.count_alert_rules().await.unwrap(), 1);

    let views = store.list_alert_views().await.unwrap();
    assert_eq!(views.len(), 1);
    assert_eq!(views[0].rule.id, rule.id);
    assert_eq!(views[0].status, AlertStatus::Ok);
    assert!(views[0].value.is_none());
    assert!(views[0].last_evaluated_at.is_none());
    assert!(views[0].last_changed_at.is_some());

    assert_eq!(store.list_enabled_alert_rules().await.unwrap().len(), 1);
    store.set_alert_enabled(&rule.id, false).await.unwrap();
    assert!(store.list_enabled_alert_rules().await.unwrap().is_empty());
    assert!(!store.list_alert_views().await.unwrap()[0].rule.enabled);
    store.set_alert_enabled(&rule.id, true).await.unwrap();
    assert_eq!(store.list_enabled_alert_rules().await.unwrap().len(), 1);

    store.delete_alert_rule(&rule.id).await.unwrap();
    assert_eq!(store.count_alert_rules().await.unwrap(), 0);
    assert!(store.list_alert_views().await.unwrap().is_empty());
}

#[tokio::test]
async fn alert_state_updates() {
    let (_dir, store) = open().await;
    let rule = store
        .create_alert_rule("Errors", "level = error | count", AlertOperator::Gte, 1.0)
        .await
        .unwrap();
    let initial_changed = store.list_alert_views().await.unwrap()[0].last_changed_at;

    // Same status, not changed: last_changed_at is untouched.
    store.upsert_alert_state(&rule.id, AlertStatus::Ok, Some(0.0), None, false).await.unwrap();
    let v = &store.list_alert_views().await.unwrap()[0];
    assert_eq!(v.value, Some(0.0));
    assert!(v.last_evaluated_at.is_some());
    assert_eq!(v.last_changed_at, initial_changed);

    // Transition to firing.
    store
        .upsert_alert_state(&rule.id, AlertStatus::Firing, Some(5.0), Some("5 errors"), true)
        .await
        .unwrap();
    assert_eq!(store.get_alert_status(&rule.id).await.unwrap(), AlertStatus::Firing);
    let v = &store.list_alert_views().await.unwrap()[0];
    assert_eq!(v.status, AlertStatus::Firing);
    assert_eq!(v.message.as_deref(), Some("5 errors"));
    assert!(v.last_changed_at >= initial_changed);
}

#[tokio::test]
async fn alert_status_defaults_to_ok_for_unknown_rule() {
    let (_dir, store) = open().await;
    assert_eq!(store.get_alert_status("missing").await.unwrap(), AlertStatus::Ok);
}

#[tokio::test]
async fn alert_views_are_sorted_by_name() {
    let (_dir, store) = open().await;
    store.create_alert_rule("zeta", "| count", AlertOperator::Gt, 1.0).await.unwrap();
    store.create_alert_rule("alpha", "| count", AlertOperator::Gt, 1.0).await.unwrap();
    let names: Vec<_> = store.list_alert_views().await.unwrap().into_iter().map(|v| v.rule.name).collect();
    assert_eq!(names, ["alpha", "zeta"]);
}
