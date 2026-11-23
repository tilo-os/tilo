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

type Window = ui::framework::testing::TestApp<settings::SettingsShell>;
type Accounts = ui::framework::WorkspaceResource<accounts::person::AccountList>;

/// `person`'s Settings window in the test workspace, the workspace's
/// accounts record it keeps, and the accounts root over it at `tilo_root`.
fn member(
    person: &str,
    tilo_root: &std::path::Path,
) -> Result<(Window, Accounts, AccountRoot), String> {
    let mut window = Window::for_person(person).map_err(|error| error.to_string())?;
    let record = window
        .shared_workspace::<accounts::person::AccountList>(accounts::person::ACCOUNTS_OWNER)
        .ok_or("Settings keeps the workspace's accounts")?;
    let root = AccountRoot::at(tilo_root).with_person(record.clone());
    Ok((window, record, root))
}

/// An email account one member adds in Settings is the workspace's: another
/// member's Settings lists it once the workspace delivers the record, and no
/// device keeps a copy of its own.
#[test]
fn an_email_account_added_by_one_member_is_listed_for_another() {
    let ada_dir = tempfile::tempdir().unwrap();
    let bo_dir = tempfile::tempdir().unwrap();
    let (mut ada, ada_accounts, ada_root) =
        member(&"ad".repeat(32), &ada_dir.path().join("tilo")).unwrap();
    let (mut bo, bo_accounts, bo_root) =
        member(&"b0".repeat(32), &bo_dir.path().join("tilo")).unwrap();

    let mut pane =
        AccountsPane::in_root(ada_root).with_account_tester(std::sync::Arc::new(AcceptAccount));
    pane.command(AccountViewCommand::Add(AccountKind::Mail));
    assert!(pane.form_open(), "Add opens the inline form");
    // A Gmail address, so the servers are worked out from it.
    pane.set_form_fields_for_test("Ada Personal", "ada@gmail.com", "", "");
    pane.set_form_password_for_test("s3cret-app-password");
    pane.submit_form_for_test();
    assert!(
        pane.finish_form_submission_for_test(tilo_test_env::hang_bound()),
        "bounded account probe and save complete"
    );
    assert!(!pane.form_open(), "a successful save closes the form");

    let email = accounts::AccountTypeId::from("email");
    assert_eq!(ada_accounts.get().of_type(email.as_str()).len(), 1);
    assert!(bo_root.store().is_empty(), "not delivered yet");
    ada.deliver_resource_to(&mut bo, ada_accounts.id())
        .expect("the other member holds the workspace's accounts");

    let shared = bo_accounts.get();
    let listed = shared.of_type(email.as_str());
    assert_eq!(listed.len(), 1, "exactly the account Ada added");
    let config = listed[0]
        .config_as::<serde_json::Value>()
        .expect("the connection config deserialises");
    assert_eq!(config["email"], "ada@gmail.com", "the address round-trips");
    assert_eq!(config["imap_host"], "imap.gmail.com");
    assert_eq!(config["smtp_host"], "smtp.gmail.com");
    assert_eq!(config["imap_tls"], true);
    assert!(
        !serde_json::to_string(&config)
            .unwrap()
            .contains("s3cret-app-password"),
        "the password never reaches the record: {config}"
    );
    assert_eq!(
        settings::accounts::AccountDirectory::load_in(&bo_root)
            .views_of(AccountKind::Mail)
            .len(),
        1,
        "Bo's Settings lists it"
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

/// The one email account the workspace's accounts keep, as it is stored.
fn stored_mail_config(accounts: &Accounts) -> serde_json::Value {
    let shared = accounts.get();
    let listed = shared.of_type(accounts::AccountTypeId::from("email").as_str());
    assert_eq!(listed.len(), 1, "exactly the account Settings added");
    listed
        .first()
        .and_then(accounts::Account::config_as::<serde_json::Value>)
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
    let (_window, record, root) = member(&"ad".repeat(32), &tilo_root).unwrap();
    let mut pane = AccountsPane::in_root(root).with_account_tester(checked.clone());

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
    assert!(record.get().is_empty());

    pane.set_form_mail_servers_for_test("mail.example.org:143", "mail.example.org:587");
    pane.submit_form_for_test();
    assert!(
        pane.finish_form_submission_for_test(tilo_test_env::hang_bound()),
        "{:?}",
        pane.form_error_for_test()
    );
    let config = stored_mail_config(&record);
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
    // It names another incoming server now. With no password entered for it
    // the form asks for one, and nothing is checked against that server with
    // the password kept for the first.
    let checks = checked.0.lock().unwrap().len();
    pane.submit_form_for_test();
    assert!(!pane.finish_form_submission_for_test(tilo_test_env::hang_bound()));
    assert!(
        pane.form_error_for_test()
            .is_some_and(|error| error.contains("another server")),
        "{:?}",
        pane.form_error_for_test()
    );
    assert_eq!(checked.0.lock().unwrap().len(), checks);
    pane.set_form_password_for_test("s3cret-app-password");
    pane.submit_form_for_test();
    assert!(
        pane.finish_form_submission_for_test(tilo_test_env::hang_bound()),
        "{:?}",
        pane.form_error_for_test()
    );
    let config = stored_mail_config(&record);
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
    let (_window, record, root) = member(&"ad".repeat(32), &tilo_root).unwrap();
    let mut pane =
        AccountsPane::in_root(root).with_account_tester(std::sync::Arc::new(AcceptAccount));
    pane.command(AccountViewCommand::Add(AccountKind::Mail));
    pane.set_form_fields_for_test("Ada", "ada@example.org", "", "");
    pane.set_form_password_for_test("s3cret-app-password");
    pane.submit_form_for_test();
    assert!(pane.finish_form_submission_for_test(tilo_test_env::hang_bound()));
    let config = stored_mail_config(&record);
    assert_eq!(config["imap_host"], "imap.example.org");
    assert_eq!(config["imap_port"], 993);
    assert_eq!(config["imap_tls"], true);
    assert_eq!(config["smtp_host"], "smtp.example.org");
    assert_eq!(config["smtp_port"], 465);
}
