//! The Settings side of cloud sign-in, driven through the pane exactly as
//! the controls drive it, against fake providers under a private config
//! root.

use std::sync::{Arc, Mutex};

use account_custody::OAuthCloud;
use account_custody::icloud::{
    ICloudAuth, ICloudAuthError, ICloudChallenge, ICloudHeaders, ICloudSessionState, ICloudSignIn,
};
use test_http::{Reply, Responder};
use zeroize::Zeroizing;

use super::*;

/// Plays the system browser: records every URL and, when `follow` is set,
/// walks the redirects back to the loopback listener.
struct FakeBrowser {
    opened: Mutex<Vec<String>>,
    follow: bool,
}

impl FakeBrowser {
    fn new(follow: bool) -> Arc<Self> {
        Arc::new(Self {
            opened: Mutex::new(Vec::new()),
            follow,
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
    let mut app = ui::framework::testing::TestApp::<crate::resource::SettingsShell>::new().unwrap();
    let record = app.person::<accounts::person::PersonAccounts>().unwrap();
    let (_dir, root) = scratch_root();
    let root = root.with_person(record.clone());
    let mut existing = accounts::Account::new("a-drive", "google-drive");
    existing.config = serde_json::json!({"kept": true});
    let mut original = accounts::AccountStore::new();
    original.upsert(existing.clone());
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
        record.get().store().get(&"a-drive".into()),
        original.get(&"a-drive".into())
    );
    assert_eq!(record.get().store().len(), 1);
    assert_eq!(root.vault().get(&"a-drive".into()), Some(old_secret));
    assert!(root.vault().get(&"z-mail".into()).is_none());
    assert!(!root.mutation_pending());
}

#[test]
fn live_pair_projection_failure_restores_person_metadata_and_keeps_recovery_intent() {
    let mut app = ui::framework::testing::TestApp::<crate::resource::SettingsShell>::new().unwrap();
    let record = app.person::<accounts::person::PersonAccounts>().unwrap();
    let (dir, root) = scratch_root();
    let root = root.with_person(record.clone());
    let first = accounts::Account::new("drive", "google-drive");
    let second = accounts::Account::new("ada@example.org", "email");
    // The live record accepts both edits; only its device projection fails.
    std::fs::create_dir_all(dir.path().join("tilo/accounts/store.json")).unwrap();
    let secret = accounts::Secret::password("sealed-credential");
    let error =
        AccountsPane::commit_accounts(&root, vec![(first, Some(&secret)), (second, Some(&secret))])
            .unwrap_err();
    assert!(error.contains("metadata rollback also failed"), "{error}");
    assert!(
        record.get().store().is_empty(),
        "published live metadata must be restored even when projection is still blocked"
    );
    assert!(root.vault().get(&"drive".into()).is_none());
    assert!(root.vault().get(&"ada@example.org".into()).is_none());
    assert!(root.mutation_pending());
    std::fs::remove_dir(dir.path().join("tilo/accounts/store.json")).unwrap();
    root.recover_mutation().unwrap();
    assert!(!root.mutation_pending());
    assert!(record.get().store().is_empty());
    assert!(accounts::AccountStore::load_in(&dir.path().join("tilo")).is_empty());
}

#[test]
fn published_google_mail_reauth_uses_browser_and_preserves_the_mailbox() {
    for identity in ["ada@example.org", "other@example.org"] {
        let dir = tempfile::tempdir().unwrap();
        let root = AccountRoot::at(dir.path());
        let mut app =
            ui::framework::testing::TestApp::<crate::resource::SettingsShell>::new().unwrap();
        let record = app.person::<accounts::person::PersonAccounts>().unwrap();
        let root = root.with_person(record);
        let mut account = accounts::Account::new("stable-mail-id", "email");
        account.set_config(&accounts::email::AccountConfig {
            email: "ada@example.org".into(),
            imap_host: "imap.gmail.com".into(),
            auth: accounts::email::AuthMethod::OAuth,
            ..Default::default()
        });
        let mut store = accounts::AccountStore::new();
        store.upsert(account.clone());
        root.save_store(&store).unwrap();
        let oauth = tilo_oauth::fake::FakeAuthorizationServer::start();
        let gmail = Responder::start(vec![
            Reply::status(200)
                .header("Content-Type", "application/json")
                .body(format!(r#"{{"emailAddress":"{identity}"}}"#)),
        ]);
        let browser = FakeBrowser::new(true);
        let mut clients = account_custody::ClientTable::defaults().clone();
        // Gmail reauthentication must work even when Drive is disabled in this build.
        clients.google.scopes.files.clear();
        let pane = AccountsPane::in_root(root.clone())
            .with_custodian(
                root.custodian()
                    .with_client_table(clients)
                    .with_issuer_endpoints(account_custody::OAuthIssuer::Google, oauth.provider())
                    .with_gmail_profile_endpoint(format!("{}/profile", gmail.url())),
            )
            .with_browser_opener(browser.clone());
        app.surface::<crate::SettingsApp, _>(|settings| {
            *settings.accounts().model().borrow_mut() = pane;
        })
        .unwrap();
        app.settle();
        app.call_on(
            "settings.account:stable-mail-id",
            "sign-in-again",
            serde_json::json!({}),
        )
        .unwrap();
        app.call_on("settings.root", "save-account-form", serde_json::json!({}))
            .unwrap();
        app.surface::<crate::SettingsApp, _>(|settings| {
            let shared = settings.accounts().model();
            let mut pane = shared.borrow_mut();
            assert!(pane.finish_sign_in_step_for_test(tilo_test_env::hang_bound()));
            if identity == "ada@example.org" {
                assert!(
                    !pane.sign_in_open(),
                    "{:?}",
                    pane.sign_in_message_for_test()
                );
            } else {
                assert!(
                    pane.sign_in_message_for_test()
                        .unwrap()
                        .contains("different mailbox")
                );
            }
        })
        .unwrap();
        assert_eq!(
            root.store().len(),
            1,
            "standalone mail reauth must not create Drive"
        );
        assert_eq!(
            root.store().get(&account.id).unwrap().config["email"],
            "ada@example.org"
        );
        assert_eq!(
            root.vault().get(&account.id).is_some(),
            identity == "ada@example.org"
        );
        let urls = browser.opened.lock().unwrap();
        assert!(urls[0].contains("mail.google.com"));
        assert!(!urls[0].contains("auth%2Fdrive"));
        drop(urls);
    }
}

fn scratch_root() -> (tempfile::TempDir, AccountRoot) {
    let dir = tempfile::tempdir().expect("scratch config root");
    let root = AccountRoot::at(dir.path().join("tilo"));
    (dir, root)
}

fn pane(
    root: &AccountRoot,
    custodian: account_custody::Custodian,
    browser: Arc<FakeBrowser>,
) -> AccountsPane {
    AccountsPane::in_root(root.clone())
        .with_custodian(custodian)
        .with_browser_opener(browser)
}

fn command(pane: &mut AccountsPane, command: AccountViewCommand) {
    pane.command(command);
    let _ = pane.poll();
}

fn only_account(root: &AccountRoot, type_id: &str) -> accounts::Account {
    let store = root.store();
    let list = store.list(&accounts::AccountTypeId::from(type_id));
    assert_eq!(list.len(), 1, "exactly one {type_id} account");
    list[0].clone()
}

#[test]
fn oauth_sign_in_through_the_pane_seals_the_grant() {
    let (_dir, root) = scratch_root();
    let server = tilo_oauth::fake::FakeAuthorizationServer::start();
    let custodian =
        root.custodian()
            .with_provider(OAuthCloud::GoogleDrive, server.provider(), "drive-client");
    let browser = FakeBrowser::new(true);
    let mut pane = pane(&root, custodian, Arc::clone(&browser));

    command(&mut pane, AccountViewCommand::Add(AccountKind::GoogleDrive));
    assert!(pane.sign_in_open());
    command(&mut pane, AccountViewCommand::Submit);
    assert!(
        PaneModel::busy(&pane),
        "the pane keeps polling while the browser is out"
    );
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
    let account = only_account(&root, account_custody::providers::GOOGLE_DRIVE_TYPE);
    assert_eq!(account.display_name, "Google Drive");
    let store_json = serde_json::to_string(&root.store()).unwrap();
    assert!(
        !store_json.contains("refresh-"),
        "no token in account metadata"
    );
    let secret = root.vault().get(&account.id).expect("sealed grant");
    assert!(secret.password.is_empty());
    assert!(
        secret
            .token
            .as_deref()
            .is_some_and(|grant| grant.contains("refresh-"))
    );
    assert_eq!(pane.directory().views_of(AccountKind::GoogleDrive).len(), 1);
}

#[test]
fn a_forged_redirect_saves_nothing() {
    let (_dir, root) = scratch_root();
    let server = tilo_oauth::fake::FakeAuthorizationServer::start();
    server.forge_state("forged");
    let custodian =
        root.custodian()
            .with_provider(OAuthCloud::Dropbox, server.provider(), "dropbox-client");
    let mut pane = pane(&root, custodian, FakeBrowser::new(true));
    command(&mut pane, AccountViewCommand::Add(AccountKind::Dropbox));
    command(&mut pane, AccountViewCommand::Submit);
    assert!(
        pane.finish_sign_in_step_for_test(tilo_test_env::hang_bound()),
        "the sign-in step never reported back within the hang bound"
    );
    assert!(pane.sign_in_open(), "the flow stays open to try again");
    assert!(
        pane.sign_in_message_for_test()
            .is_some_and(|message| message.contains("did not match")),
        "{:?}",
        pane.sign_in_message_for_test()
    );
    assert!(root.store().is_empty());
}

#[test]
fn a_provider_without_a_client_id_explains_instead_of_starting() {
    if OAuthCloud::Dropbox.client_id().resolve().is_some() {
        // This build or environment provides one; the flow is live.
        return;
    }
    let (_dir, root) = scratch_root();
    let browser = FakeBrowser::new(false);
    // A build whose registration table has no Dropbox app key.
    let custodian = root
        .custodian()
        .with_client_table(account_custody::ClientTable::default());
    let mut pane = pane(&root, custodian, Arc::clone(&browser));
    command(&mut pane, AccountViewCommand::Add(AccountKind::Dropbox));
    command(&mut pane, AccountViewCommand::Submit);
    let message = pane
        .sign_in_message_for_test()
        .unwrap_or_default()
        .to_string();
    assert!(message.contains("not configured"), "{message}");
    assert!(
        message.contains("TILO_OAUTH_DROPBOX_CLIENT_ID"),
        "{message}"
    );
    assert!(!PaneModel::busy(&pane), "nothing was started");
    assert!(browser.opened.lock().unwrap().is_empty());
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

#[test]
fn nextcloud_connects_through_login_flow_v2_without_a_typed_password() {
    let (_dir, root) = scratch_root();
    let server = fake_nextcloud();
    let browser = FakeBrowser::new(false);
    let mut pane = pane(&root, root.custodian(), Arc::clone(&browser));
    command(&mut pane, AccountViewCommand::Add(AccountKind::Nextcloud));
    assert!(
        pane.sign_in_open(),
        "Nextcloud opens the sign-in flow, not a password form"
    );
    assert!(!pane.form_open());
    pane.set_sign_in_fields_for_test("Home cloud", &server.url(), "", "");
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

#[derive(Default)]
struct FakeICloud {
    adp: bool,
}

impl ICloudAuth for FakeICloud {
    fn sign_in(&self, apple_id: &str, password: &str) -> Result<ICloudSignIn, ICloudAuthError> {
        if self.adp {
            return Err(ICloudAuthError::AdvancedDataProtection);
        }
        if password != "apple-password" {
            return Err(ICloudAuthError::InvalidCredentials);
        }
        Ok(ICloudSignIn::NeedsCode(ICloudChallenge(Zeroizing::new(
            apple_id.to_string(),
        ))))
    }

    fn verify_code(
        &self,
        challenge: &ICloudChallenge,
        code: &str,
    ) -> Result<ICloudSessionState, ICloudAuthError> {
        if code == "123456" {
            Ok(ICloudSessionState(Zeroizing::new(format!(
                "trust:{}",
                challenge.0.as_str()
            ))))
        } else {
            Err(ICloudAuthError::InvalidCode)
        }
    }

    fn resume(
        &self,
        session: &ICloudSessionState,
    ) -> Result<(ICloudSessionState, ICloudHeaders), ICloudAuthError> {
        Ok((
            session.clone(),
            ICloudHeaders {
                headers: Vec::new(),
                expires_unix: None,
            },
        ))
    }
}

#[test]
fn icloud_signs_in_with_a_verification_code_and_never_stores_the_password() {
    let (_dir, root) = scratch_root();
    let custodian = root
        .custodian()
        .with_icloud(Arc::new(FakeICloud::default()));
    let mut pane = pane(&root, custodian, FakeBrowser::new(false));
    command(&mut pane, AccountViewCommand::Add(AccountKind::ICloudDrive));
    pane.set_sign_in_fields_for_test("", "", "ada@icloud.com", "apple-password");
    command(&mut pane, AccountViewCommand::Submit);
    assert!(
        pane.finish_sign_in_step_for_test(tilo_test_env::hang_bound()),
        "the sign-in step never reported back within the hang bound"
    );
    assert!(pane.sign_in_awaiting_code_for_test());

    pane.set_sign_in_code_for_test("000000");
    command(&mut pane, AccountViewCommand::Submit);
    assert!(
        pane.finish_sign_in_step_for_test(tilo_test_env::hang_bound()),
        "the sign-in step never reported back within the hang bound"
    );
    assert!(
        pane.sign_in_message_for_test()
            .is_some_and(|message| message.contains("incorrect")),
    );
    assert!(
        pane.sign_in_awaiting_code_for_test(),
        "a wrong code can be corrected without starting over"
    );
    pane.set_sign_in_code_for_test("123 456");
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

    let account = only_account(&root, account_custody::providers::ICLOUD_DRIVE_TYPE);
    assert_eq!(account.config["identity"], "ada@icloud.com");
    let secret = root.vault().get(&account.id).unwrap();
    assert!(secret.password.is_empty());
    let grant = secret.token.unwrap_or_default();
    assert!(grant.contains("trust:ada@icloud.com"));
    assert!(!grant.contains("apple-password"));
}

#[test]
fn advanced_data_protection_is_explained() {
    let (_dir, root) = scratch_root();
    let custodian = root
        .custodian()
        .with_icloud(Arc::new(FakeICloud { adp: true }));
    let mut pane = pane(&root, custodian, FakeBrowser::new(false));
    command(&mut pane, AccountViewCommand::Add(AccountKind::ICloudDrive));
    pane.set_sign_in_fields_for_test("", "", "ada@icloud.com", "apple-password");
    command(&mut pane, AccountViewCommand::Submit);
    assert!(
        pane.finish_sign_in_step_for_test(tilo_test_env::hang_bound()),
        "the sign-in step never reported back within the hang bound"
    );
    let message = pane.sign_in_message_for_test().unwrap_or_default();
    assert!(message.contains("Advanced Data Protection"), "{message}");
    assert!(
        message.contains("Access iCloud Data on the Web"),
        "{message}"
    );
    assert!(root.store().is_empty());
}

#[test]
fn icloud_without_a_provider_is_not_available_rather_than_broken() {
    let (_dir, root) = scratch_root();
    assert!(
        root.custodian().icloud().is_available(),
        "Settings signs in to iCloud through the iCloud provider"
    );
    // A custodian that was given no iCloud sign-in.
    let custodian =
        account_custody::Custodian::for_root(root.explicit().map(std::path::Path::to_path_buf));
    let mut pane = pane(&root, custodian, FakeBrowser::new(false));
    command(&mut pane, AccountViewCommand::Add(AccountKind::ICloudDrive));
    command(&mut pane, AccountViewCommand::Submit);
    assert!(
        pane.sign_in_message_for_test()
            .is_some_and(|message| message.contains("not available")),
    );
    assert!(!PaneModel::busy(&pane));
}

#[test]
fn icloud_rows_are_marked_best_effort_and_offer_reauthentication() {
    fn collect(widget: &dyn Widget, texts: &mut Vec<String>) {
        if let Some(node) = widget.accessibility() {
            texts.extend(node.name);
            texts.extend(node.value);
        }
        for child in widget.children() {
            collect(child, texts);
        }
    }

    let (_dir, root) = scratch_root();
    let id = accounts::AccountId::from("icloud-drive-1");
    let mut account =
        accounts::Account::new(id.clone(), account_custody::providers::ICLOUD_DRIVE_TYPE);
    account.display_name = "My iCloud".into();
    account.config = serde_json::json!({"identity": "ada@icloud.com"});
    let mut store = accounts::AccountStore::new();
    store.upsert(account);
    root.save_store(&store).unwrap();
    root.sync_reporter(id).needs_authentication().unwrap();

    let pane = AccountsPane::in_root(root);
    let theme = ui::Theme::dark();
    let mut taffy: taffy::TaffyTree<()> = taffy::TaffyTree::new();
    let mut node_map = std::collections::HashMap::new();
    let mut composed = pane.build_composition();
    let _ = composed.build_layout(&mut taffy, &theme, &mut node_map);

    let mut texts = Vec::new();
    collect(composed.as_ref(), &mut texts);
    assert!(
        texts.iter().any(|text| text == "Re-authenticate"),
        "{texts:?}"
    );
    assert!(texts.iter().any(|text| text == "Best effort"), "{texts:?}");
    assert!(
        !texts.iter().any(|text| text == "Edit"),
        "cloud storage accounts have no password form to edit"
    );
}
