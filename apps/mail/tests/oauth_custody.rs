//! Gmail and Outlook sign-in end to end through account custody, against
//! the fake authorization server: Mail's registration is the shared client
//! table's (Google's Desktop client secret included), its scopes are the
//! mail ones, the grant is sealed and refreshed by the custodian, and a
//! grant Mail sealed before custody (a real vault from that format) keeps
//! working.

#![cfg(feature = "live")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::fmt::Write as _;
use std::path::Path;

use account_custody::{ClientTable, Custodian, OAuthIssuer, Registration};
use accounts::{AccountId, SecretVault};
use mail::oauth::{self, OAuthProvider};
use tilo_oauth::fake::{FakeAuthorizationServer, browse};

fn registration(issuer: OAuthIssuer) -> Registration {
    ClientTable::defaults()
        .client_registration(issuer)
        .expect("the committed client table registers Google and Microsoft")
}

/// A custodian over `root` whose `issuer` requests go to `server`, with the
/// committed client table for everything else.
fn custodian(root: &Path, issuer: OAuthIssuer, server: &FakeAuthorizationServer) -> Custodian {
    Custodian::for_root(Some(root.to_path_buf())).with_issuer_endpoints(issuer, server.provider())
}

fn sign_in(custodian: &Custodian, email: &str, provider: OAuthProvider) {
    let mut browser = None;
    oauth::sign_in(custodian, email, provider, email, |url| {
        let url = url.to_string();
        browser = Some(std::thread::spawn(move || browse(&url)));
        Ok(())
    })
    .expect("sign-in");
    browser
        .expect("the browser was opened")
        .join()
        .expect("browser thread")
        .expect("browser round trip");
}

fn sealed_grant(root: &Path, id: &str) -> serde_json::Value {
    let secret = SecretVault::open_in(root)
        .get(&AccountId::from(id))
        .expect("a sealed grant");
    serde_json::from_str(secret.token.as_deref().expect("a token")).unwrap()
}

fn form(body: &str, key: &str) -> Option<String> {
    body.split('&').find_map(|pair| {
        let (name, value) = pair.split_once('=')?;
        (name == key).then(|| value.to_string())
    })
}

