//! Real local DAV discovery → explicit selection → canonical store → Calendar.
use settings::accounts::{AccountKind, AccountRoot};
use settings::panes::accounts::{AccountViewCommand, AccountsPane};
use test_http::{Reply, Responder};

fn response(href: &str, props: &str) -> String {
    format!(
        r#"<d:multistatus xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav"><d:response><d:href>{href}</d:href><d:propstat><d:prop>{props}</d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response></d:multistatus>"#
    )
}

#[test]
fn actual_discovery_requires_selection_and_calendar_reads_the_saved_collections() {
    let principal = response(
        "/cloud/dav/",
        "<d:current-user-principal><d:href>/cloud/principals/internal-id/</d:href></d:current-user-principal>",
    );
    let home = response(
        "/cloud/principals/internal-id/",
        "<c:calendar-home-set><d:href>/cloud/calendars/internal-id/</d:href></c:calendar-home-set>",
    );
    let listing = r#"<d:multistatus xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav">
        <d:response><d:href>/cloud/calendars/internal-id/team/</d:href><d:propstat><d:prop><d:resourcetype><d:collection/><c:calendar/></d:resourcetype><d:displayname>Team</d:displayname><d:current-user-privilege-set><d:privilege><d:read/></d:privilege><d:privilege><d:write/></d:privilege></d:current-user-privilege-set></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>
        <d:response><d:href>/cloud/calendars/internal-id/holidays/</d:href><d:propstat><d:prop><d:resourcetype><c:calendar/></d:resourcetype><d:displayname>Holidays</d:displayname><d:current-user-privilege-set><d:privilege><d:read/></d:privilege></d:current-user-privilege-set></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>
    </d:multistatus>"#;
    let server = Responder::start(vec![
        Reply::status(207).body(principal),
        Reply::status(207).body(home),
        Reply::status(207).body(listing),
    ]);
    let root = tempfile::tempdir().unwrap();
    let account_root = AccountRoot::at(root.path());
    let mut pane = AccountsPane::in_root(account_root.clone());
    pane.command(AccountViewCommand::Add(AccountKind::CalDav));
    // The responder listens on 127.0.0.1 only. A `localhost` URL would let
    // the client try [::1] first, where another test's server may hold the
    // same port number and answer instead.
    let server_url = format!("http://127.0.0.1:{}/cloud/dav/", server.port());
    pane.set_form_fields_for_test("Cloud calendars", "", "login@example.test", &server_url);
    pane.set_form_password_for_test("private-app-password");
    pane.submit_form_for_test();
    assert!(
        pane.finish_form_submission_for_test(tilo_test_env::hang_bound()),
        "discovery completed"
    );
    assert!(
        pane.form_open(),
        "discovery is a review, not a saved account"
    );
    assert!(account_root.store().is_empty());
    assert!(account_root.vault().is_empty());
    assert!(
        pane.select_calendar("https://other.example/calendar", true)
            .is_err()
    );
    let team = format!(
        "http://127.0.0.1:{}/cloud/calendars/internal-id/team/",
        server.port()
    );
    let holidays = format!(
        "http://127.0.0.1:{}/cloud/calendars/internal-id/holidays/",
        server.port()
    );
    pane.select_calendar(&team, false).unwrap();
    pane.select_calendar(&holidays, false).unwrap();
    pane.submit_form_for_test();
    assert!(
        account_root.store().is_empty(),
        "explicitly deselecting every calendar cannot report success"
    );
    pane.finish_form_submission_for_test(tilo_test_env::hang_bound());
    pane.select_calendar(&holidays, true).unwrap();
    pane.submit_form_for_test();
    assert!(pane.finish_form_submission_for_test(tilo_test_env::hang_bound()));
    assert!(!pane.form_open());
    let loaded = calendar::account::AccountStore::load_from_settings_in(root.path());
    assert_eq!(loaded.len(), 1);
    let account = &loaded.accounts[0];
    assert_eq!(account.config.username, "login@example.test");
    assert_eq!(account.config.password, "private-app-password");
    assert_eq!(
        account.calendars.len(),
        1,
        "deselected calendars are not sync targets"
    );
    assert_eq!(account.calendars[0].id, holidays);
    assert_eq!(account.calendars[0].name, "Holidays");
    assert!(account.calendars[0].read_only);
    let json = std::fs::read_to_string(accounts::store_path_in(root.path())).unwrap();
    assert!(!json.contains("private-app-password"));
    assert_eq!(
        account_root.store().len(),
        1,
        "no duplicate Nextcloud account is created"
    );
    for path in [
        "/cloud/dav/",
        "/cloud/principals/internal-id/",
        "/cloud/calendars/internal-id/",
    ] {
        let request = server.next_request();
        assert_eq!(request.target, path);
        assert_eq!(request.method, "PROPFIND");
        assert!(request.header("authorization").is_some());
    }
}

