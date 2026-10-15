#![cfg(feature = "custody")]

use account_custody::{Capability, ClientTable, Custodian, OAuthIssuer, OAuthUse, TokenSet};
use test_http::{Reply, Responder};

fn query(
    capabilities: &[Capability],
) -> Result<std::collections::BTreeMap<String, String>, Box<dyn std::error::Error>> {
    let authorization = Custodian::for_root(None).begin_sign_in_for_capabilities(
        OAuthIssuer::Google,
        capabilities,
        Some("ada@example.org"),
    )?;
    Ok(url::Url::parse(authorization.url())?
        .query_pairs()
        .into_owned()
        .collect())
}

#[test]
fn calendar_uses_the_caldav_scope_without_files_or_mail() {
    let table = ClientTable::defaults();
    assert_eq!(
        table.scopes(OAuthUse::calendar(OAuthIssuer::Google)),
        ["https://www.googleapis.com/auth/calendar"]
    );
    assert!(!table.is_configured(OAuthUse::calendar(OAuthIssuer::Microsoft)));
    let params = query(&[Capability::Calendar]).unwrap();
    assert_eq!(params["scope"], "https://www.googleapis.com/auth/calendar");
    assert_eq!(params["login_hint"], "ada@example.org");
    assert_eq!(params["code_challenge_method"], "S256");
}

#[test]
fn explicit_google_capabilities_never_force_drive_for_mail() {
    assert_eq!(
        query(&[Capability::Mail]).unwrap()["scope"],
        "https://mail.google.com/"
    );
    let params = query(&[Capability::Mail, Capability::Calendar, Capability::Mail]).unwrap();
    assert_eq!(
        params["scope"],
        "https://mail.google.com/ https://www.googleapis.com/auth/calendar"
    );
    let params = query(&[Capability::Files, Capability::Mail, Capability::Calendar]).unwrap();
    assert_eq!(
        params["scope"],
        "https://www.googleapis.com/auth/drive https://mail.google.com/ https://www.googleapis.com/auth/calendar"
    );
}

#[test]
fn unsupported_or_empty_capabilities_are_not_silently_dropped() {
    let custodian = Custodian::live();
    assert!(
        custodian
            .begin_sign_in_for_capabilities(OAuthIssuer::Google, &[], None)
            .is_err()
    );
    assert!(
        custodian
            .begin_sign_in_for_capabilities(
                OAuthIssuer::Dropbox,
                &[Capability::Files, Capability::Mail],
                None
            )
            .is_err()
    );
    assert!(
        custodian
            .begin_sign_in_for_capabilities(
                OAuthIssuer::Microsoft,
                &[Capability::Mail, Capability::Identity],
                None
            )
            .is_err()
    );
    assert!(
        custodian
            .begin_sign_in_for_capabilities(
                OAuthIssuer::Microsoft,
                &[Capability::Files, Capability::Mail],
                None
            )
            .is_err()
    );
}

#[test]
fn combining_capabilities_refuses_different_client_registrations() {
    let server = tilo_oauth::fake::FakeAuthorizationServer::start();
    let custodian = Custodian::live().with_provider(
        account_custody::OAuthCloud::GoogleDrive,
        server.provider(),
        "different-client",
    );
    assert!(
        custodian
            .begin_sign_in_for_capabilities(
                OAuthIssuer::Google,
                &[Capability::Files, Capability::Mail],
                None,
            )
            .is_err()
    );
}

#[test]
fn microsoft_identity_consent_has_no_mail_or_files_permission() {
    assert_eq!(
        ClientTable::defaults().scopes(OAuthUse::identity(OAuthIssuer::Microsoft)),
        ["offline_access", "User.Read"]
    );
    let auth = Custodian::live()
        .begin_sign_in_for(OAuthUse::identity(OAuthIssuer::Microsoft), None)
        .unwrap();
    let url = url::Url::parse(auth.url()).unwrap();
    let query: std::collections::BTreeMap<_, _> = url.query_pairs().collect();
    assert_eq!(query["scope"], "offline_access User.Read");
}

#[test]
fn microsoft_identity_is_provider_confirmed_and_authorization_is_sent() {
    let server = Responder::start(vec![
        Reply::status(200)
            .header("Content-Type", "application/json")
            .body(r#"{"mail":"ada@hosted.example","userPrincipalName":"not-a-mailbox"}"#),
    ]);
    let custodian =
        Custodian::live().with_microsoft_profile_endpoint(format!("{}/me", server.url()));
    let tokens = TokenSet {
        access_token: "graph-token".into(),
        ..Default::default()
    };
    assert_eq!(
        custodian.microsoft_mail_identity(&tokens).unwrap(),
        "ada@hosted.example"
    );
    let request = server.next_request();
    assert_eq!(request.method, "GET");
    assert_eq!(request.header("Authorization"), Some("Bearer graph-token"));
}

#[test]
fn microsoft_identity_refuses_missing_mail_invalid_profiles_and_http_errors() {
    let replies = vec![
        Reply::status(200).body(r#"{"userPrincipalName":"not-mail@tenant.example"}"#),
        Reply::status(200).body(r#"{"mail":"@example.org"}"#),
        Reply::status(200).body(r#"{"mail":"ada@example.org\r\nInjected"}"#),
        Reply::status(200).body("invalid json"),
        Reply::status(403),
    ];
    let server = Responder::start(replies);
    let custodian =
        Custodian::live().with_microsoft_profile_endpoint(format!("{}/me", server.url()));
    for _ in 0..5 {
        assert!(
            custodian
                .microsoft_mail_identity(&TokenSet::default())
                .is_err()
        );
    }
}

#[test]
fn microsoft_identity_does_not_follow_redirects_with_a_bearer() {
    let destination = Responder::start(vec![
        Reply::status(200).body(r#"{"mail":"ada@example.org"}"#),
    ]);
    let source = Responder::start(vec![
        Reply::status(302).header("Location", &destination.url()),
    ]);
    let custodian = Custodian::live().with_microsoft_profile_endpoint(source.url());
    assert!(
        custodian
            .microsoft_mail_identity(&TokenSet::default())
            .is_err()
    );
    assert_eq!(source.next_request().method, "GET");
}

#[test]
fn mail_settings_match_real_clients_including_hosted_domains() {
    let config = OAuthIssuer::Microsoft
        .mail_config("ada@hosted.example")
        .unwrap();
    assert_eq!(config.imap_host, "outlook.office365.com");
    assert_eq!(config.imap_port, 993);
    assert_eq!(config.smtp_host, "smtp.office365.com");
    assert_eq!(config.smtp_port, 587);
    assert!(config.imap_tls && config.smtp_tls);
    assert_eq!(config.username, "ada@hosted.example");
    assert_eq!(config.auth, accounts::email::AuthMethod::OAuth);
    assert!(config.password.is_empty());
    assert!(config.validate().is_empty());
    assert!(
        OAuthIssuer::Dropbox
            .mail_config("ada@example.org")
            .is_none()
    );
    for email in [
        "@example.org",
        "ada@",
        "ada@@example.org",
        "ada@example.org\0",
    ] {
        assert!(OAuthIssuer::Microsoft.mail_config(email).is_none());
    }
}
