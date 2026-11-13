//! End-to-end proof of the Accounts → Mail loop.

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

    assert!(
        accounts::AccountStore::load_in(&tilo_root).is_empty(),
        "precondition: the shared store starts empty"
    );

    let mut pane = AccountsPane::in_root(AccountRoot::at(&tilo_root))
        .with_account_tester(std::sync::Arc::new(AcceptAccount));

    pane.command(AccountViewCommand::Add(AccountKind::Mail));
    assert!(pane.form_open(), "Add opens the inline form");

    // 2) Fill the form (a Gmail address so IMAP/SMTP autofill kicks in).
    pane.set_form_fields_for_test("Ada Personal", "ada@gmail.com", "", "");
    pane.set_form_password_for_test("s3cret-app-password");

    pane.submit_form_for_test();
    assert!(
        pane.finish_form_submission_for_test(tilo_test_env::hang_bound()),
        "bounded account probe and disk transaction complete"
    );
    assert!(!pane.form_open(), "a successful save closes the form");

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