#[test]
fn nextcloud_calendars_reuse_the_canonical_account_and_never_rewrite_its_secret() {
    for use_files in [true, false] {
        let server = Responder::start(vec![
        Reply::status(207).body(response("/cloud/", "<d:current-user-principal><d:href>/cloud/principals/opaque-id/</d:href></d:current-user-principal>")),
        Reply::status(207).body(response("/cloud/principals/opaque-id/", "<c:calendar-home-set><d:href>/cloud/calendars/opaque-id/</d:href></c:calendar-home-set>")),
        Reply::status(207).body(response("/cloud/calendars/opaque-id/personal/", "<d:resourcetype><c:calendar/></d:resourcetype><d:displayname>Personal</d:displayname>")),
    ]);
        let root = tempfile::tempdir().unwrap();
        let account_root = AccountRoot::at(root.path());
        let mut account = accounts::Account::new("canonical-nextcloud", "nextcloud");
        account.display_name = "My cloud".into();
        account.config = serde_json::json!({
            "id": "canonical-nextcloud", "label": "My cloud",
            "server_url": format!("http://127.0.0.1:{}/cloud/", server.port()),
            "username": "different-login@example.test", "use_files": use_files,
            "use_calendar": false, "use_mail": false, "future_setting": "keep"
        });
        let mut shared = account_root.store();
        shared.upsert(account.clone());
        account_root.save_store(&shared).unwrap();
        let mut vault = account_root.vault();
        vault.put(
            &account.id,
            &accounts::Secret::password("existing-nextcloud-password"),
        );
        vault.try_save().unwrap();
        let vault_path = accounts::vault_dir_in(root.path()).join("vault.enc");
        let ciphertext = std::fs::read(&vault_path).unwrap();
        let mut pane = AccountsPane::in_root(account_root.clone());
        pane.connect_nextcloud_calendars(account.id.as_str())
            .unwrap();
        assert!(pane.finish_form_submission_for_test(tilo_test_env::hang_bound()));
        assert_eq!(account_root.store().get(&account.id), Some(&account));
        let href = format!(
            "http://127.0.0.1:{}/cloud/calendars/opaque-id/personal/",
            server.port()
        );
        pane.submit_form_for_test();
        assert!(pane.finish_form_submission_for_test(tilo_test_env::hang_bound()));
        assert!(!pane.form_open());
        let stored = account_root.store();
        assert_eq!(stored.len(), 1);
        let saved = stored.get(&account.id).unwrap();
        assert_eq!(saved.type_id.as_str(), "nextcloud");
        assert_eq!(saved.config["use_files"], use_files);
        assert_eq!(saved.config["use_mail"], false);
        assert_eq!(saved.config["use_calendar"], true);
        assert_eq!(saved.config["future_setting"], "keep");
        assert_eq!(
            std::fs::read(&vault_path).unwrap(),
            ciphertext,
            "calendar setup must not rewrite or copy the canonical sign-in"
        );
        let loaded = calendar::account::AccountStore::load_from_settings_in(root.path());
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded.accounts[0].id, account.id.as_str());
        assert_eq!(loaded.accounts[0].config.display_name, "My cloud");
        assert_eq!(
            loaded.accounts[0].config.password,
            "existing-nextcloud-password"
        );
        assert_eq!(loaded.accounts[0].calendars[0].id, href);
        loaded.persist_in(root.path()).unwrap();
        let round_trip = account_root.store();
        let round_trip = round_trip.get(&account.id).unwrap();
        assert_eq!(round_trip.config["use_files"], use_files);
        assert_eq!(round_trip.config["username"], account.config["username"]);
        assert_eq!(
            round_trip.config["server_url"],
            account.config["server_url"]
        );
        assert_eq!(round_trip.config["future_setting"], "keep");
        assert_eq!(
            account_root
                .store()
                .get(&account.id)
                .unwrap()
                .type_id
                .as_str(),
            "nextcloud"
        );
        assert_eq!(std::fs::read(&vault_path).unwrap(), ciphertext);
        assert!(
            !std::fs::read_to_string(accounts::store_path_in(root.path()))
                .unwrap()
                .contains("existing-nextcloud-password")
        );
    }
}

