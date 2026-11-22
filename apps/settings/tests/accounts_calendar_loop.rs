//! Local DAV discovery, an explicit selection, the canonical store, Calendar.
use settings::accounts::{AccountKind, AccountRoot};
use settings::panes::accounts::{AccountViewCommand, AccountsPane};
use test_http::{Reply, Responder};

type Window = ui::framework::testing::TestApp<settings::SettingsShell>;

fn response(href: &str, props: &str) -> String {
    format!(
        r#"<d:multistatus xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav"><d:response><d:href>{href}</d:href><d:propstat><d:prop>{props}</d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response></d:multistatus>"#
    )
}

/// An accounts root at `config_root` over a new workspace's accounts record,
/// the one a Settings window opens, with the window that keeps it.
fn scratch_root(config_root: &std::path::Path) -> Result<(Window, AccountRoot), String> {
    let mut window = Window::new().map_err(|error| error.to_string())?;
    let record = window
        .shared_workspace::<accounts::person::AccountList>(accounts::person::ACCOUNTS_OWNER)
        .ok_or("Settings keeps the workspace's accounts")?;
    Ok((window, AccountRoot::at(config_root).with_person(record)))
}

/// The vault entry the test's session keeps an account's password in.
const ITEM: &str = "vault.item:0123456789abcdef0123456789abcdef";

/// What Settings asked the session for: each password it kept, as (account, password, username,
/// servers), and each entry whose secret it was released.
#[derive(Default)]
struct Asked {
    kept: Vec<(String, String, String, Vec<String>)>,
    released: Vec<String>,
}

/// The session as the test answers it: it keeps a typed password in the workspace's vault under
/// [`ITEM`], and releases `secret` for that entry, writing down what it was asked.
fn session(
    secret: &'static str,
) -> (
    ui::collaboration::browser_service_sessions::Client,
    std::sync::Arc<std::sync::Mutex<Asked>>,
) {
    use ui::collaboration::browser_service_sessions::{
        BrowserServiceSessionCommand as Command, BrowserServiceSessionReply as Reply, Client,
    };
    let asked = std::sync::Arc::new(std::sync::Mutex::new(Asked::default()));
    let log = std::sync::Arc::clone(&asked);
    let client = Client::answering(move |command| match command {
        Command::StoreAccountSecret {
            account,
            password,
            username,
            servers,
        } => {
            log.lock().unwrap().kept.push((
                account.clone(),
                password.as_str().to_owned(),
                username,
                servers,
            ));
            Ok(Reply::AccountSecretStored {
                account,
                item: ITEM.into(),
            })
        }
        Command::ReleaseAccountSecret { item } => {
            log.lock().unwrap().released.push(item.clone());
            Ok(Reply::AccountSecret {
                item,
                secret: secret.to_owned().into(),
            })
        }
        other => Err(format!("the session was asked {other:?}")),
    });
    (client, asked)
}

