//! An email account added through Settings' published action reaches Mail,
//! which is released its secret by account custody (never opening a vault
//! itself) and signs in to the account's IMAP server.

use std::sync::Arc;

use ui::framework::testing::TestApp;

use crate::fake_servers::{FakeImap, FakeSmtp, Layout, accepting_client_config, server_tls_config};
use crate::resource::MailShell;
use settings::panes::accounts::AccountsEffect;

const ME: &str = "hiring@studio.example";

#[test]
fn an_account_added_through_settings_reaches_mail_which_is_released_its_secret_and_fetches() {
    let rt = ui::rt::app();
    let imap = rt
        .block_on(FakeImap::start(Layout::Gmail, server_tls_config()))
        .unwrap();
    let smtp = rt.block_on(FakeSmtp::start(server_tls_config())).unwrap();
    imap.append(
        "INBOX",
        "From: Ada <ada@example.test>\r\nTo: hiring@studio.example\r\n\
Subject: Application\r\nMessage-ID: <application@example.test>\r\n\
Date: Thu, 29 Oct 2026 07:00:00 +0000\r\n\r\nHello.\r\n",
        &[],
    );
    let kept = tempfile::tempdir().unwrap();
    let mut settings =
        TestApp::<settings::resource::SettingsShell>::open_data_dir(kept.path()).unwrap();
    let added = settings
        .effect(&AccountsEffect::AddEmailAccount {
            display_name: "Studio hiring".into(),
            email: ME.into(),
            username: None,
            imap_host: "127.0.0.1".into(),
            imap_port: imap.port,
            smtp_host: "127.0.0.1".into(),
            smtp_port: smtp.port,
            password: "app-password".into(),
        })
        .expect("Settings adds the account");
    assert_eq!(added.value.unwrap()["account"], ME);
    let record = settings
        .shared_person::<accounts::person::PersonAccounts>()
        .unwrap();
    let account = record
        .get()
        .store()
        .get(&accounts::AccountId::from(ME))
        .cloned()
        .expect("the account is the person's");
    assert_eq!(account.type_id.as_str(), crate::account::TYPE_ID);
    // The password is no setting of the account's.
    assert!(!account.config.to_string().contains("app-password"));

    // A Mail window opens no vault: custody releases it the secret.
    let mut mail = TestApp::<MailShell>::new().unwrap();
    {
        let app = mail.surface_mut::<crate::app::MailApp>();
        app.custody = Some(crate::custody::SecretRelease::kept_under(
            kept.path().to_path_buf(),
        ));
        let engine = crate::engine::live::LiveEngine::trusting(
            tilo_content::ContentStore::memory(),
            Arc::new(|| {}),
            accepting_client_config(),
        )
        .unwrap();
        let (tx, rx) = crate::engine::spawn_with(Box::new(engine));
        app.state().engine = Some(crate::app::EngineHandle { tx, rx });
        app.follow_person_accounts(record);
    }
    let held = 1;
    let deadline = tilo_test_env::hang_deadline();
    loop {
        mail.settle();
        let (shown, fetched, error) = {
            let app = mail.surface_mut::<crate::app::MailApp>();
            let state = app.shared.borrow();
            (
                state
                    .accounts
                    .active_account()
                    .map(|account| (account.id.clone(), !account.config.password.is_empty())),
                state.mailbox.messages.len(),
                state.sync_error.clone(),
            )
        };
        assert_eq!(error, None, "the account signs in");
        if fetched >= held {
            assert_eq!(shown, Some((ME.to_owned(), true)));
            break;
        }
        assert!(
            ui::time::now() < deadline,
            "the inbox is fetched: {shown:?}"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    drop(imap);
}

/// Custody releases nothing for an account of a type Mail does not keep, and
/// Mail says so instead of signing in with nothing.
#[test]
fn custody_releases_nothing_mail_does_not_keep() {
    let kept = tempfile::tempdir().unwrap();
    let mut store = accounts::AccountStore::new();
    store.upsert(accounts::Account::new(
        accounts::AccountId::from("dav@example.test"),
        "caldav",
    ));
    store.save_in(kept.path()).unwrap();
    let mut credentials = accounts::AccountCredentials::open_in(kept.path()).unwrap();
    accounts::custody::seal(
        &mut credentials,
        accounts::person::ACCOUNTS_OWNER,
        "dav@example.test",
        "dav-password",
    )
    .unwrap();
    let mut release = crate::custody::SecretRelease::kept_under(kept.path().to_path_buf());
    release.ask("dav@example.test");
    let answers = release.poll(None);
    assert!(matches!(
        answers.as_slice(),
        [crate::custody::Released::Refused { account, .. }] if account == "dav@example.test"
    ));
}
