use super::*;
use accounts::person::AccountList;
use accounts::{Account, AccountId, SecretVault};
use test_http::{Reply, Responder};

fn collection(server: &str) -> RemoteCalendar {
    RemoteCalendar {
        id: format!("{server}/cal/work/"),
        name: "Work".into(),
        color: 1,
        visible: true,
        read_only: false,
    }
}

tilo_test_env::scenario! {
    /// Setting up a Nextcloud account: discovery, and the selection that is kept or refused.
    fn nextcloud_calendars_are_discovered_and_selected;

    phase selected_nextcloud_calendars_keep_files_and_no_password;
    {
        let root = tempfile::tempdir().unwrap();
        let mut account = Account::new("canonical-nextcloud", "nextcloud");
        account.config = serde_json::json!({"server_url":"https://cloud.example/remote.php/dav/files/internal/", "username":"internal", "use_files":true, "files":{"root":"documents"}});
        let original = account.config.clone();
        let config = CalDavConfig {
            server_url: "https://cloud.example/remote.php/dav/".into(),
            username: "internal".into(),
            ..Default::default()
        };
        let calendars = vec![collection("https://cloud.example")];
        select_calendars(
            &mut account,
            &config,
            &calendars,
            &[calendars[0].id.clone()],
            false,
        )
        .unwrap();
        assert_eq!(account.config["files"], original["files"]);
        assert_eq!(account.config["server_url"], original["server_url"]);
        let mut record = accounts::person::staged_in(root.path()).unwrap();
        record
            .apply_confirmed(AccountList::saving(&account))
            .unwrap();
        accounts::person::keep_staged_in(root.path(), &mut record).unwrap();
        let saved = record.state().unwrap();
        assert_eq!(saved.all().len(), 1);
        assert_eq!(
            saved.account(account.id.as_str()).unwrap().type_id.as_str(),
            "nextcloud"
        );
        assert_eq!(
            saved.account(account.id.as_str()).unwrap().config["files"],
            original["files"]
        );
        let projection = crate::CalendarAccounts::from_shared(&saved);
        assert_eq!(projection.accounts.len(), 1);
        assert_eq!(projection.accounts[0].id, "canonical-nextcloud");
        assert_eq!(
            projection.accounts[0].config.password, "",
            "a calendar's password is the workspace's vault's, never this device's"
        );
        assert!(
            SecretVault::open_in(root.path()).get(&account.id).is_none(),
            "a calendar's password is the workspace's vault's, never this device's"
        );
    }

    phase an_account_connected_for_reading_only_stays_so_when_its_calendars_are_chosen_again;
    {
        let config = CalDavConfig {
            server_url: "https://cloud.example/remote.php/dav/".into(),
            username: "internal".into(),
            ..Default::default()
        };
        // Its server would let the person write this calendar.
        let calendars = vec![collection("https://cloud.example")];
        let mut account = Account::new("canonical-nextcloud", "nextcloud");
        account.config = serde_json::json!({
            "server_url": "https://cloud.example/remote.php/dav/files/internal/",
            "username": "internal",
            "use_files": true,
            "calendar": {
                "server_url": "https://cloud.example/remote.php/dav/",
                "username": "internal",
                "read_only": true,
            },
        });
        select_calendars(
            &mut account,
            &config,
            &calendars,
            &[calendars[0].id.clone()],
            false,
        )
        .unwrap();
        assert_eq!(
            account.config["calendar"]["read_only"], true,
            "the account keeps its word: {}",
            account.config
        );
        assert_eq!(
            account.config["calendar"]["calendars"][0]["read_only"], true,
            "no calendar of it is taken as one that can be written: {}",
            account.config
        );
        let mut shared = AccountList::default();
        shared.keep(&account).unwrap();
        let projection = crate::CalendarAccounts::from_shared(&shared);
        assert_eq!(projection.accounts.len(), 1);
        assert!(projection.accounts[0].read_only);
        assert!(projection.accounts[0].calendars[0].read_only);

        // An account that is written to is chosen as its server answers.
        let mut writing = Account::new("another-nextcloud", "nextcloud");
        writing.config = serde_json::json!({
            "server_url": "https://cloud.example/remote.php/dav/files/internal/",
            "username": "internal",
            "use_files": true,
        });
        select_calendars(
            &mut writing,
            &config,
            &calendars,
            &[calendars[0].id.clone()],
            false,
        )
        .unwrap();
        assert!(writing.config["calendar"].get("read_only").is_none());
        assert_eq!(writing.config["calendar"]["calendars"][0]["read_only"], false);
    }

    phase selection_rejects_unconfirmed_duplicate_and_cross_origin_urls_without_mutation;
    {
        let mut account = Account::new("a", "nextcloud");
        account.config = serde_json::json!({"use_files":true});
        let before = account.clone();
        let config = CalDavConfig {
            server_url: "https://cloud.example/".into(),
            ..Default::default()
        };
        let valid = collection("https://cloud.example");
        for selected in [
            vec![],
            vec!["unknown".into()],
            vec![valid.id.clone(), valid.id.clone()],
        ] {
            assert!(
                select_calendars(
                    &mut account,
                    &config,
                    std::slice::from_ref(&valid),
                    &selected,
                    false
                )
                .is_err()
            );
            assert_eq!(account, before);
        }
        let hostile = collection("https://attacker.example");
        assert!(
            select_calendars(
                &mut account,
                &config,
                std::slice::from_ref(&hostile),
                std::slice::from_ref(&hostile.id),
                false
            )
            .is_err()
        );
        assert_eq!(account, before);
    }

}

