use super::AccountsPane;
use crate::{SettingsApp, SettingsShell};

pub(super) fn reviewed_nextcloud(config: serde_json::Value) -> (tempfile::TempDir, AccountsPane) {
    let root = tempfile::tempdir().unwrap();
    let account_root = crate::accounts::AccountRoot::at(root.path());
    let mut account = accounts::Account::new("cloud", "nextcloud");
    account.config = config;
    let mut store = account_root.store();
    store.upsert(account.clone());
    account_root.save_store(&store).unwrap();
    let secret = accounts::Secret::password("original-private-password");
    let mut vault = account_root.vault();
    vault.put(&account.id, &secret);
    vault.try_save().unwrap();
    let mut pane = AccountsPane::in_root(account_root);
    // Stage the same private baseline that the live discovery worker receives.
    pane.form_kind = Some(crate::accounts::AccountKind::CalDav);
    pane.form_account_id = Some(account.id.to_string());
    pane.calendar_source = Some(account.clone());
    pane.calendar_baseline = Some((Some(account), Some(secret)));
    pane.stage_calendar_choices(vec![
        tilo_dav::discovery::CalendarCollection {
            href: "https://cloud.example/cal/a/".into(),
            name: "A".into(),
            read_only: false,
        },
        tilo_dav::discovery::CalendarCollection {
            href: "https://cloud.example/cal/b/".into(),
            name: "B".into(),
            read_only: true,
        },
    ])
    .unwrap();
    (root, pane)
}

#[test]
fn new_setup_selects_all_but_an_existing_selection_is_authoritative() {
    for (config, expected) in [
        (serde_json::json!({}), [true, true]),
        (serde_json::json!({"calendars": []}), [false, false]),
        (
            serde_json::json!({"calendars": [{"id": "https://cloud.example/cal/a/"}]}),
            [true, false],
        ),
    ] {
        let (_root, pane) = reviewed_nextcloud(config);
        assert_eq!(
            pane.calendar_choices
                .iter()
                .map(|(_, selected)| *selected)
                .collect::<Vec<_>>(),
            expected
        );
    }
}

#[test]
fn changed_private_signin_account_type_target_and_scopes_cannot_commit_review() {
    for change in [
        "credential",
        "type",
        "server_url",
        "username",
        "use_files",
        "use_calendar",
        "removed",
    ] {
        let (_root, mut pane) = reviewed_nextcloud(serde_json::json!({
            "server_url": "https://cloud.example/", "username": "ada", "use_files": true, "use_calendar": false
        }));
        let root = pane.account_root.clone();
        let id = accounts::AccountId::from("cloud");
        if change == "credential" {
            let mut vault = root.vault();
            vault.put(
                &id,
                &accounts::Secret::password("replacement-private-password"),
            );
            vault.try_save().unwrap();
        } else {
            let mut store = root.store();
            let mut account = store.get(&id).unwrap().clone();
            match change {
                "type" => account.type_id = "caldav".into(),
                "removed" => {
                    store.remove(&id);
                }
                field => account.config[field] = "changed".into(),
            }
            if change != "removed" {
                store.upsert(account);
            }
            root.save_store(&store).unwrap();
        }
        let before = root.store().get(&id).cloned();
        let secret = root.vault().get(&id);
        pane.save_calendar_selection();
        pane.settle_lane();
        assert!(pane.form_error.is_some(), "{change}");
        assert_eq!(root.store().get(&id), before.as_ref(), "{change}");
        assert_eq!(root.vault().get(&id), secret, "{change}");
    }
}

