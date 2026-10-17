use super::*;
use test_http::{Reply, Responder};

fn run<T>(future: impl Future<Output = T>) -> T {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(future)
}

#[test]
fn every_operation_rejects_another_origin_before_sending_credentials() {
    let root = tempfile::tempdir().unwrap();
    let service = Service::new(Custodian::for_root(Some(root.path().to_owned())));
    let trusted = Responder::start(vec![Reply::status(204)]);
    let attacker = Responder::silent();
    let client = service
        .authorised(None, &trusted.url(), "ada", "secret", "/cal/")
        .unwrap();
    run(async {
        assert!(
            client
                .report(&attacker.url(), Depth::One, "<report/>".into())
                .await
                .is_err()
        );
        assert!(
            client
                .propfind(&attacker.url(), Depth::Zero, &[])
                .await
                .is_err()
        );
        assert!(
            client
                .put(&attacker.url(), vec![], None, Precondition::None)
                .await
                .is_err()
        );
        assert!(
            client
                .delete(&attacker.url(), Precondition::None)
                .await
                .is_err()
        );
        assert_eq!(
            client
                .delete("/cal/one.ics", Precondition::IfMatch("\"v1\""))
                .await
                .unwrap()
                .status()
                .as_u16(),
            204
        );
    });
    let request = trusted.next_request();
    assert_eq!(request.target, "/cal/one.ics");
    assert_eq!(
        request.header("authorization"),
        Some("Basic YWRhOnNlY3JldA==")
    );
    assert_eq!(request.header("if-match"), Some("\"v1\""));
    assert_eq!(request.header("user-agent"), Some("Tilo-Calendar/0.1"));
    attacker.assert_untouched();
}

#[cfg(feature = "test-support")]
#[test]
fn calendar_only_google_grant_uses_canonical_provider_and_enabled_state() {
    let root = tempfile::tempdir().unwrap();
    let id = AccountId::from("calendar-only");
    let custodian = Custodian::for_root(Some(root.path().to_owned()));
    custodian
        .seal_oauth(
            &id,
            &account_custody::TokenSet {
                access_token: "calendar-only-access".into(),
                refresh_token: "refresh".into(),
                obtained_at: u64::MAX / 2,
                expires_in: 3600,
                scope: Some(CALENDAR_SCOPE.into()),
            },
        )
        .unwrap();
    let trusted = Responder::start(vec![Reply::status(204)]);
    let service = Service::for_test_origin(custodian, &trusted.url()).unwrap();
    let mut store = AccountStore::new();
    let mut account = accounts::Account::new(id.clone(), "caldav");
    account.config = serde_json::json!({"calendar_provider": "google"});
    store.upsert(account);
    store.save_in(root.path()).unwrap();
    let client = service
        .authorised(Some(id.as_str()), &trusted.url(), "", "", "/")
        .unwrap();
    run(async {
        assert_eq!(
            client
                .report("/cal/", Depth::One, "<report/>".into())
                .await
                .unwrap()
                .status()
                .as_u16(),
            204
        );
    });
    assert_eq!(
        trusted.next_request().header("authorization"),
        Some("Bearer calendar-only-access")
    );

    for (type_id, provider, enabled, expected) in [
        ("caldav", "nextcloud", true, "account type"),
        ("caldav", "", true, "account type"),
        (cloud_types::DROPBOX, "google", true, "account type"),
        ("caldav", "google", false, "disabled"),
        (cloud_types::GOOGLE_DRIVE, "google", false, "disabled"),
    ] {
        let mut account = accounts::Account::new(id.clone(), type_id);
        account.config = serde_json::json!({"calendar_provider": provider});
        account.enabled = enabled;
        store.upsert(account);
        store.save_in(root.path()).unwrap();
        let error = service
            .authorised(Some(id.as_str()), &trusted.url(), "", "", "/")
            .err()
            .unwrap();
        assert!(
            error.contains(expected),
            "{type_id}/{provider}/{enabled}: {error}"
        );
        let error = service.discover(&id, &trusted.url(), "", true).unwrap_err();
        assert!(
            error.contains(expected),
            "{type_id}/{provider}/{enabled}: {error}"
        );
    }
}

