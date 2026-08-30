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
    reason = "a test that unwraps is stating an invariant"
)]

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

use automerge::AutoCommit;
use automerge::transaction::Transactable;

use settings::panes::accounts::{
    ADD_MAIL_KEY, AccountsPane, FORM_EMAIL_KEY, FORM_NAME_KEY, FORM_PASSWORD_KEY, FORM_SAVE_KEY,
    FORM_USER_KEY,
};
use ui::widget::Widget;

/// A process-unique temp config root so this test never collides with a real
/// `~/.config/qos` or a parallel run.
fn unique_config_root() -> PathBuf {
    static N: AtomicU32 = AtomicU32::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "qos-settings-mailloop-{}-{}",
        std::process::id(),
        n
    ))
}

#[test]
fn adding_an_email_account_in_settings_is_read_by_mail() {
    let root_dir = unique_config_root();
    let _ = std::fs::remove_dir_all(&root_dir);
    std::fs::create_dir_all(&root_dir).unwrap();

    // Point every shared-store path resolver (Settings' write + Mail's read)
    // at the throwaway config root. `set_var` is `unsafe` on edition 2024;
    // this test binary is single-threaded around this mutation.
    unsafe {
        std::env::set_var("XDG_CONFIG_HOME", &root_dir);
    }

    // The shared crate resolves `<XDG_CONFIG_HOME>/qos` as its config root.
    let qos_root = root_dir.join("qos");

    // A fresh install: no accounts yet.
    assert!(
        accounts::AccountStore::load_in(&qos_root).is_empty(),
        "precondition: the shared store starts empty"
    );

    // Drive the real Settings pane exactly as the UI would.
    let mut pane = AccountsPane::new();
    let mut doc = AutoCommit::new();
    let root = automerge::ROOT;

    // 1) Click "Add" → the inline email form opens.
    let _ = doc.put(&root, ADD_MAIL_KEY, true);
    pane.sync(&mut doc, &root);
    assert!(pane.form_open(), "Add opens the inline form");

    // 2) Fill the form (a Gmail address so IMAP/SMTP autofill kicks in).
    let _ = doc.put(&root, FORM_NAME_KEY, "Ada Personal");
    let _ = doc.put(&root, FORM_EMAIL_KEY, "ada@gmail.com");
    let _ = doc.put(&root, FORM_USER_KEY, "");
    let _ = doc.put(&root, FORM_PASSWORD_KEY, "s3cret-app-password");

    // 3) Click "Save" → the pane builds the account, autofills the servers,
    //    persists it to the shared store, and seals the password in the vault.
    let _ = doc.put(&root, FORM_SAVE_KEY, true);
    pane.sync(&mut doc, &root);
    assert!(!pane.form_open(), "a successful save closes the form");

    // ---- Now read it back the way MAIL does, through Mail's own loader. ----
    let mail_store = mail::account::AccountStore::load_in(&qos_root);
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
        std::fs::read_to_string(accounts::store_path_in(&qos_root)).expect("store.json exists");
    assert!(
        !store_json.contains("s3cret-app-password"),
        "the password never touches the plaintext store: {store_json}"
    );
    assert_eq!(
        acct.config.password, "s3cret-app-password",
        "Mail rehydrates the password from the shared secret vault"
    );

    // Cleanup.
    unsafe {
        std::env::remove_var("XDG_CONFIG_HOME");
    }
    let _ = std::fs::remove_dir_all(&root_dir);
}