#[test]
fn changing_or_canceling_a_live_discovery_draft_never_commits_it() {
    for change in [
        "before-result",
        "after-result",
        "password",
        "cancel",
        "switch",
        "cancel-before-result",
        "switch-before-result",
    ] {
        let server = Responder::start(vec![
            Reply::status(207).body(response(
                "/cal/",
                "<c:calendar-home-set><d:href>/cal/</d:href></c:calendar-home-set>",
            )),
            Reply::status(207).body(response(
                "/cal/a/",
                "<d:resourcetype><c:calendar/></d:resourcetype>",
            )),
        ]);
        let root = tempfile::tempdir().unwrap();
        let account_root = AccountRoot::at(root.path());
        let mut pane = AccountsPane::in_root(account_root.clone());
        pane.command(AccountViewCommand::Add(AccountKind::CalDav));
        let url = format!("http://127.0.0.1:{}/cal/", server.port());
        pane.set_form_fields_for_test("Cloud", "", "ada", &url);
        pane.set_form_password_for_test("candidate-password");
        pane.submit_form_for_test();
        if !change.ends_with("before-result") {
            assert!(pane.finish_form_submission_for_test(tilo_test_env::hang_bound()));
        }
        match change {
            "password" => pane.set_form_password_for_test("other-password"),
            "cancel" | "cancel-before-result" => pane.command(AccountViewCommand::Cancel),
            "switch" | "switch-before-result" => {
                pane.command(AccountViewCommand::Add(AccountKind::WebDav));
            }
            _ => pane.set_form_fields_for_test("Cloud", "", "different-user", &url),
        }
        if change == "before-result" {
            assert!(!pane.finish_form_submission_for_test(tilo_test_env::hang_bound()));
        } else {
            // Polling a canceled/replaced form cannot consume the old receiver.
            pane.finish_form_submission_for_test(tilo_test_env::hang_bound());
            pane.submit_form_for_test();
        }
        assert!(account_root.store().is_empty(), "{change}");
        assert!(account_root.vault().is_empty(), "{change}");
    }
}

#[test]
fn failed_or_empty_discovery_never_saves_an_account_or_credential() {
    let home = response("/cal/", "<d:resourcetype><d:collection/></d:resourcetype>");
    for replies in [
        vec![Reply::status(401)],
        vec![Reply::status(207).body("<broken")],
        vec![
            Reply::status(207).body(home.clone()),
            Reply::status(207).body(home),
        ],
    ] {
        let server = Responder::start(replies);
        let root = tempfile::tempdir().unwrap();
        let account_root = AccountRoot::at(root.path());
        let mut pane = AccountsPane::in_root(account_root.clone());
        pane.command(AccountViewCommand::Add(AccountKind::CalDav));
        pane.set_form_fields_for_test(
            "Cloud",
            "",
            "ada",
            &format!("http://127.0.0.1:{}/cal/", server.port()),
        );
        pane.set_form_password_for_test("candidate-password");
        pane.submit_form_for_test();
        assert!(!pane.finish_form_submission_for_test(tilo_test_env::hang_bound()));
        assert!(pane.form_open());
        assert!(account_root.store().is_empty());
        assert!(account_root.vault().is_empty());
    }
}
