use super::*;
use test_http::{Reply, Responder};

#[test]
fn nextcloud_pending_discovers_server_identity_with_private_input() {
    let xml = |href: &str, properties: &str| {
        format!(
            r#"<d:multistatus xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav"><d:response><d:href>{href}</d:href><d:propstat><d:prop>{properties}</d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response></d:multistatus>"#
        )
    };
    let server = Responder::start(vec![
        Reply::status(207).body(xml("/cloud/remote.php/dav/", "<d:current-user-principal><d:href>/principal/opaque/</d:href></d:current-user-principal>")),
        Reply::status(207).body(xml("/principal/opaque/", "<c:calendar-home-set><d:href>/cal/opaque/</d:href></c:calendar-home-set>")),
        Reply::status(207).body(xml("/cal/opaque/team/", "<d:resourcetype><d:collection/><c:calendar/></d:resourcetype><d:displayname>Team</d:displayname>")),
    ]);
    let pending =
        PendingNextcloudCalendar::new(&format!("{}/cloud", server.url()), "login", "private")
            .unwrap();
    let result = pending.discover().unwrap();
    assert_eq!(result.username, "login");
    assert_eq!(
        result.server_url,
        format!("{}/cloud/remote.php/dav/", server.url())
    );
    assert_eq!(result.collections.len(), 1);
    assert_eq!(
        result.collections[0].href,
        format!("{}/cal/opaque/team/", server.url())
    );
    for path in [
        "/cloud/remote.php/dav/",
        "/principal/opaque/",
        "/cal/opaque/",
    ] {
        let request = server.next_request();
        assert_eq!(request.target, path);
        assert_eq!(
            request.header("authorization"),
            Some("Basic bG9naW46cHJpdmF0ZQ==")
        );
    }
}

#[test]
fn nextcloud_pending_refuses_credential_forwarding() {
    let attacker = Responder::silent();
    let server = Responder::start(vec![Reply::status(302).header("location", &attacker.url())]);
    let pending = PendingNextcloudCalendar::new(&server.url(), "login", "private").unwrap();
    assert!(pending.discover().is_err());
    attacker.assert_untouched();
}

#[test]
fn pending_nextcloud_rejects_unsafe_endpoints() {
    for server in [
        "http://example.org",
        "https://login:secret@example.org",
        "https://example.org/?token=secret",
        "https://example.org/#private",
        "https://example.org/%2e%2e/elsewhere",
    ] {
        assert!(
            PendingNextcloudCalendar::new(server, "login", "private").is_err(),
            "{server}"
        );
    }
}

#[test]
fn pending_google_stages_combined_grant_without_returning_tokens() {
    let root = tempfile::tempdir().unwrap();
    let tokens = TokenSet {
        access_token: "combined-access".into(),
        refresh_token: "combined-refresh".into(),
        obtained_at: 123,
        expires_in: 3600,
        scope: Some(format!(
            "{CALENDAR_SCOPE} https://www.googleapis.com/auth/drive"
        )),
    };
    let expected = account_custody::oauth_secret(&tokens);
    let pending = PendingGoogleCalendar { tokens };
    let mut vault = accounts::SecretVault::open_in(root.path());
    let ids = ["files", "mail", "calendar"].map(accounts::AccountId::from);
    for id in &ids {
        pending.stage(id, &mut vault);
    }
    vault.try_save().unwrap();
    drop(pending);
    let saved = accounts::SecretVault::open_in(root.path());
    for id in &ids {
        let secret = saved.get(id).unwrap();
        assert_eq!(secret.password, expected.password);
        assert_eq!(secret.token, expected.token);
    }
}

#[test]
fn pending_google_rejects_other_issuers_before_exchange() {
    let issuer = account_custody::OAuthIssuer::Microsoft;
    let registration = account_custody::Registration {
        client_id: "test-client".into(),
        ..Default::default()
    };
    let authorization = Authorization::begin(&issuer.endpoints(), &registration, None).unwrap();
    assert!(PendingGoogleCalendar::complete(authorization).is_err());
    let mut provider = account_custody::OAuthIssuer::Google.endpoints();
    provider.scopes = vec!["openid".into()];
    let authorization = Authorization::begin(&provider, &registration, None).unwrap();
    assert!(PendingGoogleCalendar::complete(authorization).is_err());
}
