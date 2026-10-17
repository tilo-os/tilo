use super::*;
use accounts::{Account, AccountId, AccountStore, Secret, SecretVault};
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

#[test]
fn selected_nextcloud_calendars_keep_files_and_adopted_credentials() {
    let root = tempfile::tempdir().unwrap();
    let mut account = Account::new("canonical-nextcloud", "nextcloud");
    account.config = serde_json::json!({"server_url":"https://cloud.example/remote.php/dav/files/internal/", "username":"internal", "use_files":true, "files":{"root":"documents"}});
    let original = account.config.clone();
    let secret = account_custody::copied_secret(&account, &Secret::password("adopted-password"));
    let mut vault = SecretVault::open_in(root.path());
    vault.put(&account.id, &secret);
    vault.save();
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
    let mut shared = AccountStore::new();
    shared.upsert(account.clone());
    shared.save_in(root.path()).unwrap();
    let mut projection = crate::AccountStore::from_store(&shared, &vault);
    assert_eq!(projection.accounts.len(), 1);
    assert_eq!(projection.accounts[0].id, "canonical-nextcloud");
    assert_eq!(projection.accounts[0].config.password, "adopted-password");
    projection.accounts[0].calendars[0].visible = false;
    projection.persist_in(root.path()).unwrap();
    let saved = AccountStore::load_in(root.path());
    assert_eq!(saved.all().len(), 1);
    assert_eq!(
        saved.get(&account.id).unwrap().type_id.as_str(),
        "nextcloud"
    );
    assert_eq!(
        saved.get(&account.id).unwrap().config["files"],
        original["files"]
    );
    assert_eq!(
        SecretVault::open_in(root.path()).get(&account.id),
        Some(secret)
    );
}

#[test]
fn selection_rejects_unconfirmed_duplicate_and_cross_origin_urls_without_mutation() {
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

#[test]
fn google_refreshes_canonical_grant_and_sends_bearer_report_and_guarded_writes() {
    let root = tempfile::tempdir().unwrap();
    let token_server = Responder::start(vec![Reply::status(200).header("Content-Type", "application/json").body(r#"{"access_token":"renewed-calendar","token_type":"Bearer","expires_in":3600,"refresh_token":"rotated-refresh"}"#)]);
    let mut provider = account_custody::OAuthIssuer::Google.endpoints();
    provider.token_endpoint = format!("{}/token", token_server.url());
    let custodian = account_custody::Custodian::for_root(Some(root.path().to_owned()))
        .with_issuer_endpoints(account_custody::OAuthIssuer::Google, provider);
    let id = AccountId::from("canonical-google");
    custodian
        .seal_oauth(
            &id,
            &account_custody::TokenSet {
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
    let mut store = AccountStore::new();
    store.upsert(Account::new(id.clone(), "google-drive"));
    store.save_in(root.path()).unwrap();
    let service = tilo_dav::oauth::Service::for_test_origin(custodian, &server.url()).unwrap();
    let url = format!("{}/cal/", server.url());
    let client = service
        .authorised(Some(id.as_str()), &config.server_url, "", "", &url)
        .unwrap();
    let run = |future| ui::rt::block_on(future).expect("test thread may block");
    // A single opaque capability can never be redirected to a foreign origin.
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

#[test]
fn nextcloud_discovery_uses_adopted_canonical_password_and_server_owned_paths() {
    let xml = |href: &str, properties: &str| {
        format!(
            r#"<d:multistatus xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav"><d:response><d:href>{href}</d:href><d:propstat><d:prop>{properties}</d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response></d:multistatus>"#
        )
    };
    let server = Responder::start(vec![
        Reply::status(207).body(xml("/dav/", "<d:current-user-principal><d:href>/principals/opaque/</d:href></d:current-user-principal>")),
        Reply::status(207).body(xml("/principals/opaque/", "<c:calendar-home-set><d:href>/calendars/opaque/</d:href></c:calendar-home-set>")),
        Reply::status(207).body(xml("/calendars/opaque/team/", "<d:resourcetype><d:collection/><c:calendar/></d:resourcetype><d:displayname>Team</d:displayname><d:current-user-privilege-set><d:privilege><d:read/></d:privilege></d:current-user-privilege-set>")),
    ]);
    let root = tempfile::tempdir().unwrap();
    let account = Account::new("canonical", "nextcloud");
    let secret = account_custody::copied_secret(&account, &Secret::password("adopted"));
    let mut vault = SecretVault::open_in(root.path());
    vault.put(&account.id, &secret);
    vault.save();
    let config = CalDavConfig {
        server_url: format!("{}/dav/", server.url()),
        username: "login".into(),
        ..Default::default()
    };
    let custodian = account_custody::Custodian::for_root(Some(root.path().to_owned()));
    let mut store = AccountStore::new();
    store.upsert(account.clone());
    store.save_in(root.path()).unwrap();
    let service = tilo_dav::oauth::Service::new(custodian);
    let found = discover_calendars(&service, &account, &config, false).unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(
        found[0].id,
        format!("{}/calendars/opaque/team/", server.url())
    );
    assert!(found[0].read_only);
    for path in ["/dav/", "/principals/opaque/", "/calendars/opaque/"] {
        let request = server.next_request();
        assert_eq!(request.method, "PROPFIND");
        assert_eq!(request.target, path);
        assert_eq!(
            request.header("authorization"),
            Some("Basic bG9naW46YWRvcHRlZA==")
        );
    }
    assert_eq!(
        SecretVault::open_in(root.path()).get(&account.id),
        Some(secret)
    );
}

#[test]
fn google_collection_id_cannot_change_the_url_path_or_query() {
    let config = google_config("ada+calendar /?#%@example.org");
    assert_eq!(
        config.server_url,
        "https://apidata.googleusercontent.com/caldav/v2/ada%2Bcalendar%20%2F%3F%23%25@example.org/user"
    );
}

#[test]
fn google_projection_uses_original_id_and_never_serializes_tokens() {
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
    let mut shared = AccountStore::new();
    shared.upsert(account);
    let root = tempfile::tempdir().unwrap();
    shared.save_in(root.path()).unwrap();
    let custodian = account_custody::Custodian::for_root(Some(root.path().to_owned()));
    custodian
        .seal_oauth(
            &AccountId::from("drive-account"),
            &account_custody::TokenSet {
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
    let projection = crate::AccountStore::from_store(&shared, &vault);
    assert_eq!(
        projection.accounts[0].config.oauth_account_id.as_deref(),
        Some("drive-account")
    );
    assert!(projection.accounts[0].config.password.is_empty());
    projection.persist_in(root.path()).unwrap();
    assert_eq!(AccountStore::load_in(root.path()).all().len(), 1);
    assert_eq!(
        SecretVault::open_in(root.path()).get(&AccountId::from("drive-account")),
        Some(before)
    );
    let plaintext = std::fs::read_to_string(accounts::store_path_in(root.path())).unwrap();
    assert!(!plaintext.contains("approved-google-access"));
    assert!(!plaintext.contains("canonical-refresh"));
}
