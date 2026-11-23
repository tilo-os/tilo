use std::sync::{Arc, Mutex};

use test_http::{Reply, Responder};

use super::*;

/// Records the pages the pane asks the system to open, like the portal would; following one goes
/// where a browser would, so an `OAuth` sign-in finishes.
struct FakeBrowser {
    opened: Mutex<Vec<String>>,
    follow: bool,
}

impl FakeBrowser {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            opened: Mutex::new(Vec::new()),
            follow: false,
        })
    }

    fn following() -> Arc<Self> {
        Arc::new(Self {
            opened: Mutex::new(Vec::new()),
            follow: true,
        })
    }
}

impl BrowserOpener for FakeBrowser {
    fn open(&self, url: &str) -> ui::portal::LaunchChannel {
        self.opened.lock().unwrap().push(url.to_string());
        if self.follow {
            let url = url.to_string();
            std::thread::spawn(move || {
                let _ = tilo_oauth::fake::browse(&url);
            });
        }
        let (sender, receiver) = std::sync::mpsc::channel();
        let _ = sender.send(ui::portal::LaunchOutcome::Opened);
        receiver
    }
}

#[test]
fn live_second_account_refusal_restores_already_published_metadata() {
    let (_dir, root) = scratch_root();
    let record = root.record_for_test();
    let mut existing = accounts::Account::new("a-cloud", "nextcloud");
    existing.config = serde_json::json!({"kept": true});
    let mut original = accounts::person::AccountList::default();
    original.keep(&existing).unwrap();
    root.save_store(&original).unwrap();
    let old_secret = accounts::Secret::password("old-credential");
    let mut vault = root.vault();
    vault.put(&existing.id, &old_secret);
    vault.try_save().unwrap();
    existing.display_name = "Updated before refusal".into();
    let invalid = accounts::Account::new("z-mail", "");
    let replacement = accounts::Secret::password("replacement");
    let error = AccountsPane::commit_accounts(
        &root,
        vec![
            (existing, Some(&replacement)),
            (invalid, Some(&replacement)),
        ],
    )
    .unwrap_err();
    assert!(
        error.contains("an account needs an id and a type"),
        "{error}"
    );
    assert_eq!(
        record.get().get(&"a-cloud".into()),
        original.get(&"a-cloud".into())
    );
    assert_eq!(record.get().len(), 1);
    assert_eq!(root.vault().get(&"a-cloud".into()), Some(old_secret));
    assert!(root.vault().get(&"z-mail".into()).is_none());
    assert!(!root.mutation_pending());
}

#[test]
fn nextcloud_connects_through_login_flow_v2_without_a_typed_password() {
    let (_dir, root) = scratch_root();
    let server = fake_nextcloud();
    let browser = FakeBrowser::new();
    let mut pane = pane(&root, root.credential_store(), Arc::clone(&browser));
    command(&mut pane, AccountViewCommand::Add(AccountKind::Nextcloud));
    assert!(
        pane.sign_in_open(),
        "Nextcloud opens the sign-in flow, not a password form"
    );
    assert!(!pane.form_open());
    pane.set_sign_in_fields_for_test("Home cloud", &server.url());
    command(&mut pane, AccountViewCommand::Submit);
    assert!(
        pane.finish_sign_in_step_for_test(tilo_test_env::hang_bound()),
        "the sign-in step never reported back within the hang bound"
    );
    assert!(
        !pane.sign_in_open(),
        "{:?}",
        pane.sign_in_message_for_test()
    );

    assert_eq!(
        browser.opened.lock().unwrap().clone(),
        vec![format!("{}/login/v2/flow/xyz", server.url())]
    );
    let account = only_account(&root, crate::accounts::NEXTCLOUD_TYPE);
    assert_eq!(account.display_name, "Home cloud");
    assert_eq!(account.config["username"], "ada");
    assert_eq!(account.config["server_url"], server.url());
    assert_eq!(account.config["app_password"], true);
    assert_eq!(account.config["use_files"], true);
    assert_eq!(
        root.vault().get(&account.id).map(|secret| secret.password),
        Some("minted-app-password".into())
    );
    assert_eq!(pane.directory().views_of(AccountKind::Nextcloud).len(), 1);
}

fn scratch_root() -> (tempfile::TempDir, AccountRoot) {
    let dir = tempfile::tempdir().expect("scratch config root");
    let root = AccountRoot::scratch_at(dir.path().join("tilo"));
    (dir, root)
}

fn pane(
    root: &AccountRoot,
    credential_store: account_credentials::CredentialStore,
    browser: Arc<FakeBrowser>,
) -> AccountsPane {
    AccountsPane::in_root(root.clone())
        .with_credential_store(credential_store)
        .with_browser_opener(browser)
}

fn command(pane: &mut AccountsPane, command: AccountViewCommand) {
    pane.command(command);
    let _ = pane.poll();
}