#[test]
fn portable_calendar_password_uses_custody_for_plain_and_copied_grants() {
    let secret = accounts::Secret::password("app-password");
    assert_eq!(calendar_password(&secret), "app-password");
    let account = accounts::Account::new(AccountId::from("nextcloud"), cloud_types::NEXTCLOUD);
    let copied = account_custody::copied_secret(&account, &secret);
    assert_eq!(calendar_password(&copied), "app-password");
}

#[test]
fn calendar_transport_keeps_its_deadline_and_refuses_redirects() {
    let options = options();
    assert_eq!(options.timeout, std::time::Duration::from_secs(20));
    assert_eq!(options.user_agent, Some("Tilo-Calendar/0.1"));
    assert_eq!(options.redirects, Redirects::Refuse);
}

#[test]
fn production_google_never_accepts_alternate_origin_or_endpoint() {
    let root = tempfile::tempdir().unwrap();
    let service = Service::new(Custodian::for_root(Some(root.path().to_owned())));
    let attacker = Responder::silent();
    for server in [
        attacker.url(),
        "https://apidata.googleusercontent.com/not-caldav".into(),
        "http://apidata.googleusercontent.com/caldav/v2/".into(),
    ] {
        let error = service
            .authorised(Some("google"), &server, "", "", &server)
            .err()
            .unwrap();
        assert!(error.contains("Google CalDAV v2"), "{error}");
        assert!(
            service
                .discover(&AccountId::from("google"), &server, "", true)
                .unwrap_err()
                .contains("Google CalDAV v2")
        );
    }
    assert!(
        service
            .authorised(Some("google"), GOOGLE_SERVER, "", "", &attacker.url())
            .is_err()
    );
    attacker.assert_untouched();
}

#[test]
fn discovery_reuses_adopted_nextcloud_password_and_follows_server_identity() {
    let root = tempfile::tempdir().unwrap();
    let id = AccountId::from("nextcloud");
    let account = accounts::Account::new(id.clone(), cloud_types::NEXTCLOUD);
    let mut store = AccountStore::new();
    store.upsert(account.clone());
    store.save_in(root.path()).unwrap();
    let mut vault = SecretVault::open_in(root.path());
    vault.put(
        &id,
        &account_custody::copied_secret(&account, &accounts::Secret::password("adopted")),
    );
    vault.save();
    let xml = |href: &str, properties: &str| {
        format!(
            r#"<d:multistatus xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav"><d:response><d:href>{href}</d:href><d:propstat><d:prop>{properties}</d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response></d:multistatus>"#
        )
    };
    let server = Responder::start(vec![
        Reply::status(207).body(xml("/dav/", "<d:current-user-principal><d:href>/principal/opaque/</d:href></d:current-user-principal>")),
        Reply::status(207).body(xml("/principal/opaque/", "<c:calendar-home-set><d:href>/cal/opaque/</d:href></c:calendar-home-set>")),
        Reply::status(207).body(xml("/cal/opaque/team/", "<d:resourcetype><d:collection/><c:calendar/></d:resourcetype><d:displayname>Team</d:displayname>")),
    ]);
    let service = Service::new(Custodian::for_root(Some(root.path().to_owned())));
    let collections = service
        .discover(&id, &format!("{}/dav/", server.url()), "login", false)
        .unwrap();
    assert_eq!(collections.len(), 1);
    assert_eq!(collections[0].name, "Team");
    assert_eq!(
        collections[0].href,
        format!("{}/cal/opaque/team/", server.url())
    );
    for path in ["/dav/", "/principal/opaque/", "/cal/opaque/"] {
        let request = server.next_request();
        assert_eq!(request.target, path);
        assert_eq!(
            request.header("authorization"),
            Some("Basic bG9naW46YWRvcHRlZA==")
        );
    }
}