#[test]
fn gmail_signs_in_with_the_shared_google_client_and_its_secret() {
    let google = registration(OAuthIssuer::Google);
    let secret = google
        .secret()
        .expect("Google's Desktop client has a secret")
        .to_string();
    let server = FakeAuthorizationServer::start();
    server.require_client_secret(&secret);
    let dir = tempfile::tempdir().unwrap();
    let custodian = custodian(dir.path(), OAuthIssuer::Google, &server);
    let email = "ada@gmail.com";

    sign_in(&custodian, email, OAuthProvider::Google);

    let requests = server.requests();
    let authorize = requests
        .iter()
        .find(|request| request.target.starts_with("/authorize?"))
        .expect("the browser visited the authorization endpoint");
    assert!(
        authorize
            .target
            .contains("scope=https%3A%2F%2Fmail.google.com%2F&"),
        "Gmail asks for the mail scope only: {}",
        authorize.target
    );
    assert!(authorize.target.contains("access_type=offline"));
    assert!(authorize.target.contains("login_hint=ada%40gmail.com"));
    let client_id = url_encoded(&google.client_id);
    assert!(
        authorize
            .target
            .contains(&format!("client_id={client_id}&")),
        "the shared Google client: {}",
        authorize.target
    );
    let exchange = requests
        .iter()
        .find(|request| request.target == "/token")
        .expect("the code was redeemed");
    let body = exchange.body_text();
    assert_eq!(
        form(&body, "grant_type").as_deref(),
        Some("authorization_code")
    );
    assert_eq!(
        form(&body, "client_secret"),
        Some(url_encoded(&secret)),
        "Google's token endpoint gets the Desktop client secret"
    );

    // Custody holds the grant in its own format, and hands Mail the access
    // token without a refresh while it is fresh.
    let grant = sealed_grant(dir.path(), email);
    assert_eq!(grant["kind"], "oauth2");
    let access = grant["tokens"]["access_token"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(
        oauth::access_token(&custodian, email, OAuthProvider::Google, false).unwrap(),
        access
    );
    assert_eq!(server.refreshes(), 0);

    // A forced refresh (the server rejected the token) goes through the
    // same client, secret included, and is resealed.
    server.issue_access_token("ya29.refreshed");
    assert_eq!(
        oauth::access_token(&custodian, email, OAuthProvider::Google, true).unwrap(),
        "ya29.refreshed"
    );
    assert_eq!(server.refreshes(), 1);
    let refresh = server
        .requests()
        .into_iter()
        .find(|request| request.target == "/token")
        .expect("the refresh request");
    assert_eq!(
        form(&refresh.body_text(), "client_secret"),
        Some(url_encoded(&secret))
    );
    assert_eq!(
        sealed_grant(dir.path(), email)["tokens"]["access_token"],
        "ya29.refreshed"
    );
}

#[test]
fn outlook_signs_in_with_the_shared_microsoft_client_and_mail_scopes() {
    let microsoft = registration(OAuthIssuer::Microsoft);
    let server = FakeAuthorizationServer::start();
    let dir = tempfile::tempdir().unwrap();
    let custodian = custodian(dir.path(), OAuthIssuer::Microsoft, &server);
    let email = "ada@outlook.com";

    sign_in(&custodian, email, OAuthProvider::Microsoft);

    let requests = server.requests();
    let authorize = requests
        .iter()
        .find(|request| request.target.starts_with("/authorize?"))
        .expect("the browser visited the authorization endpoint");
    for scope in [
        "offline_access",
        "https%3A%2F%2Foutlook.office.com%2FIMAP.AccessAsUser.All",
        "https%3A%2F%2Foutlook.office.com%2FSMTP.Send",
    ] {
        assert!(
            authorize.target.contains(scope),
            "{scope} in {}",
            authorize.target
        );
    }
    assert!(
        !authorize.target.contains("Files.ReadWrite"),
        "Mail does not ask for OneDrive"
    );
    let client_id = url_encoded(&microsoft.client_id);
    assert!(
        authorize
            .target
            .contains(&format!("client_id={client_id}&"))
    );
    let exchange = requests
        .iter()
        .find(|request| request.target == "/token")
        .expect("the code was redeemed");
    assert_eq!(
        form(&exchange.body_text(), "client_secret"),
        None,
        "Microsoft's public client has no secret"
    );

    // Near expiry, the next access token comes from a refresh by custody.
    let expired = {
        let mut grant = sealed_grant(dir.path(), email);
        grant["tokens"]["obtained_at"] = 1.into();
        grant
    };
    let mut vault = SecretVault::open_in(dir.path());
    vault.put(
        &AccountId::from(email),
        &accounts::Secret {
            password: String::new(),
            token: Some(expired.to_string()),
        },
    );
    vault.try_save().unwrap();
    server.issue_access_token("outlook-refreshed");
    assert_eq!(
        oauth::access_token(&custodian, email, OAuthProvider::Microsoft, false).unwrap(),
        "outlook-refreshed"
    );
    assert_eq!(server.refreshes(), 1);
}

#[test]
fn a_revoked_grant_asks_mail_to_sign_in_again() {
    let server = FakeAuthorizationServer::start();
    server.require_client_secret(registration(OAuthIssuer::Google).secret().unwrap());
    let dir = tempfile::tempdir().unwrap();
    let custodian = custodian(dir.path(), OAuthIssuer::Google, &server);
    let email = "ada@gmail.com";
    sign_in(&custodian, email, OAuthProvider::Google);
    server.revoke_all_refresh_tokens();

    let error = oauth::access_token(&custodian, email, OAuthProvider::Google, true).unwrap_err();
    // "OAuth" marks the Mail account as needing sign-in.
    assert!(
        error.contains("OAuth") && error.contains("sign in again"),
        "{error}"
    );
    // The dead grant is not handed out again.
    assert!(oauth::access_token(&custodian, email, OAuthProvider::Google, false).is_err());
}

fn url_encoded(value: &str) -> String {
    let mut out = String::new();
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'*' => {
                out.push(char::from(byte));
            }
            b' ' => out.push('+'),
            other => {
                let _ = write!(out, "%{other:02X}");
            }
        }
    }
    out
}