tilo_test_env::scenario! {
    fn calendar_discovery_saves_only_what_was_selected;

    phase actual_discovery_requires_selection_and_calendar_reads_the_saved_collections;
    {
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
        let (_window, account_root) = scratch_root(root.path()).unwrap();
        let (session, asked) = session("");
        let mut pane = AccountsPane::in_root(account_root.clone()).with_session_secrets(session);
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
        let loaded = calendar::account::CalendarAccounts::from_shared(&account_root.store());
        assert_eq!(loaded.len(), 1);
        let account = &loaded.accounts[0];
        assert_eq!(account.config.username, "login@example.test");
        // The password is the workspace's vault's, typed once into Settings and kept through
        // the session: the account names its entry, and this device keeps nothing of it.
        let kept = std::mem::take(&mut asked.lock().unwrap().kept);
        assert_eq!(kept.len(), 1, "the password is kept once: {:?}", kept.iter().map(|k| &k.0).collect::<Vec<_>>());
        assert_eq!(kept[0].1, "private-app-password");
        assert_eq!(kept[0].2, "login@example.test");
        assert_eq!(account.config.password_item.as_deref(), Some(ITEM));
        assert!(account.config.password.is_empty(), "Calendar holds no password of its own");
        assert!(account_root.vault().is_empty(), "the device's vault keeps nothing");
        assert_eq!(
            account.calendars.len(),
            1,
            "deselected calendars are not sync targets"
        );
        assert_eq!(account.calendars[0].id, holidays);
        assert_eq!(account.calendars[0].name, "Holidays");
        assert!(account.calendars[0].read_only);
        assert!(
            !serde_json::to_string(&account_root.store().all())
                .unwrap()
                .contains("private-app-password"),
            "a sign-in never reaches the workspace's accounts"
        );
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

    phase nextcloud_calendars_reuse_the_canonical_account_and_never_rewrite_its_secret;
    {
        for use_files in [true, false] {
            let server = Responder::start(vec![
            Reply::status(207).body(response("/cloud/", "<d:current-user-principal><d:href>/cloud/principals/opaque-id/</d:href></d:current-user-principal>")),
            Reply::status(207).body(response("/cloud/principals/opaque-id/", "<c:calendar-home-set><d:href>/cloud/calendars/opaque-id/</d:href></c:calendar-home-set>")),
            Reply::status(207).body(response("/cloud/calendars/opaque-id/personal/", "<d:resourcetype><c:calendar/></d:resourcetype><d:displayname>Personal</d:displayname>")),
        ]);
            let root = tempfile::tempdir().unwrap();
            let (_window, account_root) = scratch_root(root.path()).unwrap();
            let mut account = accounts::Account::new("canonical-nextcloud", "nextcloud");
            account.display_name = "My cloud".into();
            account.config = serde_json::json!({
                "id": "canonical-nextcloud", "label": "My cloud",
                "server_url": format!("http://127.0.0.1:{}/cloud/", server.port()),
                "username": "different-login@example.test", "use_files": use_files,
                "use_calendar": false, "use_mail": false, "future_setting": "keep"
            });
            // Its sign-in is the workspace's vault's entry, which the session releases.
            accounts::email::set_password_item(&mut account.config, Some(ITEM));
            let mut shared = account_root.store();
            shared.keep(&account).unwrap();
            account_root.save_store(&shared).unwrap();
            let (session, asked) = session("existing-nextcloud-password");
            let mut pane =
                AccountsPane::in_root(account_root.clone()).with_session_secrets(session);
            pane.connect_nextcloud_calendars(account.id.as_str())
                .unwrap();
            assert!(pane.finish_form_submission_for_test(tilo_test_env::hang_bound()));
            assert_eq!(
                account_root.store().get(&account.id),
                Some(account.clone())
            );
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
            let Asked { kept, released } = std::mem::take(&mut *asked.lock().unwrap());
            assert_eq!(released, [ITEM], "the calendars are found with its sign-in");
            assert!(
                kept.is_empty(),
                "calendar setup must not rewrite or copy the canonical sign-in"
            );
            assert!(account_root.vault().is_empty(), "the device's vault keeps nothing");
            let loaded = calendar::account::CalendarAccounts::from_shared(&account_root.store());
            assert_eq!(loaded.len(), 1);
            assert_eq!(loaded.accounts[0].id, account.id.as_str());
            assert_eq!(loaded.accounts[0].config.display_name, "My cloud");
            assert_eq!(loaded.accounts[0].config.password_item.as_deref(), Some(ITEM));
            assert_eq!(loaded.accounts[0].calendars[0].id, href);
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
            assert!(account_root.vault().is_empty());
            assert!(
                !serde_json::to_string(&account_root.store().all())
                    .unwrap()
                    .contains("existing-nextcloud-password"),
                "a sign-in never reaches the workspace's accounts"
            );
        }
    }

    phase changing_or_canceling_a_live_discovery_draft_never_commits_it;
    {
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
            let (_window, account_root) = scratch_root(root.path()).unwrap();
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
                pane.finish_form_submission_for_test(tilo_test_env::hang_bound());
                pane.submit_form_for_test();
            }
            assert!(account_root.store().is_empty(), "{change}");
            assert!(account_root.vault().is_empty(), "{change}");
        }
    }

    phase failed_or_empty_discovery_never_saves_an_account_or_credential;
    {
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
            let (_window, account_root) = scratch_root(root.path()).unwrap();
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
}