#[test]
fn canceled_review_cannot_save_even_with_a_late_result() {
    let (_root, mut pane) = reviewed_nextcloud(serde_json::json!({}));
    let before = pane
        .account_root
        .store()
        .get(&accounts::AccountId::from("cloud"))
        .cloned();
    pane.cancel_form();
    assert!(
        pane.stage_calendar_choices(vec![tilo_dav::discovery::CalendarCollection {
            href: "https://cloud.example/cal/a/".into(),
            name: "A".into(),
            read_only: false
        }])
        .is_err()
    );
    pane.save_calendar_selection();
    assert_eq!(
        pane.account_root
            .store()
            .get(&accounts::AccountId::from("cloud")),
        before.as_ref()
    );
}
use test_http::{Reply, Responder};
use ui::framework::testing::TestApp;

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one journey through the review, step by step, reads best as one test"
)]
fn calendar_review_is_discoverable_and_operable_through_settings_resources() {
    tilo_test_env::require_isolated_desktop();
    let _home = tilo_test_env::TestHome::new().unwrap();
    let root = tempfile::tempdir().unwrap();
    let account_root = crate::accounts::AccountRoot::at(root.path());
    let server = Responder::start(vec![
        Reply::status(207).body(r#"<d:multistatus xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav"><d:response><d:href>/dav/</d:href><d:propstat><d:prop><c:calendar-home-set><d:href>/calendars/</d:href></c:calendar-home-set></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response></d:multistatus>"#),
        Reply::status(207).body(r#"<d:multistatus xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav"><d:response><d:href>/calendars/personal/</d:href><d:propstat><d:prop><d:resourcetype><c:calendar/></d:resourcetype><d:displayname>Personal</d:displayname></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response></d:multistatus>"#),
    ]);
    let mut account = accounts::Account::new("cloud", "nextcloud");
    account.display_name = "Cloud".into();
    account.config = serde_json::json!({"id": "cloud", "label": "Cloud", "server_url": format!("http://localhost:{}/dav/", server.port()), "username": "ada"});
    let mut store = account_root.store();
    store.upsert(account.clone());
    account_root.save_store(&store).unwrap();
    let mut vault = account_root.vault();
    vault.put(
        &account.id,
        &accounts::Secret::password("private-app-password"),
    );
    vault.try_save().unwrap();

    let mut app = TestApp::<SettingsShell>::new().unwrap();
    app.surface::<SettingsApp, _>(|settings| {
        let mut pane = crate::panes::accounts::area(settings)
            .unwrap()
            .model()
            .borrow_mut();
        pane.account_root = account_root.clone();
        pane.reload();
        pane.changed = true;
    })
    .unwrap();
    app.settle();
    app.call_on(
        "settings.account:cloud",
        "connect-nextcloud-calendars",
        serde_json::json!({}),
    )
    .unwrap();
    let href = format!("http://localhost:{}/calendars/personal/", server.port());
    let choice = format!("settings.calendar-choice:{href}");
    let deadline = ui::time::now() + std::time::Duration::from_secs(10);
    loop {
        app.settle();
        if app
            .actions()
            .unwrap()
            .iter()
            .any(|action| action.address.resource == choice)
        {
            break;
        }
        assert!(
            ui::time::now() < deadline,
            "calendar discovery did not publish its choices"
        );
        std::thread::yield_now();
    }
    let review = app
        .call_on(
            "settings.calendar-choices",
            "inspect-calendar-choices",
            serde_json::json!({}),
        )
        .unwrap()
        .unwrap();
    assert!(!review.to_string().contains("private-app-password"));
    assert_eq!(review["calendars"][0]["selected"], true);
    assert_eq!(review["checking"], false);
    // Explicitly deselect all through the published action before checking refusal.
    app.call_on(&choice, "select-calendar", serde_json::json!({}))
        .unwrap();
    assert!(
        app.call_on(
            "settings.calendar-choices",
            "save-account-form",
            serde_json::json!({})
        )
        .is_err()
    );
    assert_eq!(account_root.store().get(&account.id), Some(&account));
    app.call_on(&choice, "select-calendar", serde_json::json!({}))
        .unwrap();
    app.call_on(
        "settings.calendar-choices",
        "save-account-form",
        serde_json::json!({}),
    )
    .unwrap();
    let deadline = ui::time::now() + std::time::Duration::from_secs(10);
    while account_root.store().get(&account.id) == Some(&account) {
        app.settle();
        assert!(
            ui::time::now() < deadline,
            "calendar selection did not save"
        );
        std::thread::yield_now();
    }
    let saved = account_root.store();
    assert_eq!(saved.len(), 1);
    assert_eq!(
        saved.get(&account.id).unwrap().config["calendars"][0]["id"],
        href
    );
    assert!(app.harness().component_contract_violations().is_empty());
}
