//! End-to-end proof of the Accounts → Mail loop.
//!
//! Adding an email account in the Settings "External Accounts" pane must
//! persist to the shared account store + secret vault, in the shape the mail
//! account loader reads at startup. This test drives the real Settings `sync`
//! path (open form → fill fields → Save) — the same code the live UI runs —
//! then reads the result straight back from the shared `accounts` crate on
//! disk: the typed account record, its flattened connection config, and the
//! sealed password. No mocks: the two apps meet at that store.

use settings::accounts::AccountKind;
use settings::accounts::AccountRoot;
use settings::panes::accounts::{AccountViewCommand, AccountsPane};

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
fn adding_an_email_account_in_settings_lands_in_the_shared_store() {
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

    // 1) Click "Add" → the inline email form opens.
    pane.command(AccountViewCommand::Add(AccountKind::Mail));
    assert!(pane.form_open(), "Add opens the inline form");

    // 2) Fill the form (a Gmail address so IMAP/SMTP autofill kicks in).
    pane.set_form_fields_for_test("Ada Personal", "ada@gmail.com", "", "");
    pane.set_form_password_for_test("s3cret-app-password");

    // 3) Click "Save" → the pane builds the account, autofills the servers,
    //    persists it to the shared store, and seals the password in the vault.
    pane.submit_form_for_test();
    assert!(
        pane.finish_form_submission_for_test(),
        "bounded account probe and disk transaction complete"
    );
    assert!(!pane.form_open(), "a successful save closes the form");

    // ---- Read it back the way a consumer of the shared store does. ----
    let shared = accounts::AccountStore::load_in(&tilo_root);
    let email = accounts::AccountTypeId::from("email");
    let listed = shared.list(&email);
    assert_eq!(listed.len(), 1, "exactly the account Settings added");
    let stored = listed[0];
    assert_eq!(
        shared.active_id(&email),
        Some(&stored.id),
        "the first account becomes the active one"
    );

    let config = stored
        .config_as::<serde_json::Value>()
        .expect("the connection config deserialises");
    assert_eq!(config["email"], "ada@gmail.com", "the address round-trips");
    assert_eq!(
        config["imap_host"], "imap.gmail.com",
        "Settings' autofill reached the shared store"
    );
    assert_eq!(config["smtp_host"], "smtp.gmail.com");
    assert_eq!(
        config["imap_tls"], true,
        "TLS defaults survive the round-trip"
    );

    // The password must NOT be in the plaintext store, but MUST be readable
    // from the sealed vault: the credential genuinely made it across,
    // encrypted at rest.
    let store_json =
        std::fs::read_to_string(accounts::store_path_in(&tilo_root)).expect("store.json exists");
    assert!(
        !store_json.contains("s3cret-app-password"),
        "the password never touches the plaintext store: {store_json}"
    );
    assert_eq!(
        accounts::SecretVault::open_in(&tilo_root)
            .get(&stored.id)
            .map(|secret| secret.password),
        Some("s3cret-app-password".to_string()),
        "the password is sealed in the shared secret vault"
    );
}
