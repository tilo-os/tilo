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

/// A tester that accepts every account and keeps what it was asked to check.
#[derive(Default)]
struct Checked(std::sync::Mutex<Vec<serde_json::Value>>);

impl settings::account_probe::AccountTester for Checked {
    fn test(
        &self,
        account: &accounts::Account,
        _secret: &accounts::Secret,
    ) -> Result<String, String> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(account.config.clone());
        Ok("signed in".into())
    }
}

/// The one email account the shared store keeps under `root`, as it is stored.
fn stored_mail_config(root: &std::path::Path) -> serde_json::Value {
    let shared = accounts::AccountStore::load_in(root);
    let listed = shared.list(&accounts::AccountTypeId::from("email"));
    assert_eq!(listed.len(), 1, "exactly the account Settings added");
    listed
        .first()
        .and_then(|account| account.config_as::<serde_json::Value>())
        .unwrap_or_default()
}

/// A person who runs their own mail server names it, with its port. The
/// account is checked against and kept with the servers as written, a
/// connection on IMAP's port 143 is one that upgrades, and editing shows the
/// servers as they are kept.
#[test]
fn a_mail_account_keeps_the_servers_its_person_wrote() {
    let root_dir = tempfile::tempdir().unwrap();
    let tilo_root = root_dir.path().join("tilo");
    let checked = std::sync::Arc::new(Checked::default());
    let mut pane =
        AccountsPane::in_root(AccountRoot::at(&tilo_root)).with_account_tester(checked.clone());

    // What is no server name is said, and nothing is checked or kept.
    pane.command(AccountViewCommand::Add(AccountKind::Mail));
    pane.set_form_fields_for_test("Ada", "ada@example.org", "", "");
    pane.set_form_mail_servers_for_test("imaps://mail.example.org", "");
    pane.set_form_password_for_test("s3cret-app-password");
    pane.submit_form_for_test();
    assert!(
        pane.form_open(),
        "the form stays for the server to be fixed"
    );
    assert!(
        pane.form_error_for_test()
            .is_some_and(|error| error.contains("colon")),
        "{:?}",
        pane.form_error_for_test()
    );
    assert!(checked.0.lock().unwrap().is_empty());
    assert!(accounts::AccountStore::load_in(&tilo_root).is_empty());

    pane.set_form_mail_servers_for_test("mail.example.org:143", "mail.example.org:587");
    pane.submit_form_for_test();
    assert!(
        pane.finish_form_submission_for_test(tilo_test_env::hang_bound()),
        "{:?}",
        pane.form_error_for_test()
    );
    let config = stored_mail_config(&tilo_root);
    assert_eq!(config["imap_host"], "mail.example.org");
    assert_eq!(config["imap_port"], 143);
    assert_eq!(
        config["imap_tls"], false,
        "port 143 starts in the clear and upgrades"
    );
    assert_eq!(config["smtp_host"], "mail.example.org");
    assert_eq!(config["smtp_port"], 587);
    // The check ran against the servers as written, not against a guess.
    let asked = checked.0.lock().unwrap().last().cloned().unwrap();
    assert_eq!(asked["imap_host"], "mail.example.org");
    assert_eq!(asked["imap_port"], 143);
    assert_eq!(asked["smtp_port"], 587);

    // Editing shows the servers as they are kept. One rewritten without a
    // port is on the usual one, and one left as shown keeps what it had.
    pane.command(AccountViewCommand::Edit("ada@example.org".into()));
    assert!(pane.form_open());
    assert_eq!(
        pane.form_mail_servers_for_test(),
        (
            "mail.example.org:143".to_owned(),
            "mail.example.org:587".to_owned()
        )
    );
    pane.set_form_mail_servers_for_test("imap.example.org", "mail.example.org:587");
    pane.submit_form_for_test();
    assert!(
        pane.finish_form_submission_for_test(tilo_test_env::hang_bound()),
        "{:?}",
        pane.form_error_for_test()
    );
    let config = stored_mail_config(&tilo_root);
    assert_eq!(config["imap_host"], "imap.example.org");
    assert_eq!(config["imap_port"], 993);
    assert_eq!(config["imap_tls"], true);
    assert_eq!(config["smtp_host"], "mail.example.org");
    assert_eq!(config["smtp_port"], 587);
}

/// An address alone still adds an account: the servers are worked out from
/// it when its person names none.
#[test]
fn a_mail_account_without_written_servers_is_worked_out_from_its_address() {
    let root_dir = tempfile::tempdir().unwrap();
    let tilo_root = root_dir.path().join("tilo");
    let mut pane = AccountsPane::in_root(AccountRoot::at(&tilo_root))
        .with_account_tester(std::sync::Arc::new(AcceptAccount));
    pane.command(AccountViewCommand::Add(AccountKind::Mail));
    pane.set_form_fields_for_test("Ada", "ada@example.org", "", "");
    pane.set_form_password_for_test("s3cret-app-password");
    pane.submit_form_for_test();
    assert!(pane.finish_form_submission_for_test(tilo_test_env::hang_bound()));
    let config = stored_mail_config(&tilo_root);
    assert_eq!(config["imap_host"], "imap.example.org");
    assert_eq!(config["imap_port"], 993);
    assert_eq!(config["imap_tls"], true);
    assert_eq!(config["smtp_host"], "smtp.example.org");
    assert_eq!(config["smtp_port"], 465);
}