#[test]
fn redirects_never_forward_the_password() {
    let root = tempfile::tempdir().unwrap();
    let attacker = Responder::silent();
    let trusted = Responder::start(vec![Reply::status(302).header("Location", &attacker.url())]);
    let service = Service::new(Custodian::for_root(Some(root.path().to_owned())));
    let client = service
        .authorised(None, &trusted.url(), "ada", "secret", "/")
        .unwrap();
    run(async {
        assert_eq!(
            client
                .report("/", Depth::One, "<report/>".into())
                .await
                .unwrap()
                .status()
                .as_u16(),
            302
        );
    });
    assert!(trusted.next_request().header("authorization").is_some());
    attacker.assert_untouched();
}

#[cfg(feature = "test-support")]
#[test]
fn fixture_bearer_is_bound_and_requires_canonical_google_calendar_grant() {
    let root = tempfile::tempdir().unwrap();
    let token_server = Responder::start(vec![Reply::status(200)
        .header("Content-Type", "application/json")
        .body(r#"{"access_token":"calendar-access","token_type":"Bearer","expires_in":3600,"refresh_token":"rotated"}"#)]);
    let mut provider = OAuthIssuer::Google.endpoints();
    provider.token_endpoint = format!("{}/token", token_server.url());
    let custodian = Custodian::for_root(Some(root.path().to_owned()))
        .with_issuer_endpoints(OAuthIssuer::Google, provider);
    let id = AccountId::from("google");
    let mut store = AccountStore::new();
    store.upsert(accounts::Account::new(
        id.clone(),
        cloud_types::GOOGLE_DRIVE,
    ));
    store.save_in(root.path()).unwrap();
    custodian
        .seal_oauth(
            &id,
            &account_custody::TokenSet {
                access_token: "expired".into(),
                refresh_token: "refresh".into(),
                obtained_at: 1,
                expires_in: 1,
                scope: Some(CALENDAR_SCOPE.into()),
            },
        )
        .unwrap();
    let trusted = Responder::start(vec![Reply::status(204)]);
    let attacker = Responder::silent();
    let service = Service::for_test_origin(custodian.clone(), &trusted.url()).unwrap();
    assert!(
        service
            .authorised(Some(id.as_str()), &attacker.url(), "", "", "/")
            .is_err()
    );
    let client = service
        .authorised(Some(id.as_str()), &trusted.url(), "", "", "/")
        .unwrap();
    run(async {
        assert!(
            client
                .report(&attacker.url(), Depth::One, "<report/>".into())
                .await
                .is_err()
        );
        client
            .report("/cal/", Depth::One, "<report/>".into())
            .await
            .unwrap();
    });
    assert_eq!(
        trusted.next_request().header("authorization"),
        Some("Bearer calendar-access")
    );
    let refreshed = token_server.next_request();
    assert_eq!(refreshed.target, "/token");
    assert!(refreshed.body_text().contains("grant_type=refresh_token"));
    assert!(refreshed.body_text().contains("refresh_token=refresh"));
    // A files grant and a wrong account type must fail, even if a token exists.
    custodian
        .seal_oauth(
            &id,
            &account_custody::TokenSet {
                access_token: "files-access".into(),
                refresh_token: "refresh".into(),
                obtained_at: u64::MAX / 2,
                expires_in: 3600,
                scope: Some("https://www.googleapis.com/auth/drive".into()),
            },
        )
        .unwrap();
    assert!(
        service
            .authorised(Some(id.as_str()), &trusted.url(), "", "", "/")
            .err()
            .unwrap()
            .contains("no calendar grant")
    );
    store.upsert(accounts::Account::new(id.clone(), cloud_types::DROPBOX));
    store.save_in(root.path()).unwrap();
    assert!(
        service
            .authorised(Some(id.as_str()), &trusted.url(), "", "", "/")
            .err()
            .unwrap()
            .contains("account type")
    );
    attacker.assert_untouched();
}
