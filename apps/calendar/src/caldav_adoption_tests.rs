use account_custody::{Custodian, OAuthIssuer, TokenSet, portable};
use accounts::{Account, AccountId, AccountStore, SecretVault};

struct AdoptionFixture {
    destination: tempfile::TempDir,
    server: Responder,
    tokens: Responder,
    id: AccountId,
    expired: TokenSet,
    selected: RemoteCalendar,
}

fn adopted_google() -> AdoptionFixture {
    let source = tempfile::tempdir().unwrap();
    let destination = tempfile::tempdir().unwrap();
    let server = Responder::start(vec![
        Reply::status(207).body(r#"<d:multistatus xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav"><d:sync-token>next</d:sync-token><d:response><d:href>/cal/work/event.ics</d:href><d:propstat><d:prop><d:getetag>"v1"</d:getetag><c:calendar-data>BEGIN:VCALENDAR
VERSION:2.0
BEGIN:VEVENT
UID:remote-event
DTSTART:20261010T090000Z
DTEND:20261010T100000Z
SUMMARY:Adopted event
END:VEVENT
END:VCALENDAR
</c:calendar-data></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response></d:multistatus>"#),
        Reply::status(412),
        Reply::status(204).header("ETag", "\"v2\""),
        Reply::status(412),
        Reply::status(204),
    ]);
    let tokens = Responder::start(vec![
        Reply::status(200).header("Content-Type", "application/json").body(r#"{"access_token":"first-access","token_type":"Bearer","expires_in":3600,"refresh_token":"first-rotation"}"#),
        Reply::status(200).header("Content-Type", "application/json").body(r#"{"access_token":"second-access","token_type":"Bearer","expires_in":3600,"refresh_token":"second-rotation"}"#),
    ]);
    let mut account = Account::new("canonical-google", "google-drive");
    account.config = serde_json::json!({"use_files":true});
    let config = CalDavConfig {
        server_url: server.url(),
        ..Default::default()
    };
    let selected = RemoteCalendar {
        id: format!("{}/cal/work/", server.url()),
        name: "Work".into(),
        visible: true,
        read_only: false,
        color: 2,
    };
    let unselected = RemoteCalendar {
        id: format!("{}/cal/private/", server.url()),
        ..selected.clone()
    };
    crate::setup::select_calendars(
        &mut account,
        &config,
        &[selected.clone(), unselected],
        std::slice::from_ref(&selected.id),
        true,
    )
    .unwrap();
    let id = AccountId::from("canonical-google");
    let mut store = AccountStore::new();
    store.upsert(account);
    store.save_in(source.path()).unwrap();
    let expired = TokenSet {
        access_token: "expired".into(),
        refresh_token: "selected-refresh".into(),
        obtained_at: 1,
        expires_in: 1,
        scope: Some("https://www.googleapis.com/auth/calendar".into()),
    };
    Custodian::for_root(Some(source.path().to_owned()))
        .seal_oauth(&id, &expired)
        .unwrap();
    let password = b"explicit portable password";
    let binding = b"calendar-selected-fixture";
    let artifact = portable::seal_selected_accounts_with_password(
        source.path(),
        std::slice::from_ref(&id),
        password,
        binding,
    )
    .unwrap();
    assert_eq!(
        portable::import_password_sealed_accounts_matching(
            destination.path(),
            &artifact,
            password,
            binding,
            std::slice::from_ref(&id)
        )
        .unwrap(),
        vec![id.clone()]
    );
    AdoptionFixture {
        destination,
        server,
        tokens,
        id,
        expired,
        selected,
    }
}

#[test]
fn portable_google_selection_uses_production_report_and_etag_guards() {
    let AdoptionFixture {
        destination,
        server,
        tokens,
        id,
        expired,
        selected,
    } = adopted_google();
    let store = AccountStore::load_in(destination.path());
    assert_eq!(store.all().len(), 1);
    assert_eq!(store.get(&id).unwrap().config["use_files"], true);
    let projection = crate::AccountStore::from_store(
        &store,
        &accounts::AccountCredentials::open_in(destination.path()).unwrap(),
    );
    assert_eq!(projection.accounts.len(), 1);
    let adopted = &projection.accounts[0];
    assert_eq!(adopted.calendars.len(), 1);
    assert_eq!(adopted.calendars[0].id, selected.id);
    assert_eq!(
        adopted.config.oauth_account_id.as_deref(),
        Some("canonical-google")
    );
    let mut endpoints = OAuthIssuer::Google.endpoints();
    endpoints.token_endpoint = format!("{}/token", tokens.url());
    let custodian = Custodian::for_root(Some(destination.path().to_owned()))
        .with_issuer_endpoints(OAuthIssuer::Google, endpoints);
    let service = Service::for_test_origin(custodian.clone(), &server.url()).unwrap();
    let client = CalDavClient::new(service);
    let (pulled, outcome) = run(client.sync_all(
        &[(adopted.config.clone(), adopted.calendars.clone())],
        NaiveDate::from_ymd_opt(2026, 10, 10).unwrap(),
        &[],
    ));
    assert!(
        matches!(outcome, SyncOutcome::Synced { calendars: 1, .. }),
        "{outcome:?}"
    );
    assert_eq!(pulled.resources.len(), 1);
    let resource = &pulled.resources[0];
    assert_eq!(resource.etag.as_deref(), Some("\"v1\""));
    assert_eq!(resource.events[0].title, "Adopted event");
    let job = super::super::UploadJob {
        config: adopted.config.clone(),
        calendar: selected.clone(),
        event: resource.events[0].clone(),
        href: Some(resource.href.clone()),
        etag: resource.etag.clone(),
    };
    // Expire canonical custody after constructing and using the client. The
    // next request must refresh rather than reuse its earlier bearer token.
    custodian
        .seal_oauth(
            &id,
            &TokenSet {
                refresh_token: "first-rotation".into(),
                ..expired
            },
        )
        .unwrap();
    let rejected = run(client.push(std::slice::from_ref(&job)));
    assert_eq!(rejected.conflicts.len(), 1);
    assert!(rejected.confirmed.is_empty());
    let accepted = run(client.push(&[job]));
    assert_eq!(accepted.confirmed, vec![resource.events[0].id]);
    assert_eq!(accepted.revisions[0].etag.as_deref(), Some("\"v2\""));
    let remove = super::super::RemoveJob {
        config: adopted.config.clone(),
        calendar: selected,
        id: resource.events[0].id,
        href: Some(resource.href.clone()),
        etag: accepted.revisions[0].etag.clone(),
    };
    let rejected = run(client.remove(std::slice::from_ref(&remove)));
    assert_eq!(rejected.conflicts.len(), 1);
    assert!(rejected.confirmed.is_empty());
    assert_eq!(
        run(client.remove(&[remove])).confirmed,
        vec![resource.events[0].id]
    );
    assert_requests(&server, &tokens);
    let stored =
        serde_json::to_string(&SecretVault::open_in(destination.path()).get(&id).unwrap()).unwrap();
    assert!(stored.contains("second-rotation"));
    assert!(!stored.contains("first-rotation"));
}

fn assert_requests(server: &Responder, tokens: &Responder) {
    for (method, target, bearer, etag) in [
        ("REPORT", "/cal/work/", "Bearer first-access", None),
        (
            "PUT",
            "/cal/work/event.ics",
            "Bearer second-access",
            Some("\"v1\""),
        ),
        (
            "PUT",
            "/cal/work/event.ics",
            "Bearer second-access",
            Some("\"v1\""),
        ),
        (
            "DELETE",
            "/cal/work/event.ics",
            "Bearer second-access",
            Some("\"v2\""),
        ),
        (
            "DELETE",
            "/cal/work/event.ics",
            "Bearer second-access",
            Some("\"v2\""),
        ),
    ] {
        let request = server.next_request();
        assert_eq!(request.method, method);
        assert_eq!(request.target, target);
        assert_eq!(request.header("authorization"), Some(bearer));
        assert_eq!(request.header("if-match"), etag);
        if method == "REPORT" {
            assert!(request.body_text().contains("sync-collection"));
        }
        if method == "PUT" {
            assert!(request.body_text().contains("SUMMARY:Adopted event"));
        }
    }
    for refresh in ["selected-refresh", "first-rotation"] {
        let request = tokens.next_request();
        assert!(
            request
                .body_text()
                .contains(&format!("refresh_token={refresh}"))
        );
        assert!(request.body_text().contains("grant_type=refresh_token"));
    }
}
