//! End-to-end proof of the Accounts → Mail loop.
//!
//! Adding an email account in the Settings "External Accounts" pane must
//! persist to the shared account store + secret vault, and **Mail** must then
//! read that exact account back through its own loader. This test drives the
//! real Settings `sync` path (open form → fill fields → Save) — the same code
//! the live UI runs — then loads the result with `mail::account::AccountStore`,
//! the reader Mail uses at startup. No mocks: the two apps meet at the shared
//! `accounts` crate on disk.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

use automerge::AutoCommit;
use automerge::transaction::Transactable;

use settings::accounts::AccountRoot;
use settings::panes::accounts::{ADD_MAIL_KEY, AccountsPane};
use ui::widget::Widget;

struct AcceptAccount;

impl settings::account_probe::AccountTester for AcceptAccount {
    fn test(
        &self,
        _account: &accounts::Account,
        _secret: &accounts::Secret,
    ) -> Result<String, String> {
        Ok("signed in".into())
    }
}

#[test]
fn adding_an_email_account_in_settings_is_read_by_mail() {
    // Settings writes and Mail reads the same explicit config root, so the
    // test never touches the user's accounts and needs no environment.
    let root_dir = tempfile::tempdir().unwrap();
    let tilo_root = root_dir.path().join("tilo");

    // A fresh install: no accounts yet.
    assert!(
        accounts::AccountStore::load_in(&tilo_root).is_empty(),
        "precondition: the shared store starts empty"
    );

    // Drive the real Settings pane exactly as the UI would.
    let mut pane = AccountsPane::in_root(AccountRoot::at(&tilo_root))
        .with_account_tester(std::sync::Arc::new(AcceptAccount));
    let mut doc = AutoCommit::new();
    let root = automerge::ROOT;

    // 1) Click "Add" → the inline email form opens.
    let _ = doc.put(&root, ADD_MAIL_KEY, true);
    pane.sync(&mut doc, &root);
    assert!(pane.form_open(), "Add opens the inline form");

    // 2) Fill the form (a Gmail address so IMAP/SMTP autofill kicks in).
    pane.set_form_fields_for_test("Ada Personal", "ada@gmail.com", "", "");
    pane.set_form_password_for_test("s3cret-app-password");

    // 3) Click "Save" → the pane builds the account, autofills the servers,
    //    persists it to the shared store, and seals the password in the vault.
    pane.submit_form_for_test();
    assert!(
        pane.finish_form_submission_for_test(&mut doc, &root),
        "bounded account probe and disk transaction complete"
    );
    assert!(!pane.form_open(), "a successful save closes the form");

    // ---- Now read it back the way MAIL does, through Mail's own loader. ----
    let mail_store = mail::account::AccountStore::load_in(&tilo_root);
    assert_eq!(
        mail_store.len(),
        1,
        "Mail sees exactly the account Settings added"
    );

    let acct = mail_store.active_account().expect("an active account");
    assert_eq!(
        acct.config.email, "ada@gmail.com",
        "the address round-trips"
    );
    assert_eq!(
        acct.config.imap_host, "imap.gmail.com",
        "Settings' autofill reached Mail via the shared store"
    );
    assert_eq!(acct.config.smtp_host, "smtp.gmail.com");
    assert!(acct.config.imap_tls, "TLS defaults survive the round-trip");

    // The password must NOT be in the plaintext store, but MUST be rehydrated
    // from the sealed vault when Mail loads — the credential genuinely made it
    // across, encrypted at rest.
    let store_json =
        std::fs::read_to_string(accounts::store_path_in(&tilo_root)).expect("store.json exists");
    assert!(
        !store_json.contains("s3cret-app-password"),
        "the password never touches the plaintext store: {store_json}"
    );
    assert_eq!(
        acct.config.password, "s3cret-app-password",
        "Mail rehydrates the password from the shared secret vault"
    );
}