fn only_account(root: &AccountRoot, type_id: &str) -> accounts::Account {
    let store = root.store();
    let list = store.of_type(accounts::AccountTypeId::from(type_id).as_str());
    assert_eq!(list.len(), 1, "exactly one {type_id} account");
    list[0].clone()
}

#[test]
fn a_cloud_drive_is_added_by_its_provider_page_with_the_files_mode_chosen() {
    let (_dir, root) = scratch_root();
    let server = tilo_oauth::fake::FakeAuthorizationServer::start();
    let credential_store = root.credential_store().with_provider(
        account_credentials::OAuthCloud::GoogleDrive,
        server.provider(),
        "drive-client",
    );
    let browser = FakeBrowser::following();
    let mut pane = pane(&root, credential_store, Arc::clone(&browser));
    command(&mut pane, AccountViewCommand::Add(AccountKind::GoogleDrive));
    assert!(pane.sign_in_open());
    pane.select_files_mode("import").unwrap();
    command(&mut pane, AccountViewCommand::Submit);
    assert!(
        pane.finish_sign_in_step_for_test(tilo_test_env::hang_bound()),
        "the sign-in step never reported back within the hang bound"
    );
    assert!(
        !pane.sign_in_open(),
        "{:?}",
        pane.sign_in_message_for_test()
    );
    let opened = browser.opened.lock().unwrap().clone();
    assert_eq!(opened.len(), 1);
    assert!(opened[0].starts_with(&format!("{}/authorize?", server.url())));
    let account = only_account(&root, accounts::cloud_types::GOOGLE_DRIVE);
    assert_eq!(
        accounts::cloud_types::files_mode(&account),
        Some(accounts::cloud_types::FilesMode::Import)
    );
    assert_eq!(account.config["provider"], "google-drive");
    let secret = root.vault().get(&account.id).expect("sealed grant");
    assert!(secret.password.is_empty(), "no token in the account");
    assert!(secret.token.is_some_and(|grant| grant.contains("refresh-")));
    assert_eq!(pane.directory().views_of(AccountKind::GoogleDrive).len(), 1);
}

#[test]
fn a_cloud_drives_files_mode_changes_without_another_consent() {
    let (_dir, root) = scratch_root();
    let server = tilo_oauth::fake::FakeAuthorizationServer::start();
    let credential_store = root.credential_store().with_provider(
        account_credentials::OAuthCloud::GoogleDrive,
        server.provider(),
        "drive-client",
    );
    let browser = FakeBrowser::following();
    let mut pane = pane(&root, credential_store, Arc::clone(&browser));
    command(&mut pane, AccountViewCommand::Add(AccountKind::GoogleDrive));
    pane.select_files_mode("import").unwrap();
    command(&mut pane, AccountViewCommand::Submit);
    assert!(pane.finish_sign_in_step_for_test(tilo_test_env::hang_bound()));
    let account = only_account(&root, accounts::cloud_types::GOOGLE_DRIVE);
    assert_eq!(
        accounts::cloud_types::files_mode(&account),
        Some(accounts::cloud_types::FilesMode::Import)
    );
    let opened = browser.opened.lock().unwrap().len();
    command(
        &mut pane,
        AccountViewCommand::Reauthenticate(account.id.to_string()),
    );
    assert!(pane.sign_in_open());
    pane.select_files_mode("sync").unwrap();
    command(&mut pane, AccountViewCommand::Submit);
    assert!(pane.finish_sign_in_step_for_test(tilo_test_env::hang_bound()));
    assert_eq!(
        browser.opened.lock().unwrap().len(),
        opened,
        "changing the mode asks for no new consent"
    );
    let kept = only_account(&root, accounts::cloud_types::GOOGLE_DRIVE);
    assert_eq!(kept.id, account.id);
    assert_eq!(
        accounts::cloud_types::files_mode(&kept),
        Some(accounts::cloud_types::FilesMode::Sync)
    );
}

fn fake_nextcloud() -> Responder {
    let base = Arc::new(Mutex::new(String::new()));
    let seen = Arc::clone(&base);
    let responder = Responder::handle(move |request| {
        let base = seen.lock().unwrap().clone();
        Some(match (request.method.as_str(), request.target.as_str()) {
            ("POST", "/index.php/login/v2") => Reply::status(200).body(
                serde_json::json!({
                    "poll": {"token": "poll-token", "endpoint": format!("{base}/login/v2/poll")},
                    "login": format!("{base}/login/v2/flow/xyz"),
                })
                .to_string(),
            ),
            ("POST", "/login/v2/poll") => Reply::status(200).body(
                serde_json::json!({
                    "server": base,
                    "loginName": "ada",
                    "appPassword": "minted-app-password",
                })
                .to_string(),
            ),
            _ => Reply::status(404),
        })
    });
    *base.lock().unwrap() = responder.url();
    responder
}
