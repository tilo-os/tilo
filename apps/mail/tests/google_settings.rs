#![cfg(feature = "live")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test assertions require observable failures"
)]

use account_custody::{ClientTable, OAuthIssuer};
use settings::accounts::{AccountKind, AccountRoot};
use settings::panes::accounts::{AccountViewCommand, AccountsPane, BrowserOpener};
use std::sync::{Arc, Mutex};
use test_http::{Reply, Responder};
use tilo_oauth::fake::{FakeAuthorizationServer, browse};

#[derive(Default)]
struct Browser(Mutex<Vec<String>>);
impl BrowserOpener for Browser {
    fn open(&self, url: &str) -> ui::portal::LaunchChannel {
        self.0.lock().unwrap().push(url.to_string());
        let url = url.to_string();
        std::thread::spawn(move || browse(&url).unwrap());
        let (sender, receiver) = std::sync::mpsc::channel();
        sender.send(ui::portal::LaunchOutcome::Opened).unwrap();
        receiver
    }
}

#[test]
fn explicit_google_mail_consent_connects_settings_to_mail_and_refresh() {
    let dir = tempfile::tempdir().unwrap();
    let root = AccountRoot::at(dir.path().to_path_buf());
    let oauth = FakeAuthorizationServer::start();
    let registration = ClientTable::defaults()
        .client_registration(OAuthIssuer::Google)
        .unwrap();
    oauth.require_client_secret(registration.secret().unwrap());
    let gmail = Responder::start(vec![
        Reply::status(200)
            .header("Content-Type", "application/json")
            .body(r#"{"emailAddress":"ada@example.org"}"#),
    ]);
    let custody = root
        .custodian()
        .with_issuer_endpoints(OAuthIssuer::Google, oauth.provider())
        .with_gmail_profile_endpoint(format!("{}/gmail/v1/users/me/profile", gmail.url()));
    let browser = Arc::new(Browser::default());
    let mut pane = AccountsPane::in_root(root.clone())
        .with_custodian(custody.clone())
        .with_browser_opener(browser.clone());
    pane.command(AccountViewCommand::Add(AccountKind::GoogleDrive));
    pane.command(AccountViewCommand::SubmitGoogleMail);
    assert!(pane.finish_sign_in_step_for_test(tilo_test_env::hang_bound()));
    assert!(
        !pane.sign_in_open(),
        "{:?}",
        pane.sign_in_message_for_test()
    );
    let urls = browser.0.lock().unwrap();
    assert!(urls[0].contains("https%3A%2F%2Fmail.google.com%2F"));
    assert!(urls[0].contains("https%3A%2F%2Fwww.googleapis.com%2Fauth%2Fdrive"));
    drop(urls);
    let request = gmail.next_request();
    assert_eq!(request.target, "/gmail/v1/users/me/profile");
    assert!(
        request
            .header("Authorization")
            .unwrap()
            .starts_with("Bearer ")
    );
    let store = mail::account::AccountStore::load_in(dir.path());
    assert_eq!(store.accounts.len(), 1);
    let account = &store.accounts[0];
    assert_eq!(account.config.email, "ada@example.org");
    assert_eq!(account.config.imap_host, "imap.gmail.com");
    assert_eq!(account.config.smtp_host, "smtp.gmail.com");
    assert_eq!(account.config.smtp_port, 587);
    assert_eq!(account.config.auth, accounts::email::AuthMethod::OAuth);
    assert!(account.config.password.is_empty());
    assert_eq!(
        root.store()
            .list(&accounts::cloud_types::GOOGLE_DRIVE.into())
            .len(),
        1
    );
    assert!(
        !serde_json::to_string(&root.store())
            .unwrap()
            .contains("refresh-")
    );
    oauth.issue_access_token("gmail-refreshed");
    assert_eq!(
        mail::oauth::access_token(
            &custody,
            &account.id,
            mail::oauth::OAuthProvider::Google,
            true
        )
        .unwrap(),
        "gmail-refreshed"
    );
    assert_eq!(oauth.refreshes(), 1);
}

#[test]
fn files_only_google_consent_does_not_create_a_mail_account() {
    let dir = tempfile::tempdir().unwrap();
    let root = AccountRoot::at(dir.path().to_path_buf());
    let oauth = FakeAuthorizationServer::start();
    let custody = root
        .custodian()
        .with_issuer_endpoints(OAuthIssuer::Google, oauth.provider());
    let browser = Arc::new(Browser::default());
    let mut pane = AccountsPane::in_root(root.clone())
        .with_custodian(custody)
        .with_browser_opener(browser.clone());
    pane.command(AccountViewCommand::Add(AccountKind::GoogleDrive));
    pane.command(AccountViewCommand::Submit);
    assert!(pane.finish_sign_in_step_for_test(tilo_test_env::hang_bound()));
    assert!(
        !pane.sign_in_open(),
        "{:?}",
        pane.sign_in_message_for_test()
    );
    assert!(!browser.0.lock().unwrap()[0].contains("mail.google.com"));
    assert!(
        mail::account::AccountStore::load_in(dir.path())
            .accounts
            .is_empty()
    );
    assert_eq!(root.store().len(), 1);
}

#[test]
fn gmail_permission_or_capability_failure_saves_nothing_and_can_retry_files_only() {
    for status in [403, 404] {
        let dir = tempfile::tempdir().unwrap();
        let root = AccountRoot::at(dir.path().to_path_buf());
        let oauth = FakeAuthorizationServer::start();
        let gmail = Responder::start(vec![Reply::status(status)]);
        let custody = root
            .custodian()
            .with_issuer_endpoints(OAuthIssuer::Google, oauth.provider())
            .with_gmail_profile_endpoint(format!("{}/profile", gmail.url()));
        let mut pane = AccountsPane::in_root(root.clone())
            .with_custodian(custody)
            .with_browser_opener(Arc::new(Browser::default()));
        pane.command(AccountViewCommand::Add(AccountKind::GoogleDrive));
        pane.command(AccountViewCommand::SubmitGoogleMail);
        assert!(pane.finish_sign_in_step_for_test(tilo_test_env::hang_bound()));
        assert!(pane.sign_in_open());
        assert!(
            pane.sign_in_message_for_test()
                .unwrap()
                .contains(&format!("HTTP {status}"))
        );
        assert!(root.store().is_empty());
        assert!(root.vault().get(&"ada@example.org".into()).is_none());
        pane.select_account_services(true, false, "sync").unwrap();
        pane.command(AccountViewCommand::Submit);
        assert!(pane.finish_sign_in_step_for_test(tilo_test_env::hang_bound()));
        assert!(
            !pane.sign_in_open(),
            "{:?}",
            pane.sign_in_message_for_test()
        );
        assert!(
            mail::account::AccountStore::load_in(dir.path())
                .accounts
                .is_empty()
        );
        assert_eq!(root.store().len(), 1);
    }
}

#[test]
fn google_granular_consent_requires_both_mail_and_drive() {
    for (granted, message) in [
        (
            "https://www.googleapis.com/auth/drive",
            "Gmail permission was not granted",
        ),
        (
            "https://mail.google.com/",
            "Google Drive permission was not granted",
        ),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let root = AccountRoot::at(dir.path().to_path_buf());
        let oauth = FakeAuthorizationServer::start();
        let token_endpoint = format!("{}/token", oauth.url());
        // Preserve the fake provider's real code/PKCE validation, but model Google's
        // granular consent response: the person granted files, not mail.
        let proxy = Responder::handle(move |request| {
            assert_eq!(request.method, "POST");
            let response = tilo_net::http::blocking_client()
                .build()
                .unwrap()
                .post(&token_endpoint)
                .header("Content-Type", "application/x-www-form-urlencoded")
                .body(request.body.clone())
                .send()
                .unwrap();
            assert!(response.status().is_success());
            let mut tokens: serde_json::Value = response.json().unwrap();
            tokens["scope"] = granted.into();
            Some(
                Reply::status(200)
                    .header("Content-Type", "application/json")
                    .body(tokens.to_string()),
            )
        });
        let mut provider = oauth.provider();
        provider.token_endpoint = format!("{}/token", proxy.url());
        let custody = root
            .custodian()
            .with_issuer_endpoints(OAuthIssuer::Google, provider);
        let mut pane = AccountsPane::in_root(root.clone())
            .with_custodian(custody)
            .with_browser_opener(Arc::new(Browser::default()));
        pane.command(AccountViewCommand::Add(AccountKind::GoogleDrive));
        pane.command(AccountViewCommand::SubmitGoogleMail);
        assert!(pane.finish_sign_in_step_for_test(tilo_test_env::hang_bound()));
        assert!(pane.sign_in_open());
        assert!(pane.sign_in_message_for_test().unwrap().contains(message));
        assert!(root.store().is_empty());
        assert!(
            mail::account::AccountStore::load_in(dir.path())
                .accounts
                .is_empty()
        );
    }
}

#[test]
fn a_build_without_gmail_scopes_refuses_mail_consent_before_opening_browser() {
    let dir = tempfile::tempdir().unwrap();
    let root = AccountRoot::at(dir.path().to_path_buf());
    let mut table = ClientTable::defaults().clone();
    table.google.scopes.mail.clear();
    let browser = Arc::new(Browser::default());
    let mut pane = AccountsPane::in_root(root.clone())
        .with_custodian(root.custodian().with_client_table(table))
        .with_browser_opener(browser.clone());
    pane.command(AccountViewCommand::Add(AccountKind::GoogleDrive));
    pane.command(AccountViewCommand::SubmitGoogleMail);
    assert!(pane.sign_in_open());
    assert!(
        pane.sign_in_message_for_test()
            .unwrap()
            .contains("Gmail sign-in is not configured")
    );
    assert!(browser.0.lock().unwrap().is_empty());
    assert!(root.store().is_empty());
}