tilo_test_env::scenario! {
    /// Setting up a Google account: the grant it syncs with and what it never writes.
    fn google_calendars_sync_through_the_canonical_grant_and_keep_tokens_out;

    phase google_refreshes_canonical_grant_and_sends_bearer_report_and_guarded_writes;
    {
        let root = tempfile::tempdir().unwrap();
        let token_server = Responder::start(vec![Reply::status(200).header("Content-Type", "application/json").body(r#"{"access_token":"renewed-calendar","token_type":"Bearer","expires_in":3600,"refresh_token":"rotated-refresh"}"#)]);
        let mut provider = account_credentials::OAuthIssuer::Google.endpoints();
        provider.token_endpoint = format!("{}/token", token_server.url());
        let credential_store = account_credentials::CredentialStore::for_root(Some(root.path().to_owned()))
            .with_issuer_endpoints(account_credentials::OAuthIssuer::Google, provider);
        let id = AccountId::from("canonical-google");
        credential_store
            .seal_oauth(
                &id,
                &account_credentials::TokenSet {
                    access_token: "expired".into(),
                    refresh_token: "old-refresh".into(),
                    obtained_at: 1,
                    expires_in: 1,
                    scope: Some("https://www.googleapis.com/auth/calendar".into()),
                },
            )
            .unwrap();
        let server = Responder::start(vec![
            Reply::status(207).body("<d:multistatus xmlns:d=\"DAV:\"/>"),
            Reply::status(412),
            Reply::status(204),
        ]);
        let config = CalDavConfig {
            server_url: server.url(),
            ..Default::default()
        };
        let service = tilo_dav::oauth::Service::for_test_origin(credential_store, &server.url()).unwrap();
        let url = format!("{}/cal/", server.url());
        let client = service
            .authorised(
                Some(&Account::new(id.clone(), accounts::cloud_types::GOOGLE_DRIVE)),
                &config.server_url,
                "",
                "",
                &url,
            )
            .unwrap();
        let run = |future| ui::rt::block_on(future).expect("test thread may block");
        assert_eq!(
            run(client.report(&url, tilo_dav::Depth::One, "<report/>".into()))
                .unwrap()
                .status()
                .as_u16(),
            207
        );
        assert_eq!(
            ui::rt::block_on(client.put(
                &url,
                b"ics".to_vec(),
                Some("text/calendar"),
                tilo_dav::Precondition::IfMatch("\"v1\"")
            ))
            .unwrap()
            .unwrap()
            .status()
            .as_u16(),
            412
        );
        ui::rt::block_on(client.delete(&url, tilo_dav::Precondition::IfMatch("\"v2\"")))
            .unwrap()
            .unwrap();
        for (method, etag) in [
            ("REPORT", None),
            ("PUT", Some("\"v1\"")),
            ("DELETE", Some("\"v2\"")),
        ] {
            let request = server.next_request();
            assert_eq!(request.method, method);
            assert_eq!(
                request.header("authorization"),
                Some("Bearer renewed-calendar")
            );
            assert_eq!(request.header("if-match"), etag);
        }
        let request = token_server.next_request();
        assert!(request.body_text().contains("refresh_token=old-refresh"));
        assert!(request.body_text().contains("grant_type=refresh_token"));
        let stored = SecretVault::open_in(root.path()).get(&id).unwrap();
        assert!(
            !serde_json::to_string(&stored)
                .unwrap()
                .contains("old-refresh")
        );
        assert!(
            serde_json::to_string(&stored)
                .unwrap()
                .contains("rotated-refresh")
        );
    }

    phase google_collection_id_cannot_change_the_url_path_or_query;
    {
        let config = google_config("ada+calendar /?#%@example.org");
        assert_eq!(
            config.server_url,
            "https://apidata.googleusercontent.com/caldav/v2/ada%2Bcalendar%20%2F%3F%23%25@example.org/user"
        );
    }

    phase google_projection_uses_original_id_and_never_serializes_tokens;
    {
        let mut account = Account::new("drive-account", "google-drive");
        account.config = serde_json::json!({"use_files":true});
        let config = google_config("ada@example.org");
        assert_eq!(
            config.server_url,
            "https://apidata.googleusercontent.com/caldav/v2/ada@example.org/user"
        );
        let calendar = RemoteCalendar {
            id: config.server_url.clone(),
            ..collection("https://unused.example")
        };
        select_calendars(
            &mut account,
            &config,
            std::slice::from_ref(&calendar),
            std::slice::from_ref(&calendar.id),
            true,
        )
        .unwrap();
        let root = tempfile::tempdir().unwrap();
        let mut record = accounts::person::staged_in(root.path()).unwrap();
        record
            .apply_confirmed(AccountList::saving(&account))
            .unwrap();
        accounts::person::keep_staged_in(root.path(), &mut record).unwrap();
        let credential_store = account_credentials::CredentialStore::for_root(Some(root.path().to_owned()));
        credential_store
            .seal_oauth(
                &AccountId::from("drive-account"),
                &account_credentials::TokenSet {
                    access_token: "approved-google-access".into(),
                    refresh_token: "canonical-refresh".into(),
                    obtained_at: 1,
                    expires_in: 3600,
                    scope: None,
                },
            )
            .unwrap();
        let vault = SecretVault::open_in(root.path());
        let before = vault.get(&AccountId::from("drive-account")).unwrap();
        let shared = record.state().unwrap();
        let projection = crate::CalendarAccounts::from_shared(&shared);
        assert_eq!(
            projection.accounts[0].config.oauth_account_id.as_deref(),
            Some("drive-account")
        );
        assert!(projection.accounts[0].config.password.is_empty());
        assert_eq!(
            SecretVault::open_in(root.path()).get(&AccountId::from("drive-account")),
            Some(before)
        );
        let plaintext = String::from_utf8_lossy(
            &std::fs::read(accounts::person::staged_path_in(root.path())).unwrap(),
        )
        .into_owned();
        assert!(!plaintext.contains("approved-google-access"));
        assert!(!plaintext.contains("canonical-refresh"));
    }
}
