use super::*;
use crate::setup::account_transfer::Connector;
use accounts::SecretVault;
use std::sync::Mutex;

#[test]
fn detected_foreign_profiles_do_not_offer_or_prepare_a_replacement_login() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let config = home.join(".config");
    for relative in ["Slack", "discord"] {
        std::fs::create_dir_all(config.join(relative)).unwrap();
        std::fs::write(config.join(relative).join("session"), b"private original").unwrap();
    }
    let roots = SourceRoots { home, config };
    let inventory = super::super::super::account_transfer::discover(&roots);
    assert_eq!(inventory.candidates.len(), 2);
    let ids = inventory
        .candidates
        .iter()
        .map(|candidate| {
            assert!(!candidate.can_import_credentials);
            assert!(!can_connect_candidate(candidate));
            candidate.id.clone()
        })
        .collect::<Vec<_>>();
    let browser = Browser::new(false);
    let mut accounts = Accounts::default();
    accounts.opener = browser.clone();
    accounts.update_sources(roots.clone(), inventory).unwrap();
    for id in ids {
        assert_eq!(
            accounts.apply(AccountRequest::Source(id)),
            Err("installer-accounts-profile-only")
        );
        assert!(accounts.stage.is_none());
        assert!(accounts.prepared.is_empty());
    }
    assert!(browser.opened.lock().unwrap().is_empty());
    for relative in ["Slack", "discord"] {
        assert_eq!(
            std::fs::read(roots.config.join(relative).join("session")).unwrap(),
            b"private original"
        );
    }
}

struct Browser {
    opened: Mutex<Vec<String>>,
    follow: bool,
}
impl Browser {
    fn new(follow: bool) -> Arc<Self> {
        Arc::new(Self {
            opened: Mutex::new(Vec::new()),
            follow,
        })
    }
}
impl BrowserOpener for Browser {
    fn open(&self, url: &str) -> ui::portal::LaunchChannel {
        self.opened.lock().unwrap().push(url.to_string());
        if self.follow {
            let url = url.to_string();
            std::thread::spawn(move || {
                let _ = tilo_oauth::fake::browse(&url);
            });
        }
        let (send, result) = mpsc::channel();
        send.send(ui::portal::LaunchOutcome::Opened).unwrap();
        result
    }
}
fn settle(accounts: &mut Accounts) {
    let until = std::time::Instant::now() + tilo_test_env::hang_bound();
    while accounts.busy() {
        accounts.poll();
        assert!(
            std::time::Instant::now() < until,
            "account work did not settle"
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

#[test]
#[cfg(unix)]
fn reusable_account_is_prepared_only_after_confirm_and_handoff_reseals_it() {
    let dav_server = crate::setup::account_transfer::dav_readiness::test_server();
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("source-home");
    let config = home.join(".config");
    std::fs::create_dir_all(config.join("rclone")).unwrap();
    let path = config.join("rclone/rclone.conf");
    let original = "[Files]\ntype=webdav\nurl=https://files.example.test/dav\nuser=ada\npass=AAECAwQFBgcICQoLDA0OD2f2H1FyoCWnrFF4SHSBvFFHjlw9\n";
    std::fs::write(&path, original).unwrap();
    let other = Connector::new(config.join("tilo"))
        .connect_webdav(
            "Other files",
            &format!("{}/dav", dav_server.url()),
            "other-user",
            zeroize::Zeroizing::new("other-app-password".into()),
        )
        .unwrap();
    let source_account_metadata =
        std::fs::read(accounts::store_path_in(&config.join("tilo"))).unwrap();
    let roots = SourceRoots {
        home: home.clone(),
        config,
    };
    let inventory = super::super::super::account_transfer::discover(&roots);
    let id = inventory.candidates[0].id.clone();
    let browser = Browser::new(false);
    let mut accounts = Accounts::default();
    accounts.opener = browser.clone();
    accounts.update_sources(roots, inventory).unwrap();
    accounts.view(Rc::new(|_| {}), Rc::new(|_, _| {}));
    accounts.apply(AccountRequest::Source(id)).unwrap();
    assert!(matches!(accounts.step, Step::Confirm(Selection::Saved(_))));
    assert!(accounts.stage.is_none());
    assert!(browser.opened.lock().unwrap().is_empty());
    accounts.apply(AccountRequest::Confirm).unwrap();
    accounts.apply(AccountRequest::Back).unwrap();
    settle(&mut accounts);
    assert!(matches!(accounts.step, Step::Overview));
    assert!(accounts.can_continue());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
    assert!(browser.opened.lock().unwrap().is_empty());
    let projection = accounts.projection().to_string();
    assert!(!projection.contains("tilo-prepared") && !projection.contains("fixture-app-password"));
    let prepared = accounts.prepared_handle().unwrap();
    assert!(accounts.can_continue());
    assert_eq!(prepared.count(), 1);
    assert!(!prepared.home().starts_with(&home));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(prepared.home())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
    }
    assert_eq!(
        prepared.merge_into(&home),
        Err(ConnectionError::InvalidConfiguration)
    );
    let unselected_stage = Connector::new(prepared.config_root())
        .connect_webdav(
            "Unselected staging account",
            &format!("{}/dav", dav_server.url()),
            "other",
            zeroize::Zeroizing::new("unselected-stage-password".into()),
        )
        .unwrap();
    let key = tilo_seal::SymmetricKey::generate().unwrap();
    let selected = prepared
        .entries()
        .iter()
        .map(|entry| accounts::AccountId::new(&entry.id))
        .collect::<Vec<_>>();
    let sealed = Connector::new(prepared.config_root())
        .seal_selected_accounts(&selected, &key, b"prepared-operation-1")
        .unwrap();
    let portable_target = temp.path().join("portable-target/.config/tilo");
    let imported = Connector::new(portable_target.clone())
        .import_sealed_accounts(&sealed, &key, b"prepared-operation-1")
        .unwrap();
    assert_eq!(imported.len(), 1);
    let transported = accounts::AccountStore::load_in(&portable_target);
    assert_eq!(transported.len(), 1);
    assert!(transported.get(&unselected_stage).is_none());
    assert_eq!(
        account_custody::password_credential(
            &SecretVault::open_in(&portable_target)
                .get(&imported[0])
                .unwrap()
        )
        .to_string(),
        "fixture-app-password"
    );
    let target_home = temp.path().join("target-home");
    let journal_dir = temp.path().join("private-journal");
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&journal_dir)
            .unwrap();
    }
    #[cfg(not(unix))]
    std::fs::create_dir(&journal_dir).unwrap();
    let review = super::super::super::home_transfer::review(
        &home,
        &target_home,
        &journal_dir.join("home-transfer.json"),
    )
    .unwrap();
    let mut worker = review.start().unwrap();
    let until = std::time::Instant::now() + tilo_test_env::hang_bound();
    loop {
        if let Some(super::super::super::migration_worker::WorkerEvent::Finished(outcome)) =
            worker.try_next()
        {
            assert!(matches!(
                outcome,
                super::super::super::migration_worker::WorkerOutcome::Complete(_)
            ));
            break;
        }
        assert!(std::time::Instant::now() < until);
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    prepared.merge_into(&target_home).unwrap();
    prepared.merge_into(&target_home).unwrap();
    let root = target_home.join(".config/tilo");
    let store = accounts::AccountStore::load_in(&root);
    assert_eq!(store.len(), 2);
    assert!(store.get(&other).is_some());
    let vault = accounts::SecretVault::open_in(&root);
    assert_eq!(
        account_custody::password_credential(&vault.get(&other).unwrap()).to_string(),
        "other-app-password"
    );
    let entry = store.all().iter().find(|entry| entry.id != other).unwrap();
    assert_eq!(
        account_custody::password_credential(&SecretVault::open_in(&root).get(&entry.id).unwrap())
            .to_string(),
        "fixture-app-password"
    );
    assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
    assert_eq!(
        std::fs::read(accounts::store_path_in(&home.join(".config/tilo"))).unwrap(),
        source_account_metadata
    );
    let retained = accounts.prepared_handle().unwrap();
    assert_eq!(retained.count(), 1);
    assert_eq!(retained.home(), prepared.home());
    assert!(accounts.can_continue());
    let mut installer = super::super::InstallerPreview {
        live: true,
        accounts,
        ..Default::default()
    };
    // USB preparation is admitted only after the final review, even when
    // this account-focused fixture deliberately has no storage backend.
    installer.journey.page = super::super::model::Page::Review;
    installer.journey.destination = Some(super::super::model::Destination::Another);
    installer.journey.migrate = Some(true);
    installer.journey.categories[2] = true;
    // The other computer is identified first; this fixture focuses on the
    // missing storage backend after that gate.
    for (field, value) in [
        ("manufacturer", "Prepared Vendor"),
        ("model", "Prepared Laptop 14"),
        ("variant", "PREPARED-BOARD"),
    ] {
        installer.target.set_field(field, value.into()).unwrap();
    }
    assert_eq!(
        installer.apply(super::super::InstallerPreviewEffect::PrepareUsb),
        Err("installer-preview-storage-unavailable")
    );
    assert_eq!(
        installer.accounts.prepared_handle().unwrap().home(),
        prepared.home()
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        installer.journey.password = "several unrelated installation words".into();
        let source_artifact = home.join(".config/tilo/prepared.sealed");
        let settle_export = |installer: &mut super::super::InstallerPreview| {
            let deadline = std::time::Instant::now() + tilo_test_env::hang_bound();
            while installer.personal.busy() {
                installer.drain();
                assert!(std::time::Instant::now() < deadline);
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        };
        installer
            .apply(
                super::super::InstallerPreviewEffect::ExportPreparedAccounts {
                    destination: source_artifact.to_string_lossy().into_owned(),
                    setup_binding: "installation-1".into(),
                },
            )
            .unwrap();
        settle_export(&mut installer);
        assert_eq!(installer.personal.projection()["state"], "failed");
        assert!(!source_artifact.exists());
        let artifacts = temp.path().join("private-artifacts");
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&artifacts)
            .unwrap();
        #[cfg(target_os = "linux")]
        {
            let mut env = tilo_test_env::ScopedEnv::new();
            env.set(runtime_identity::FILE_SECRETS_ENV, "1");
            installer.journey.identity = Some(super::super::model::Identity::New);
            installer.journey.name = "Ada".into();
            installer
                .apply(
                    super::super::InstallerPreviewEffect::CreatePreparedIdentity {
                        private_custody: artifacts.join("media-identity").to_str().unwrap().into(),
                        avatar_color: "143c64".into(),
                    },
                )
                .unwrap();
            settle_export(&mut installer);
            let identity = installer.personal.prepared_handle().unwrap();
            assert_eq!(
                installer.apply(super::super::InstallerPreviewEffect::PrepareUsb),
                Err("installer-preview-storage-unavailable")
            );
            assert_eq!(
                installer.personal.prepared_handle().unwrap().public_key(),
                identity.public_key()
            );
            assert_eq!(
                installer.accounts.prepared_handle().unwrap().selected_ids(),
                prepared.selected_ids()
            );
            let media = super::super::prepared_media::PreparedMedia::prepare(
                &identity,
                Some(installer.accounts.prepared_handle().unwrap()),
                zeroize::Zeroizing::new(installer.journey.password.clone()),
                "selected-account-media-handoff".into(),
            )
            .unwrap();
            assert_eq!(
                media.projection()["public_key"],
                hex::encode(identity.public_key())
            );
            assert_eq!(
                media.projection()["account_ids"],
                serde_json::json!(prepared.selected_ids())
            );
            assert!(media.expectation().valid());
            assert_eq!(
                installer.accounts.prepared_handle().unwrap().home(),
                prepared.home()
            );
        }
        let artifact = artifacts.join("accounts.sealed");
        installer
            .apply(
                super::super::InstallerPreviewEffect::ExportPreparedAccounts {
                    destination: artifact.to_string_lossy().into_owned(),
                    setup_binding: "installation-1".into(),
                },
            )
            .unwrap();
        settle_export(&mut installer);
        let receipt = installer.personal.projection();
        assert_eq!(receipt["state"], "accounts-exported", "{receipt}");
        assert_eq!(receipt["artifact"]["accounts"], 1);
        let transport_target = temp.path().join("transport-target/.config/tilo");
        let ids = Connector::new(transport_target.clone())
            .import_selected_accounts(
                &artifact,
                installer.journey.password.as_bytes(),
                b"installation-1",
            )
            .unwrap();
        assert_eq!(ids.len(), 1);
        assert!(
            accounts::AccountStore::load_in(&transport_target)
                .get(&unselected_stage)
                .is_none()
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
        assert_eq!(
            std::fs::read(accounts::store_path_in(&home.join(".config/tilo"))).unwrap(),
            source_account_metadata
        );
    }
}

fn oauth_accounts(
    follow: bool,
) -> (
    tempfile::TempDir,
    tilo_oauth::fake::FakeAuthorizationServer,
    Accounts,
    Arc<Browser>,
) {
    let temp = tempfile::tempdir().unwrap();
    let server = tilo_oauth::fake::FakeAuthorizationServer::start();
    let browser = Browser::new(follow);
    let mut accounts = Accounts::default();
    accounts.opener = browser.clone();
    accounts.actions = AccountActions::default().with_custodian(
        account_custody::Custodian::for_root(Some(temp.path().join("injected-fixture-root")))
            .with_provider(
                account_custody::OAuthCloud::GoogleDrive,
                server.provider(),
                "tilo-client",
            ),
    );
    (temp, server, accounts, browser)
}

#[test]
fn provider_page_and_render_never_launch_and_explicit_connect_completes_real_pkce() {
    let (_temp, _server, mut accounts, browser) = oauth_accounts(true);
    accounts.view(Rc::new(|_| {}), Rc::new(|_, _| {}));
    accounts
        .apply(AccountRequest::Provider("google-drive".into()))
        .unwrap();
    accounts.view(Rc::new(|_| {}), Rc::new(|_, _| {}));
    assert!(accounts.stage.is_none());
    assert!(browser.opened.lock().unwrap().is_empty());
    accounts.apply(AccountRequest::Confirm).unwrap();
    settle(&mut accounts);
    assert_eq!(browser.opened.lock().unwrap().len(), 1);
    assert_eq!(accounts.prepared.len(), 1);
    assert!(matches!(accounts.step, Step::Done(_)));
    let projection = accounts.projection().to_string();
    for secret in [
        "code_challenge",
        "code_verifier",
        "refresh_token",
        "127.0.0.1",
        "tilo-prepared-accounts",
    ] {
        assert!(!projection.contains(secret));
    }
}

#[test]
fn cancelling_unapproved_sign_in_stops_wait_and_does_not_prepare_an_account() {
    let (_temp, _server, mut accounts, browser) = oauth_accounts(false);
    accounts
        .apply(AccountRequest::Provider("google-drive".into()))
        .unwrap();
    accounts.apply(AccountRequest::Confirm).unwrap();
    assert_eq!(browser.opened.lock().unwrap().len(), 1);
    assert!(!accounts.can_continue());
    accounts.apply(AccountRequest::Back).unwrap();
    settle(&mut accounts);
    assert!(accounts.prepared.is_empty());
    assert!(accounts.can_continue());
    let stage = accounts.stage.as_ref().unwrap();
    assert!(accounts::AccountStore::load_in(&stage.path().join(".config/tilo")).is_empty());
}

#[test]
fn cancellation_racing_completed_sign_in_removes_staged_connection() {
    let (_temp, _server, mut accounts, _browser) = oauth_accounts(true);
    accounts
        .apply(AccountRequest::Provider("google-drive".into()))
        .unwrap();
    accounts.apply(AccountRequest::Confirm).unwrap();
    let until = std::time::Instant::now() + tilo_test_env::hang_bound();
    loop {
        if accounts
            .actions
            .status()
            .iter()
            .any(|job| matches!(job.state, AccountJobState::Completed { .. }))
        {
            break;
        }
        assert!(std::time::Instant::now() < until);
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    accounts.apply(AccountRequest::Cancel).unwrap();
    settle(&mut accounts);
    assert!(accounts.prepared.is_empty());
    assert!(accounts.can_continue());
    let stage = accounts.stage.as_ref().unwrap();
    assert!(accounts::AccountStore::load_in(&stage.path().join(".config/tilo")).is_empty());
}

#[test]
fn dropping_during_token_exchange_revokes_new_grant_and_preserves_originals() {
    let (_temp, server, mut controller, browser) = oauth_accounts(true);
    let source = tempfile::tempdir().unwrap();
    let config = source.path().join(".config");
    std::fs::create_dir(&config).unwrap();
    let original = config.join("original-account");
    std::fs::write(&original, b"original-account-session").unwrap();
    controller
        .update_sources(
            SourceRoots {
                home: source.path().to_path_buf(),
                config,
            },
            Inventory::default(),
        )
        .unwrap();
    let hold = server.hold_token_exchange();
    controller
        .apply(AccountRequest::Provider("google-drive".into()))
        .unwrap();
    controller.apply(AccountRequest::Confirm).unwrap();
    assert!(hold.wait_for_exchange(tilo_test_env::hang_bound()));
    let stage = controller.stage.as_ref().unwrap().clone();
    assert_eq!(browser.opened.lock().unwrap().len(), 1);
    drop(controller);
    hold.release();
    let until = std::time::Instant::now() + tilo_test_env::hang_bound();
    loop {
        let root = stage.path().join(".config/tilo");
        if !server.revoked().is_empty() && Arc::strong_count(&stage) == 1 {
            assert!(accounts::AccountStore::load_in(&root).is_empty());
            assert!(accounts::SecretVault::open_in(&root).is_empty());
            break;
        }
        assert!(
            std::time::Instant::now() < until,
            "dropped authorization did not clean up"
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert_eq!(server.revoked().len(), 1);
    assert_eq!(
        std::fs::read(original).unwrap(),
        b"original-account-session"
    );
}

#[test]
fn dav_and_nextcloud_guided_public_fields_save_exact_private_credentials_without_launch() {
    use super::super::{InstallerPreview, InstallerPreviewEffect, model::Page};
    for (provider, account_type) in [
        ("webdav", accounts::cloud_types::WEBDAV),
        ("nextcloud", accounts::cloud_types::NEXTCLOUD),
    ] {
        let dav_server = crate::setup::account_transfer::dav_readiness::test_server();
        let endpoint = format!("{}/dav", dav_server.url());
        let browser = Browser::new(false);
        let mut preview = InstallerPreview {
            live: true,
            ..InstallerPreview::default()
        };
        preview.journey.page = Page::Accounts;
        preview.accounts.opener = browser.clone();
        let choice = |step: &str, selection: Option<String>| InstallerPreviewEffect::AccountStep {
            step: step.to_string(),
            selection,
        };
        preview
            .apply(choice("choose-provider", Some(provider.into())))
            .unwrap();
        assert!(preview.accounts.stage.is_none());
        assert!(!preview.accounts.can_continue());
        assert_eq!(
            preview.apply(choice("confirm", None)),
            Err("installer-accounts-dav-required")
        );
        preview
            .apply(InstallerPreviewEffect::SetAccountField {
                field: "server".into(),
                value: "http://unsafe.example.test".into(),
            })
            .unwrap();
        assert_eq!(
            preview.apply(choice("confirm", None)),
            Err("installer-accounts-dav-server-invalid")
        );
        for (field, value) in [
            ("server", endpoint.as_str()),
            ("login", "ada"),
            ("password", "private-dav-app-password"),
        ] {
            preview
                .apply(InstallerPreviewEffect::SetAccountField {
                    field: field.into(),
                    value: value.into(),
                })
                .unwrap();
            assert!(!preview.accounts.projection().to_string().contains(value));
            if field != "password" {
                preview.apply(choice("confirm", None)).unwrap();
                if provider == "nextcloud" && field == "server" {
                    preview
                        .apply(choice("nextcloud-app-password", None))
                        .unwrap();
                }
                assert!(preview.accounts.stage.is_none());
            }
        }
        preview.accounts.view(Rc::new(|_| {}), Rc::new(|_, _| {}));
        assert!(browser.opened.lock().unwrap().is_empty());
        preview.apply(choice("confirm", None)).unwrap();
        assert!(preview.accounts.dav_fields[2].get().is_empty());
        settle(&mut preview.accounts);
        assert_eq!(preview.accounts.projection()["state"], "saved");
        assert!(browser.opened.lock().unwrap().is_empty());
        let prepared = preview.accounts.prepared_handle().unwrap();
        let id = accounts::AccountId::new(prepared.entries()[0].id.clone());
        let store = accounts::AccountStore::load_in(&prepared.config_root());
        let account = store.get(&id).unwrap();
        assert_eq!(account.type_id.as_str(), account_type);
        assert_ne!(account.config["app_password"].as_bool(), Some(true));
        assert_eq!(account.config["username"], "ada");
        assert_eq!(account.config["server_url"], endpoint);
        assert_eq!(
            account_custody::password_credential(
                &SecretVault::open_in(&prepared.config_root())
                    .get(&id)
                    .unwrap()
            )
            .to_string(),
            "private-dav-app-password"
        );
        assert!(
            !preview
                .accounts
                .projection()
                .to_string()
                .contains("private-dav-app-password")
        );
    }
}

#[test]
fn leaving_dav_password_page_clears_private_input_and_does_not_prepare_connection() {
    let mut accounts = Accounts::default();
    accounts
        .apply(AccountRequest::Provider("webdav".into()))
        .unwrap();
    for (field, value) in [
        ("server", "https://files.example.test/dav"),
        ("login", "ada"),
    ] {
        accounts
            .set_dav_field(field, zeroize::Zeroizing::new(value.into()))
            .unwrap();
        accounts.apply(AccountRequest::Confirm).unwrap();
    }
    accounts
        .set_dav_field(
            "password",
            zeroize::Zeroizing::new("never-submitted-app-password".into()),
        )
        .unwrap();
    accounts.apply(AccountRequest::Back).unwrap();
    assert!(accounts.dav_fields[2].get().is_empty());
    assert!(accounts.stage.is_none());
    assert!(accounts.prepared_handle().is_none());
}

struct NextcloudFixture {
    server: test_http::Responder,
    started: Arc<std::sync::atomic::AtomicUsize>,
    polled: Arc<std::sync::atomic::AtomicBool>,
    release: Arc<std::sync::atomic::AtomicBool>,
    revoked: Arc<std::sync::atomic::AtomicUsize>,
}
impl NextcloudFixture {
    fn new(delayed: bool) -> Self {
        Self::with_dav_status(delayed, 207)
    }
    fn with_dav_status(delayed: bool, dav_status: u16) -> Self {
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
        let started = Arc::new(AtomicUsize::new(0));
        let polled = Arc::new(AtomicBool::new(false));
        let release = Arc::new(AtomicBool::new(!delayed));
        let revoked = Arc::new(AtomicUsize::new(0));
        let base = Arc::new(Mutex::new(String::new()));
        let (calls, polling, ready, revocations, address) = (
            started.clone(),
            polled.clone(),
            release.clone(),
            revoked.clone(),
            base.clone(),
        );
        let server = test_http::Responder::handle(move |request| {
            let base = address.lock().unwrap().clone();
            match (request.method.as_str(), request.target.as_str()) {
                ("POST", "/index.php/login/v2") => {
                    calls.fetch_add(1, Ordering::SeqCst);
                    Some(test_http::Reply::status(200).body(serde_json::json!({"login":format!("{base}/login/v2/flow/consent"),"poll":{"endpoint":format!("{base}/login/v2/poll"),"token":"private-nextcloud-poll-token"}}).to_string()))
                }
                ("POST", "/login/v2/poll") => {
                    assert_eq!(request.body_text(), "token=private-nextcloud-poll-token");
                    polling.store(true, Ordering::SeqCst);
                    let until = std::time::Instant::now() + tilo_test_env::hang_bound();
                    while !ready.load(Ordering::SeqCst) {
                        assert!(std::time::Instant::now() < until, "fixture release missing");
                        std::thread::sleep(std::time::Duration::from_millis(5));
                    }
                    Some(test_http::Reply::status(200).body(serde_json::json!({"server":base,"loginName":"ada","appPassword":"private-nextcloud-minted-app-password"}).to_string()))
                }
                ("PROPFIND", "/remote.php/dav/files/ada/") => {
                    assert_eq!(request.header("depth"), Some("0"));
                    assert!(request.header("authorization").is_some());
                    Some(test_http::Reply::status(dav_status).body(
                        crate::setup::account_transfer::dav_readiness::collection(&request.target),
                    ))
                }
                ("DELETE", "/ocs/v2.php/core/apppassword") => {
                    revocations.fetch_add(1, Ordering::SeqCst);
                    Some(test_http::Reply::status(200))
                }
                _ => None,
            }
        });
        *base.lock().unwrap() = server.url();
        Self {
            server,
            started,
            polled,
            release,
            revoked,
        }
    }
}
fn choose_nextcloud_browser(accounts: &mut Accounts, server: &str) {
    accounts
        .apply(AccountRequest::Provider("nextcloud".into()))
        .unwrap();
    accounts
        .set_dav_field("server", zeroize::Zeroizing::new(server.into()))
        .unwrap();
    accounts.apply(AccountRequest::Confirm).unwrap();
    assert!(matches!(accounts.step, Step::NextcloudMethod(_)));
}
#[test]
fn nextcloud_browser_uses_canonical_login_flow_and_prepares_secret_only_after_explicit_sign_in() {
    use std::sync::atomic::Ordering;
    let cloud = NextcloudFixture::new(false);
    let browser = Browser::new(false);
    let mut accounts = Accounts::default();
    accounts.opener = browser.clone();
    choose_nextcloud_browser(&mut accounts, &cloud.server.url());
    accounts.view(Rc::new(|_| {}), Rc::new(|_, _| {}));
    assert_eq!(cloud.started.load(Ordering::SeqCst), 0);
    assert!(accounts.stage.is_none());
    assert!(browser.opened.lock().unwrap().is_empty());
    accounts.apply(AccountRequest::NextcloudBrowser).unwrap();
    settle(&mut accounts);
    assert_eq!(accounts.projection()["state"], "connected");
    assert_eq!(cloud.started.load(Ordering::SeqCst), 1);
    assert_eq!(
        browser.opened.lock().unwrap().as_slice(),
        [format!("{}/login/v2/flow/consent", cloud.server.url())]
    );
    assert_eq!(cloud.revoked.load(Ordering::SeqCst), 0);
    let handle = accounts.prepared_handle().unwrap();
    let id = accounts::AccountId::new(handle.entries()[0].id.clone());
    let store = accounts::AccountStore::load_in(&handle.config_root());
    let account = store.get(&id).unwrap();
    assert_eq!(account.type_id.as_str(), accounts::cloud_types::NEXTCLOUD);
    assert_eq!(account.config["app_password"].as_bool(), Some(true));
    assert_eq!(account.config["username"], "ada");
    assert_eq!(
        account_custody::password_credential(
            &SecretVault::open_in(&handle.config_root())
                .get(&id)
                .unwrap()
        )
        .to_string(),
        "private-nextcloud-minted-app-password"
    );
    for secret in [
        "private-nextcloud-poll-token",
        "private-nextcloud-minted-app-password",
    ] {
        assert!(!accounts.projection().to_string().contains(secret));
    }
}
#[test]
fn nextcloud_new_grant_with_unavailable_files_root_is_revoked_before_retry() {
    use std::sync::atomic::Ordering;
    let cloud = NextcloudFixture::with_dav_status(false, 503);
    let mut accounts = Accounts::default();
    accounts.opener = Browser::new(false);
    choose_nextcloud_browser(&mut accounts, &cloud.server.url());
    accounts.apply(AccountRequest::NextcloudBrowser).unwrap();
    let stage = accounts.stage.as_ref().unwrap().path().to_path_buf();
    settle(&mut accounts);
    assert!(matches!(
        accounts.step,
        Step::Failed {
            message: "installer-accounts-dav-endpoint-unavailable",
            ..
        }
    ));
    assert_eq!(cloud.started.load(Ordering::SeqCst), 1);
    assert_eq!(cloud.revoked.load(Ordering::SeqCst), 1);
    assert!(accounts.prepared.is_empty());
    assert!(!accounts.can_continue());
    assert!(!accounts::store_path_in(&stage.join(".config/tilo")).exists());
    assert!(
        !accounts
            .projection()
            .to_string()
            .contains("private-nextcloud-minted-app-password")
    );
}

#[test]
fn dropping_nextcloud_during_inflight_approval_revokes_only_the_new_app_password() {
    use std::sync::atomic::Ordering;
    let cloud = NextcloudFixture::new(true);
    let browser = Browser::new(false);
    let mut accounts = Accounts::default();
    accounts.opener = browser;
    choose_nextcloud_browser(&mut accounts, &cloud.server.url());
    accounts.apply(AccountRequest::NextcloudBrowser).unwrap();
    let stage = accounts.stage.as_ref().unwrap().path().to_path_buf();
    let until = std::time::Instant::now() + tilo_test_env::hang_bound();
    while !cloud.polled.load(Ordering::SeqCst) {
        accounts.poll();
        assert!(std::time::Instant::now() < until);
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    drop(accounts);
    cloud.release.store(true, Ordering::SeqCst);
    while stage.exists() || cloud.revoked.load(Ordering::SeqCst) == 0 {
        assert!(
            std::time::Instant::now() < until,
            "new grant cleanup did not finish"
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert_eq!(cloud.revoked.load(Ordering::SeqCst), 1);
}

#[derive(Default)]
struct InstallerICloud {
    calls: Mutex<Vec<&'static str>>,
    entered: Mutex<Option<mpsc::Sender<()>>>,
    release: Mutex<Option<mpsc::Receiver<()>>>,
    revoked: Mutex<Option<mpsc::Sender<()>>>,
}
impl account_custody::icloud::ICloudAuth for InstallerICloud {
    fn sign_in(
        &self,
        apple_id: &str,
        password: &str,
    ) -> Result<account_custody::icloud::ICloudSignIn, account_custody::icloud::ICloudAuthError>
    {
        use account_custody::icloud::*;
        self.calls.lock().unwrap().push("sign-in");
        if apple_id != "ada@icloud.test" || password != "private-apple-password" {
            return Err(ICloudAuthError::InvalidCredentials);
        }
        Ok(ICloudSignIn::NeedsCode(ICloudChallenge(
            zeroize::Zeroizing::new("private-challenge".into()),
        )))
    }
    fn verify_code(
        &self,
        challenge: &account_custody::icloud::ICloudChallenge,
        code: &str,
    ) -> Result<account_custody::icloud::ICloudSessionState, account_custody::icloud::ICloudAuthError>
    {
        use account_custody::icloud::*;
        self.calls.lock().unwrap().push("verify");
        assert_eq!(challenge.0.as_str(), "private-challenge");
        if code != "123456" {
            return Err(ICloudAuthError::InvalidCode);
        }
        let entered = self.entered.lock().unwrap().take();
        if let Some(entered) = entered {
            entered.send(()).unwrap();
        }
        let release = self.release.lock().unwrap().take();
        if let Some(release) = release {
            release.recv_timeout(tilo_test_env::hang_bound()).unwrap();
        }
        Ok(ICloudSessionState(zeroize::Zeroizing::new(
            "private-session-trust".into(),
        )))
    }
    fn resume(
        &self,
        session: &account_custody::icloud::ICloudSessionState,
    ) -> Result<
        (
            account_custody::icloud::ICloudSessionState,
            account_custody::icloud::ICloudHeaders,
        ),
        account_custody::icloud::ICloudAuthError,
    > {
        Ok((
            session.clone(),
            account_custody::icloud::ICloudHeaders {
                headers: Vec::new(),
                expires_unix: None,
            },
        ))
    }
    fn sign_out(&self, _: &account_custody::icloud::ICloudSessionState) {
        self.calls.lock().unwrap().push("sign-out");
        let revoked = self.revoked.lock().unwrap().take();
        if let Some(revoked) = revoked {
            revoked.send(()).unwrap();
        }
    }
}
fn icloud_accounts(auth: Arc<InstallerICloud>) -> Accounts {
    let mut accounts = Accounts::default();
    accounts.actions = AccountActions::default()
        .with_custodian(account_custody::Custodian::for_root(None).with_icloud(auth));
    accounts
}
fn submit_icloud_details(accounts: &mut Accounts) {
    accounts
        .apply(AccountRequest::Provider("icloud".into()))
        .unwrap();
    accounts
        .set_dav_field(
            "icloud-login",
            zeroize::Zeroizing::new("ada@icloud.test".into()),
        )
        .unwrap();
    accounts.apply(AccountRequest::Confirm).unwrap();
    accounts
        .set_dav_field(
            "icloud-password",
            zeroize::Zeroizing::new("private-apple-password".into()),
        )
        .unwrap();
    accounts.apply(AccountRequest::Confirm).unwrap();
    settle(accounts);
    assert_eq!(accounts.projection()["state"], "icloud-code");
    assert!(!accounts.can_continue());
    assert!(accounts.prepared.is_empty());
}
#[test]
fn icloud_guided_sign_in_retries_code_and_seals_only_session_trust() {
    let auth = Arc::new(InstallerICloud::default());
    let mut accounts = icloud_accounts(auth.clone());
    accounts.view(Rc::new(|_| {}), Rc::new(|_, _| {}));
    assert!(auth.calls.lock().unwrap().is_empty());
    submit_icloud_details(&mut accounts);
    accounts
        .set_dav_field("icloud-code", zeroize::Zeroizing::new("000000".into()))
        .unwrap();
    accounts.apply(AccountRequest::Confirm).unwrap();
    settle(&mut accounts);
    assert_eq!(accounts.projection()["state"], "icloud-code");
    assert!(accounts.prepared.is_empty());
    accounts
        .set_dav_field("icloud-code", zeroize::Zeroizing::new("123456".into()))
        .unwrap();
    accounts.apply(AccountRequest::Confirm).unwrap();
    settle(&mut accounts);
    assert_eq!(accounts.projection()["state"], "connected");
    assert!(accounts.can_continue());
    let prepared = accounts.prepared_handle().unwrap();
    assert_eq!(prepared.count(), 1);
    let root = prepared.config_root();
    let store = accounts::AccountStore::load_in(&root);
    assert_eq!(store.len(), 1);
    assert_eq!(
        store.all()[0].type_id.as_str(),
        accounts::cloud_types::ICLOUD_DRIVE
    );
    let secret = accounts::SecretVault::open_in(&root)
        .get(&store.all()[0].id)
        .unwrap();
    assert!(secret.password.is_empty());
    assert!(secret.token.unwrap().contains("private-session-trust"));
    assert!(!accounts.projection().to_string().contains("private-"));
    assert_eq!(*auth.calls.lock().unwrap(), ["sign-in", "verify", "verify"]);
}
#[test]
fn dropping_icloud_during_verification_compensates_new_session_without_source_changes() {
    let source = tempfile::tempdir().unwrap();
    let original = source.path().join("original-profile");
    std::fs::write(&original, b"unchanged account profile").unwrap();
    let (entered_send, entered) = mpsc::channel();
    let (release, release_receive) = mpsc::channel();
    let (revoked_send, revoked) = mpsc::channel();
    let auth = Arc::new(InstallerICloud {
        entered: Mutex::new(Some(entered_send)),
        release: Mutex::new(Some(release_receive)),
        revoked: Mutex::new(Some(revoked_send)),
        ..Default::default()
    });
    let mut accounts = icloud_accounts(auth);
    submit_icloud_details(&mut accounts);
    let stage = accounts.stage.as_ref().unwrap().clone();
    accounts
        .set_dav_field("icloud-code", zeroize::Zeroizing::new("123456".into()))
        .unwrap();
    accounts.apply(AccountRequest::Confirm).unwrap();
    entered.recv_timeout(tilo_test_env::hang_bound()).unwrap();
    drop(accounts);
    release.send(()).unwrap();
    revoked.recv_timeout(tilo_test_env::hang_bound()).unwrap();
    let root = stage.path().join(".config/tilo");
    let deadline = std::time::Instant::now() + tilo_test_env::hang_bound();
    while !accounts::AccountStore::load_in(&root).is_empty() {
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(accounts::AccountStore::load_in(&root).is_empty());
    assert_eq!(
        std::fs::read(&original).unwrap(),
        b"unchanged account profile"
    );
}

#[test]
fn removing_prepared_import_requires_confirmation_and_preserves_original_credentials() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source");
    let config = source.join(".config");
    let auth = Arc::new(InstallerICloud::default());
    let source_config = config.join("rclone/rclone.conf");
    std::fs::create_dir_all(source_config.parent().unwrap()).unwrap();
    let original = "[Files]\ntype=webdav\nurl=https://files.example.test/dav\nuser=ada\npass=AAECAwQFBgcICQoLDA0OD2f2H1FyoCWnrFF4SHSBvFFHjlw9\n";
    std::fs::write(&source_config, original).unwrap();
    let roots = SourceRoots {
        home: source,
        config,
    };
    let inventory = super::super::super::account_transfer::discover(&roots);
    let candidate = inventory
        .candidates
        .iter()
        .find(|c| c.can_import_credentials)
        .unwrap();
    let candidate_id = candidate.id.clone();
    let mut accounts = icloud_accounts(auth.clone());
    accounts.update_sources(roots, inventory).unwrap();
    accounts
        .apply(AccountRequest::Source(candidate_id.clone()))
        .unwrap();
    accounts.apply(AccountRequest::Confirm).unwrap();
    settle(&mut accounts);
    assert!(!accounts.prepared[0].revoke_on_remove);
    let staged_id = accounts.prepared[0].id.clone();
    let stage_root = accounts.prepared_handle().unwrap().config_root();
    accounts.apply(AccountRequest::Overview).unwrap();
    let request = AccountRequest::Remove(staged_id.clone());
    let (step, selection) = request.parts();
    accounts
        .apply(AccountRequest::from_parts(step, selection).unwrap())
        .unwrap();
    assert_eq!(accounts.projection()["state"], "confirm-removal");
    assert!(!accounts.can_continue());
    assert_eq!(accounts::AccountStore::load_in(&stage_root).len(), 1);
    accounts.apply(AccountRequest::Back).unwrap();
    assert_eq!(accounts.prepared.len(), 1);
    accounts.apply(AccountRequest::Remove(staged_id)).unwrap();
    accounts.apply(AccountRequest::Confirm).unwrap();
    settle(&mut accounts);
    assert_eq!(accounts.projection()["state"], "overview");
    assert!(accounts.prepared.is_empty());
    assert_eq!(
        accounts.prepared_handle().unwrap().count(),
        0,
        "explicit empty selection remains transferable"
    );
    assert!(accounts::AccountStore::load_in(&stage_root).is_empty());
    assert!(
        auth.calls.lock().unwrap().is_empty(),
        "copied source grant must not be revoked"
    );
    assert_eq!(std::fs::read_to_string(&source_config).unwrap(), original);
    // Discovery still offers the unselected source for an explicit re-import.
    accounts
        .apply(AccountRequest::Source(candidate_id))
        .unwrap();
}

#[test]
fn removing_new_icloud_connection_revokes_only_the_selected_staged_session() {
    let auth = Arc::new(InstallerICloud::default());
    let mut accounts = icloud_accounts(auth.clone());
    submit_icloud_details(&mut accounts);
    accounts
        .set_dav_field("icloud-code", zeroize::Zeroizing::new("123456".into()))
        .unwrap();
    accounts.apply(AccountRequest::Confirm).unwrap();
    settle(&mut accounts);
    let id = accounts.prepared[0].id.clone();
    let root = accounts.prepared_handle().unwrap().config_root();
    accounts.apply(AccountRequest::Remove(id)).unwrap();
    assert_eq!(*auth.calls.lock().unwrap(), ["sign-in", "verify"]);
    accounts.apply(AccountRequest::Confirm).unwrap();
    settle(&mut accounts);
    assert_eq!(
        *auth.calls.lock().unwrap(),
        ["sign-in", "verify", "sign-out"]
    );
    assert!(accounts::AccountStore::load_in(&root).is_empty());
    assert_eq!(accounts.prepared_handle().unwrap().count(), 0);
}

#[test]
fn forgetting_copied_canonical_icloud_grant_does_not_sign_out_its_original() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source");
    let target = temp.path().join("target");
    let auth = Arc::new(InstallerICloud::default());
    let source_connector = Connector::with_custodian(
        source.clone(),
        account_custody::Custodian::for_root(Some(source.clone())).with_icloud(auth.clone()),
    );
    let id = source_connector
        .finish_icloud(
            "ada@icloud.test",
            account_custody::icloud::ICloudSessionState(zeroize::Zeroizing::new(
                "source-session-trust".into(),
            )),
        )
        .unwrap();
    let bytes = std::fs::read(accounts::store_path_in(&source)).unwrap();
    let target_connector = Connector::with_custodian(
        target.clone(),
        account_custody::Custodian::for_root(Some(target.clone())).with_icloud(auth.clone()),
    );
    let copied_id = target_connector.import_tilo(&source, &id).unwrap();
    target_connector.forget_prepared(&copied_id).unwrap();
    assert!(accounts::AccountStore::load_in(&target).is_empty());
    assert!(
        accounts::SecretVault::open_in(&target)
            .get(&copied_id)
            .is_none()
    );
    assert_eq!(
        std::fs::read(accounts::store_path_in(&source)).unwrap(),
        bytes
    );
    assert!(
        accounts::SecretVault::open_in(&source)
            .get(&id)
            .unwrap()
            .token
            .unwrap()
            .contains("source-session-trust")
    );
    assert!(auth.calls.lock().unwrap().is_empty());
}

struct DropboxPaste {
    _busy: std::net::TcpListener,
    issuer: tilo_oauth::fake::FakeAuthorizationServer,
    _storage: test_http::Responder,
    accounts: Accounts,
    browser: Arc<Browser>,
}
fn dropbox_paste() -> DropboxPaste {
    let busy = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let issuer = tilo_oauth::fake::FakeAuthorizationServer::start();
    let storage = test_http::Responder::handle(|request| {
        assert_eq!(request.method, "POST");
        assert_eq!(request.target, "/2/users/get_current_account");
        Some(
            test_http::Reply::status(200).body(
                br#"{"root_info":{"root_namespace_id":"personal","home_namespace_id":"personal"}}"#
                    .to_vec(),
            ),
        )
    });
    let api = storage.url();
    let registration = account_custody::Registration {
        client_id: "tilo-dropbox-fixture".into(),
        client_secret: None,
        redirect_ports: tilo_oauth::RedirectPorts::Registered(vec![
            busy.local_addr().unwrap().port(),
        ]),
        paste_code_fallback: true,
    };
    let custodian = account_custody::Custodian::for_root(None)
        .with_provider(
            account_custody::OAuthCloud::Dropbox,
            issuer.provider(),
            registration,
        )
        .with_storage_config(
            account_custody::OAuthCloud::Dropbox,
            serde_json::json!({
                "root":"", "upload_chunk":4194304,
                "endpoints":{"api":api,"content":api,"notify":api}
            }),
        );
    let browser = Browser::new(false);
    let mut accounts = Accounts::default();
    accounts.opener = browser.clone();
    accounts.actions = AccountActions::default().with_custodian(custodian);
    DropboxPaste {
        _busy: busy,
        issuer,
        _storage: storage,
        accounts,
        browser,
    }
}

#[test]
fn dropbox_busy_redirect_ports_offer_private_code_and_import_real_authenticated_grant() {
    let mut fixture = dropbox_paste();
    let accounts = &mut fixture.accounts;
    accounts
        .apply(AccountRequest::Provider("dropbox".into()))
        .unwrap();
    assert!(fixture.browser.opened.lock().unwrap().is_empty());
    accounts.apply(AccountRequest::Confirm).unwrap();
    assert!(matches!(accounts.step, Step::PastedCode(_)));
    assert_eq!(accounts.projection()["state"], "awaiting-code");
    assert!(fixture.issuer.requests().is_empty());
    let url = fixture
        .browser
        .opened
        .lock()
        .unwrap()
        .last()
        .unwrap()
        .clone();
    let code = tilo_oauth::fake::browse(&url).unwrap();
    accounts
        .set_dav_field("oauth-code", zeroize::Zeroizing::new(code.clone()))
        .unwrap();
    let view = accounts.view(Rc::new(|_| {}), Rc::new(|_, _| {}));
    fn find<'a>(root: &'a dyn ui::widget::Widget, id: &str) -> Option<&'a dyn ui::widget::Widget> {
        if root.test_id() == Some(id) {
            return Some(root);
        }
        root.children()
            .into_iter()
            .find_map(|child| find(child, id))
    }
    let input = find(&view, "installer.account.oauth-code").unwrap();
    let accessible = input.accessibility().unwrap();
    assert!(accessible.value.is_none());
    assert!(!accounts.projection().to_string().contains(&code));
    accounts.apply(AccountRequest::SubmitCode).unwrap();
    assert!(accounts.dav_fields[2].get().is_empty());
    settle(accounts);
    assert!(matches!(accounts.step, Step::Done(_)));
    let prepared = accounts.prepared_handle().unwrap();
    let store = accounts::AccountStore::load_in(&prepared.config_root());
    let id = accounts::AccountId::new(&accounts.prepared[0].id);
    assert_eq!(store.get(&id).unwrap().type_id.as_str(), "dropbox");
    assert!(
        store
            .get(&id)
            .unwrap()
            .config
            .get(accounts::cloud_types::MOUNT_CONFIG_KEY)
            .is_some()
    );
    let secret = accounts::SecretVault::open_in(&prepared.config_root())
        .get(&id)
        .unwrap();
    assert!(secret.token.unwrap().contains("refresh_token"));
    assert!(
        !fixture.issuer.verifiers().is_empty(),
        "canonical PKCE exchange happened"
    );
    assert!(
        !std::fs::read_to_string(accounts::store_path_in(&prepared.config_root()))
            .unwrap()
            .contains(&code)
    );
    assert!(!accounts.projection().to_string().contains(&code));
}

#[test]
fn dropbox_pasted_code_back_cancels_and_invalid_exchange_never_prepares_account() {
    let mut fixture = dropbox_paste();
    let accounts = &mut fixture.accounts;
    for invalid_exchange in [false, true] {
        accounts
            .apply(AccountRequest::Provider("dropbox".into()))
            .unwrap();
        accounts.apply(AccountRequest::Confirm).unwrap();
        accounts
            .set_dav_field(
                "oauth-code",
                zeroize::Zeroizing::new("private-invalid-code".into()),
            )
            .unwrap();
        if invalid_exchange {
            accounts.apply(AccountRequest::SubmitCode).unwrap();
            settle(accounts);
            assert!(matches!(accounts.step, Step::Failed { .. }));
        } else {
            accounts.apply(AccountRequest::Back).unwrap();
            assert!(accounts.at_overview());
            assert!(!accounts.actions.busy());
            assert!(fixture.issuer.requests().is_empty());
        }
        assert!(accounts.dav_fields[2].get().is_empty());
        assert!(accounts.prepared.is_empty());
        assert!(accounts.prepared_handle().is_none());
        assert!(
            !accounts
                .projection()
                .to_string()
                .contains("private-invalid-code")
        );
    }
}

struct TiloSource {
    _temp: tempfile::TempDir,
    roots: SourceRoots,
    id: accounts::AccountId,
}
fn tilo_source(with_grant: bool) -> TiloSource {
    let temp = tempfile::tempdir().unwrap();
    let roots = SourceRoots {
        home: temp.path().join("source-home"),
        config: temp.path().join("source-home/.config"),
    };
    let source = roots.config.join("tilo");
    let id = accounts::AccountId::new("music.spotify");
    let mut account = accounts::Account::new(id.clone(), "spotify");
    account.display_name = "My Spotify library".into();
    account.config = serde_json::json!({"provider":"spotify"});
    let mut store = accounts::AccountStore::new();
    store.upsert(account);
    let mut foreign = accounts::Account::new(accounts::AccountId::new("someone-else"), "dropbox");
    foreign.owner = Some("another-person".into());
    store.upsert(foreign);
    store.save_in(&source).unwrap();
    if with_grant {
        account_custody::Custodian::for_root(Some(source))
            .seal_oauth(
                &id,
                &account_custody::TokenSet {
                    access_token: "private-source-spotify-access".into(),
                    refresh_token: "private-source-refresh".into(),
                    expires_in: 3600,
                    scope: Some("user-read-private user-library-read".into()),
                    ..Default::default()
                }
                .stamp_now(),
            )
            .unwrap();
    }
    TiloSource {
        _temp: temp,
        roots,
        id,
    }
}

#[test]
fn guided_selected_tilo_connection_imports_usable_grant_and_preserves_source_bytes() {
    let source = tilo_source(true);
    let root = source.roots.config.join("tilo");
    let inventory = super::super::super::account_transfer::discover(&source.roots);
    assert_eq!(inventory.tilo_accounts.len(), 1);
    assert!(inventory.unreadable_sources.is_empty());
    let selected = inventory.tilo_accounts[0].source_id.clone();
    let metadata = std::fs::read(accounts::store_path_in(&root)).unwrap();
    let vault = std::fs::read(accounts::vault_dir_in(&root).join("vault.enc")).unwrap();
    let projected = serde_json::to_string(&inventory).unwrap();
    assert!(!projected.contains("private-source-spotify-access"));
    assert!(!projected.contains("private-source-refresh"));
    let browser = Browser::new(false);
    let mut accounts = Accounts::default();
    accounts.opener = browser.clone();
    accounts
        .update_sources(source.roots.clone(), inventory)
        .unwrap();
    accounts
        .apply(AccountRequest::Source(selected.clone()))
        .unwrap();
    assert!(matches!(accounts.step, Step::Confirm(Selection::Tilo(_))));
    assert!(accounts.prepared_handle().is_none());
    accounts.apply(AccountRequest::Confirm).unwrap();
    settle(&mut accounts);
    assert!(matches!(accounts.step, Step::Done(_)));
    let prepared = accounts.prepared_handle().unwrap();
    let token = account_custody::Custodian::for_root(Some(prepared.config_root()))
        .oauth_access_token(
            &source.id,
            account_custody::OAuthUse::new(
                account_custody::OAuthIssuer::Spotify,
                account_custody::Capability::Music,
            ),
            false,
        )
        .unwrap();
    assert_eq!(token.as_str(), "private-source-spotify-access");
    assert_eq!(prepared.count(), 1);
    accounts.apply(AccountRequest::Overview).unwrap();
    assert_eq!(
        accounts.apply(AccountRequest::Source(selected)),
        Err("installer-accounts-already-included")
    );
    assert_eq!(accounts.prepared_handle().unwrap().count(), 1);
    assert!(browser.opened.lock().unwrap().is_empty());
    assert_eq!(
        std::fs::read(accounts::store_path_in(&root)).unwrap(),
        metadata
    );
    assert_eq!(
        std::fs::read(accounts::vault_dir_in(&root).join("vault.enc")).unwrap(),
        vault
    );
    assert!(!accounts.projection().to_string().contains(token.as_str()));
}

#[test]
fn selected_tilo_metadata_change_and_unavailable_custody_never_claim_connection() {
    for change_metadata in [false, true] {
        let source = tilo_source(change_metadata);
        let root = source.roots.config.join("tilo");
        let inventory = super::super::super::account_transfer::discover(&source.roots);
        assert_eq!(
            inventory.tilo_accounts.len(),
            1,
            "discovery never needs vault access"
        );
        let selected = inventory.tilo_accounts[0].source_id.clone();
        let mut accounts = Accounts::default();
        let browser = Browser::new(false);
        accounts.opener = browser.clone();
        accounts
            .update_sources(source.roots.clone(), inventory)
            .unwrap();
        accounts.apply(AccountRequest::Source(selected)).unwrap();
        if change_metadata {
            let mut store = accounts::AccountStore::load_in(&root);
            let mut changed = store.get(&source.id).unwrap().clone();
            changed.type_id = "dropbox".into();
            changed.config = serde_json::json!({"provider":"dropbox"});
            store.upsert(changed);
            store.save_in(&root).unwrap();
        }
        let original = std::fs::read(accounts::store_path_in(&root)).unwrap();
        accounts.apply(AccountRequest::Confirm).unwrap();
        settle(&mut accounts);
        assert!(matches!(accounts.step, Step::Failed { .. }));
        assert_eq!(
            accounts.subheading(),
            t(if change_metadata {
                "installer-accounts-tilo-changed"
            } else {
                "installer-accounts-tilo-reconnect"
            })
        );
        assert!(accounts.prepared_handle().is_none());
        assert!(accounts.prepared.is_empty());
        let staged = accounts.stage.as_ref().unwrap().path().join(".config/tilo");
        assert!(accounts::AccountStore::load_in(&staged).all().is_empty());
        assert!(
            accounts::SecretVault::open_in(&staged)
                .get(&source.id)
                .is_none()
        );
        assert_eq!(
            std::fs::read(accounts::store_path_in(&root)).unwrap(),
            original
        );
        assert!(browser.opened.lock().unwrap().is_empty());
        if !change_metadata {
            let failed = accounts.view(Rc::new(|_| {}), Rc::new(|_, _| {}));
            assert!(account_widget(&failed, "installer.account.reconnect").is_some());
            assert!(account_widget(&failed, "installer.account.retry").is_none());
            assert_eq!(
                accounts.apply(AccountRequest::Retry),
                Err("installer-accounts-invalid-action")
            );
            accounts.apply(AccountRequest::Reconnect).unwrap();
            assert!(matches!(accounts.step, Step::Providers));
            let chooser = accounts.view(Rc::new(|_| {}), Rc::new(|_, _| {}));
            assert!(account_widget(&chooser, "installer.account.provider.google-drive").is_some());
            assert!(accounts.prepared.is_empty());
            assert!(browser.opened.lock().unwrap().is_empty());
            assert_eq!(
                std::fs::read(accounts::store_path_in(&root)).unwrap(),
                original
            );
        }
    }
}

#[test]
#[cfg(unix)]
fn saved_tilo_source_redirects_cannot_escape_the_consented_config_root() {
    use std::os::unix::fs::symlink;
    let source = tilo_source(true);
    let root = source.roots.config.join("tilo");
    let inventory = super::super::super::account_transfer::discover(&source.roots);
    let selected = inventory.tilo_accounts[0].source_id.clone();
    let saved = source._temp.path().join("outside-consented-config");
    std::fs::rename(&root, &saved).unwrap();
    let original = std::fs::read(accounts::store_path_in(&saved)).unwrap();
    let ciphertext = std::fs::read(accounts::vault_dir_in(&saved).join("vault.enc")).unwrap();
    symlink(&saved, &root).unwrap();
    let refused = super::super::super::account_transfer::discover(&source.roots);
    assert!(refused.tilo_accounts.is_empty());
    assert!(refused.unreadable_sources.contains(&"Tilo accounts"));
    assert!(
        !serde_json::to_string(&refused)
            .unwrap()
            .contains("My Spotify library")
    );
    let mut accounts = Accounts::default();
    accounts.update_sources(source.roots, inventory).unwrap();
    accounts.apply(AccountRequest::Source(selected)).unwrap();
    accounts.apply(AccountRequest::Confirm).unwrap();
    settle(&mut accounts);
    assert!(matches!(accounts.step, Step::Failed { .. }));
    assert!(accounts.prepared_handle().is_none());
    assert_eq!(
        std::fs::read(accounts::store_path_in(&saved)).unwrap(),
        original
    );
    assert_eq!(
        std::fs::read(accounts::vault_dir_in(&saved).join("vault.enc")).unwrap(),
        ciphertext
    );
}

#[test]
fn detected_oauth_profile_obeys_configuration_in_rows_and_semantic_actions() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let config = home.join(".config");
    let profile = config.join("spotify");
    std::fs::create_dir_all(&profile).unwrap();
    std::fs::write(profile.join("session"), b"preserved profile").unwrap();
    let roots = SourceRoots { home, config };
    let inventory = super::super::super::account_transfer::discover(&roots);
    let candidate = inventory.candidates[0].clone();
    assert!(can_connect_candidate(&candidate));
    assert!(!candidate.can_import_credentials);
    let browser = Browser::new(false);
    let mut accounts = Accounts::default();
    accounts.opener = browser.clone();
    accounts.actions = AccountActions::default().with_custodian(
        account_custody::Custodian::for_root(None)
            .with_client_table(account_custody::ClientTable::parse("").unwrap()),
    );
    accounts.update_sources(roots, inventory).unwrap();
    fn find<'a>(root: &'a dyn ui::widget::Widget, id: &str) -> Option<&'a dyn ui::widget::Widget> {
        if root.test_id() == Some(id) {
            return Some(root);
        }
        root.children()
            .into_iter()
            .find_map(|child| find(child, id))
    }
    let view = accounts.view(Rc::new(|_| {}), Rc::new(|_, _| {}));
    let row = find(&view, &format!("installer.account.source.{}", candidate.id)).unwrap();
    let accessible = row.accessibility().unwrap();
    assert!(accessible.state.disabled);
    assert!(
        accessible
            .value
            .unwrap()
            .contains(&t("installer-accounts-unavailable"))
    );
    assert_eq!(
        accounts.apply(AccountRequest::Source(candidate.id.clone())),
        Err("installer-accounts-unavailable")
    );
    assert_eq!(
        accounts.apply(AccountRequest::Provider("spotify".into())),
        Err("installer-accounts-unavailable")
    );
    accounts.apply(AccountRequest::AddAccount).unwrap();
    let chooser = accounts.view(Rc::new(|_| {}), Rc::new(|_, _| {}));
    for provider in ["google-drive", "onedrive", "dropbox", "spotify"] {
        assert!(
            find(&chooser, &format!("installer.account.provider.{provider}")).is_none(),
            "unconfigured {provider} must not be offered as a new connection"
        );
    }
    assert!(find(&chooser, "installer.account.provider.webdav").is_some());
    assert_eq!(
        accounts.apply(AccountRequest::Provider("google-drive".into())),
        Err("installer-accounts-unavailable")
    );
    // Configuration can be withdrawn after a prior confirmation screen was built.
    accounts.step = Step::Confirm(Selection::OAuth {
        provider: ConnectionProvider::Spotify,
        candidate: Some(candidate.clone()),
    });
    assert_eq!(
        accounts.apply(AccountRequest::Confirm),
        Err("installer-accounts-unavailable")
    );
    assert!(accounts.stage.is_none());
    assert!(accounts.prepared.is_empty());
    assert!(browser.opened.lock().unwrap().is_empty());
    assert_eq!(
        std::fs::read(profile.join("session")).unwrap(),
        b"preserved profile"
    );
    assert_eq!(accounts.inventory.candidates[0].id, candidate.id);
}

#[test]
fn configured_detected_oauth_profile_offers_the_real_confirmation_path() {
    let (_temp, _server, mut accounts, browser) = oauth_accounts(false);
    let candidate = Candidate {
        id: "configured-source".into(),
        provider: Provider::Google,
        origin: super::super::super::account_transfer::Origin::Application,
        label: "Google Drive".into(),
        identity: None,
        server: None,
        profile: std::path::PathBuf::from("unused-metadata-profile"),
        needs_sign_in: true,
        can_import_credentials: false,
        services: Vec::new(),
    };
    accounts.inventory.candidates.push(candidate.clone());
    accounts
        .apply(AccountRequest::Source(candidate.id))
        .unwrap();
    assert!(matches!(
        accounts.step,
        Step::Confirm(Selection::OAuth {
            provider: ConnectionProvider::GoogleDrive,
            ..
        })
    ));
    assert!(accounts.stage.is_none());
    assert!(browser.opened.lock().unwrap().is_empty());
}

#[cfg(target_os = "linux")]
#[test]
fn encrypted_account_checkpoint_survives_drop_and_restores_exact_grants_and_removal_provenance() {
    use account_custody::{Capability, Custodian, OAuthIssuer, OAuthUse};
    let temp = tempfile::tempdir().unwrap();
    let checkpoint_root = temp.path().join("accounts");
    super::super::personal::ensure_private_tree(&checkpoint_root).unwrap();
    let CheckpointFixture {
        before,
        source,
        copied_id,
        oauth_id,
        original,
        staged,
    } = checkpoint_grants(temp.path());
    let password = b"my previous chosen password";
    let receipt = checkpoint::save(
        &before.prepared_handle().unwrap(),
        &checkpoint_root,
        "operation-1",
        [7; 32],
        password,
    )
    .unwrap();
    assert_eq!(receipt.accounts(), 2);
    drop(before);
    assert!(!staged.exists());
    let bytes = std::fs::read(checkpoint_root.join(receipt.filename())).unwrap();
    assert!(
        !bytes
            .windows(b"private-checkpoint".len())
            .any(|part| part == b"private-checkpoint")
    );
    assert!(checkpoint::restore(&checkpoint_root, &receipt, b"wrong previous password").is_err());
    let mut foreign_fields = serde_json::to_value(&receipt).unwrap();
    foreign_fields["group"] = serde_json::json!(vec![8_u8; 32]);
    let mut foreign = serde_json::from_value(foreign_fields).unwrap();
    assert!(checkpoint::restore(&checkpoint_root, &foreign, password).is_err());
    let mut foreign_fields = serde_json::to_value(&receipt).unwrap();
    foreign_fields["binding"] = serde_json::json!("another-operation");
    foreign = serde_json::from_value(foreign_fields).unwrap();
    assert!(checkpoint::restore(&checkpoint_root, &foreign, password).is_err());
    let browser = Browser::new(false);
    let mut after = Accounts::default();
    after.opener = browser.clone();
    after
        .checkpoint_restore(
            checkpoint_root.clone(),
            receipt.clone(),
            Zeroizing::new(password.to_vec()),
        )
        .unwrap();
    assert!(!after.can_continue());
    settle_restored_checkpoint(&mut after);
    assert!(after.source_roots.is_none());
    assert!(after.inventory.candidates.is_empty());
    assert_eq!(
        after.apply(AccountRequest::from_parts("import-saved", None).unwrap()),
        Err("installer-accounts-invalid-action")
    );
    after
        .apply(AccountRequest::from_parts("review-saved", None).unwrap())
        .unwrap();
    assert_eq!(after.navigation_step(), "review-saved");
    after.apply(AccountRequest::Back).unwrap();
    assert!(!after.prepared[0].revoke_on_remove);
    assert!(after.prepared[1].revoke_on_remove);
    let restored = after.prepared_handle().unwrap();
    assert_eq!(
        account_custody::password_credential(
            &SecretVault::open_in(&restored.config_root())
                .get(&copied_id)
                .unwrap()
        )
        .to_string(),
        "original-private-app-password"
    );
    let access = Custodian::for_root(Some(restored.config_root()))
        .oauth_access_token(
            &oauth_id,
            OAuthUse::new(OAuthIssuer::Spotify, Capability::Music),
            false,
        )
        .unwrap();
    assert_eq!(access.as_str(), "private-checkpoint-access");
    assert!(browser.opened.lock().unwrap().is_empty());
    assert_eq!(
        std::fs::read(accounts::vault_dir_in(&source).join("vault.enc")).unwrap(),
        original
    );
    after.prepared.clear();
    after.reviewed_empty = true;
    let empty = checkpoint::save(
        &after.prepared_handle().unwrap(),
        &checkpoint_root,
        "operation-1",
        [7; 32],
        password,
    )
    .unwrap();
    assert_eq!(
        checkpoint::restore(&checkpoint_root, &empty, password)
            .unwrap()
            .count(),
        0
    );
    assert_ne!(empty.filename(), receipt.filename());
    let mut corrupt = bytes;
    corrupt[0] ^= 1;
    std::fs::write(checkpoint_root.join(receipt.filename()), corrupt).unwrap();
    assert!(checkpoint::restore(&checkpoint_root, &receipt, password).is_err());
}

#[cfg(target_os = "linux")]
struct CheckpointFixture {
    before: Accounts,
    source: PathBuf,
    copied_id: accounts::AccountId,
    oauth_id: accounts::AccountId,
    original: Vec<u8>,
    staged: PathBuf,
}
#[cfg(target_os = "linux")]
fn checkpoint_grants(root: &Path) -> CheckpointFixture {
    let dav_server = crate::setup::account_transfer::dav_readiness::test_server();
    use account_custody::{Custodian, TokenSet};
    let source = root.join("source");
    let copied = Connector::new(source.clone())
        .connect_webdav(
            "Copied files",
            &format!("{}/dav", dav_server.url()),
            "ada",
            Zeroizing::new("original-private-app-password".into()),
        )
        .unwrap();
    let original = std::fs::read(accounts::vault_dir_in(&source).join("vault.enc")).unwrap();
    let mut before = Accounts::default();
    let staged = before.stage_home().unwrap();
    let config = staged.join(".config/tilo");
    let copied_id = Connector::new(config.clone())
        .import_tilo(&source, &copied)
        .unwrap();
    let oauth_id = accounts::AccountId::new("music.spotify");
    let mut store = accounts::AccountStore::load_in(&config);
    let mut account = accounts::Account::new(oauth_id.clone(), "spotify");
    account.display_name = "Spotify".into();
    account.config = serde_json::json!({"provider":"spotify"});
    store.upsert(account);
    store.save_in(&config).unwrap();
    Custodian::for_root(Some(config))
        .seal_oauth(
            &oauth_id,
            &TokenSet {
                access_token: "private-checkpoint-access".into(),
                refresh_token: "private-checkpoint-refresh".into(),
                expires_in: 3600,
                scope: Some("user-read-private user-library-read".into()),
                ..Default::default()
            }
            .stamp_now(),
        )
        .unwrap();
    before.prepared = vec![
        PreparedEntry {
            id: copied_id.as_str().into(),
            source_id: Some("source-dav".into()),
            label: "Copied files".into(),
            revoke_on_remove: false,
        },
        PreparedEntry {
            id: oauth_id.as_str().into(),
            source_id: None,
            label: "Spotify".into(),
            revoke_on_remove: true,
        },
    ];
    before.step = Step::Overview;
    CheckpointFixture {
        before,
        source,
        copied_id,
        oauth_id,
        original,
        staged,
    }
}

#[cfg(target_os = "linux")]
fn settle_restored_checkpoint(accounts: &mut Accounts) {
    let until = std::time::Instant::now() + tilo_test_env::hang_bound();
    loop {
        if let Some(outcome) = accounts.checkpoint_poll() {
            assert!(matches!(outcome, Ok(None)));
            break;
        }
        assert!(std::time::Instant::now() < until);
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

#[cfg(target_os = "linux")]
pub(super) fn prepared_checkpoint_accounts(root: &Path) -> Accounts {
    checkpoint_grants(root).before
}

#[cfg(target_os = "linux")]
#[test]
fn account_overview_discloses_provider_choices_only_after_explicit_add() {
    let temp = tempfile::tempdir().unwrap();
    let mut accounts = checkpoint_fixture_for_journey(temp.path());
    let original_ids = accounts
        .prepared
        .iter()
        .map(|entry| entry.id.clone())
        .collect::<Vec<_>>();
    let browser = Browser::new(false);
    accounts.opener = browser.clone();
    let overview = accounts.view(Rc::new(|_| {}), Rc::new(|_, _| {}));
    assert!(account_widget(&overview, "installer.account.add").is_some());
    assert!(account_widget(&overview, "installer.account.provider.google-drive").is_none());
    assert!(
        account_widget(
            &overview,
            &format!("installer.account.ready.{}", original_ids[0])
        )
        .is_some()
    );
    let request = AccountRequest::from_parts("add-account", None).unwrap();
    assert_eq!(request.parts(), ("add-account", None));
    accounts.apply(request).unwrap();
    assert_eq!(accounts.projection()["state"], "providers");
    assert!(!accounts.can_continue());
    let chooser = accounts.view(Rc::new(|_| {}), Rc::new(|_, _| {}));
    assert!(account_widget(&chooser, "installer.account.provider.google-drive").is_some());
    assert!(account_widget(&chooser, "installer.account.add").is_none());
    accounts
        .apply(AccountRequest::from_parts("back", None).unwrap())
        .unwrap();
    assert!(accounts.at_overview());
    assert_eq!(
        accounts
            .prepared
            .iter()
            .map(|entry| entry.id.clone())
            .collect::<Vec<_>>(),
        original_ids
    );
    assert!(browser.opened.lock().unwrap().is_empty());
}

fn account_widget<'a>(
    root: &'a dyn ui::widget::Widget,
    id: &str,
) -> Option<&'a dyn ui::widget::Widget> {
    if root.test_id() == Some(id) {
        return Some(root);
    }
    root.children()
        .into_iter()
        .find_map(|child| account_widget(child, id))
}

#[test]
fn account_provider_confirmation_back_returns_to_the_service_choice() {
    let (_temp, _server, mut accounts, browser) = oauth_accounts(false);
    accounts.apply(AccountRequest::AddAccount).unwrap();
    accounts
        .apply(AccountRequest::from_parts("choose-provider", Some("google-drive".into())).unwrap())
        .unwrap();
    assert!(matches!(accounts.step, Step::Confirm(_)));
    accounts.apply(AccountRequest::Back).unwrap();
    assert!(matches!(accounts.step, Step::Providers));
    accounts.apply(AccountRequest::Back).unwrap();
    assert!(accounts.at_overview());
    assert!(accounts.stage.is_none());
    assert!(browser.opened.lock().unwrap().is_empty());
}

#[test]
fn spotify_connection_success_describes_the_actual_library_grant() {
    let temp = tempfile::tempdir().unwrap();
    let issuer = tilo_oauth::fake::FakeAuthorizationServer::start();
    let (endpoint, api) = crate::setup::account_transfer::spotify_identity::profile_server(vec![
        serde_json::json!({"id":"ada-spotify", "account_id":"stable-ada"}),
    ]);
    let browser = Browser::new(true);
    let mut accounts = Accounts::default();
    accounts.opener = browser.clone();
    let table = account_custody::ClientTable::parse(
        r#"
[spotify]
client_id = "tilo-client"
[spotify.scopes]
music = ["user-read-private", "user-library-read"]
"#,
    )
    .unwrap();
    accounts.actions = AccountActions::default()
        .with_custodian(
            account_custody::Custodian::for_root(Some(temp.path().join("fixture-root")))
                .with_client_table(table)
                .with_issuer_endpoints(account_custody::OAuthIssuer::Spotify, issuer.provider()),
        )
        .with_spotify_api(endpoint);
    accounts.apply(AccountRequest::AddAccount).unwrap();
    accounts
        .apply(AccountRequest::Provider("spotify".into()))
        .unwrap();
    accounts.apply(AccountRequest::Confirm).unwrap();
    settle(&mut accounts);
    api.join().unwrap();
    assert!(matches!(
        accounts.step,
        Step::Done(Selection::OAuth {
            provider: ConnectionProvider::Spotify,
            ..
        })
    ));
    let prepared = accounts.prepared_handle().unwrap();
    let store = accounts::AccountStore::load_in(&prepared.config_root());
    assert_eq!(
        store
            .get(&accounts::AccountId::new("music.spotify"))
            .unwrap()
            .type_id
            .as_str(),
        "spotify"
    );
    assert_eq!(
        store
            .get(&accounts::AccountId::new("music.spotify"))
            .unwrap()
            .config["spotify_user_id"],
        "ada-spotify"
    );
    assert_eq!(
        store
            .get(&accounts::AccountId::new("music.spotify"))
            .unwrap()
            .config["spotify_account_id"],
        "stable-ada"
    );
    assert!(
        store
            .get(&accounts::AccountId::new("music.spotify.streaming"))
            .is_none()
    );
    assert_eq!(
        accounts.heading(),
        t("installer-accounts-spotify-done-heading")
    );
    assert_eq!(
        accounts.subheading(),
        t("installer-accounts-spotify-done-intro")
    );
    accounts.apply(AccountRequest::AddAccount).unwrap();
    assert!(matches!(accounts.step, Step::Providers));
    assert_eq!(accounts.prepared.len(), 1);
    assert_eq!(browser.opened.lock().unwrap().len(), 1);
}

#[test]
fn spotify_playback_has_its_own_explicit_consent_page_and_is_hidden_when_unconfigured() {
    let browser = Browser::new(false);
    let mut accounts = Accounts::default();
    accounts.opener = browser.clone();
    let mut table = account_custody::ClientTable::parse(
        r#"
[spotify]
client_id = "tilo-test-registered-client"
[spotify.scopes]
music = ["user-read-private", "user-library-read"]
music_streaming = ["streaming", "user-read-private"]
"#,
    )
    .unwrap();
    let complete_table = table.clone();
    table.spotify.scopes.music_streaming.clear();
    accounts.actions = AccountActions::default()
        .with_custodian(account_custody::Custodian::for_root(None).with_client_table(table));
    accounts.apply(AccountRequest::AddAccount).unwrap();
    let view = accounts.view(Rc::new(|_| {}), Rc::new(|_, _| {}));
    assert!(account_widget(&view, "installer.account.provider.spotify").is_some());
    assert!(account_widget(&view, "installer.account.provider.spotify-streaming").is_none());
    accounts
        .apply(AccountRequest::Provider("spotify-options".into()))
        .unwrap();
    assert_eq!(accounts.projection()["state"], "spotify-purpose");
    let purpose = accounts.view(Rc::new(|_| {}), Rc::new(|_, _| {}));
    assert!(account_widget(&purpose, "installer.account.spotify.spotify").is_some());
    assert!(account_widget(&purpose, "installer.account.spotify.spotify-streaming").is_none());
    assert_eq!(
        accounts.apply(AccountRequest::Provider("spotify-streaming".into())),
        Err("installer-accounts-unavailable")
    );
    accounts.actions = AccountActions::default().with_custodian(
        account_custody::Custodian::for_root(None).with_client_table(complete_table),
    );
    accounts
        .apply(AccountRequest::Provider("spotify-streaming".into()))
        .unwrap();
    assert!(matches!(
        accounts.step,
        Step::Confirm(Selection::OAuth {
            provider: ConnectionProvider::SpotifyStreaming,
            ..
        })
    ));
    assert_eq!(
        accounts.subheading(),
        t("installer-accounts-spotify-playback-intro")
    );
    assert!(accounts.stage.is_none());
    assert!(accounts.prepared.is_empty());
    assert!(browser.opened.lock().unwrap().is_empty());
    accounts.apply(AccountRequest::Back).unwrap();
    accounts
        .apply(AccountRequest::Provider("spotify".into()))
        .unwrap();
    assert_eq!(accounts.subheading(), t("installer-accounts-spotify-intro"));
    assert!(accounts.stage.is_none());
    assert!(browser.opened.lock().unwrap().is_empty());
}

#[cfg(target_os = "linux")]
#[test]
fn source_home_checkpoint_requires_current_acknowledged_operation_and_keeps_generic_export_excluded()
 {
    use super::super::draft::{DraftStatus, DraftStore, IdentityKind, PublicChoices};
    let mut env = tilo_test_env::ScopedEnv::new();
    env.set(runtime_identity::FILE_SECRETS_ENV, "1");
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().canonicalize().unwrap();
    env.set("HOME", &home);
    env.set("XDG_STATE_HOME", home.join(".local/state"));
    super::super::personal::ensure_private_tree(&home).unwrap();
    let CheckpointFixture {
        mut before,
        source,
        original,
        ..
    } = checkpoint_grants(&home);
    before
        .actions
        .update_sources(
            SourceRoots {
                home: home.clone(),
                config: source.clone(),
            },
            Inventory::default(),
        )
        .unwrap();
    let handle = before.prepared_handle().unwrap();
    let store = DraftStore::open(&home.join(".local/state/tilo/installer/preparations")).unwrap();
    let pending = store
        .reserve(
            &PublicChoices {
                destination: None,
                migrate: Some(true),
                categories: [true, true, true],
                browser_domains: [true, true],
                excluded_flatpak_apps: std::collections::BTreeSet::default(),
                device_label: "My computer".into(),
                purpose: install_profile_model::default_purpose(),
                deferred: false,
                target: None,
            },
            IdentityKind::New,
        )
        .unwrap();
    let operation = pending.operation_directory();
    let binding = operation.file_name().unwrap().to_str().unwrap();
    let checkpoint_root = operation.join("accounts");
    assert!(
        checkpoint::save(
            &handle,
            &checkpoint_root,
            binding,
            [7; 32],
            b"chosen password"
        )
        .is_err()
    );
    assert!(!checkpoint_root.exists());
    let identity = std::sync::Arc::new(
        crate::setup::prepared_identity::PreparedIdentity::create(
            &pending.identity_directory(),
            "Chosen person",
            [20, 80, 140],
            tilo_identity_host::provisioning::IdentityStorage::for_session(),
        )
        .unwrap(),
    );
    store.acknowledge(&pending, &identity, None).unwrap();
    assert!(
        checkpoint::save(
            &handle,
            &checkpoint_root,
            binding,
            [7; 32],
            b"chosen password"
        )
        .is_err()
    );
    assert!(!checkpoint_root.exists());
    assert!(
        checkpoint::save(
            &handle,
            &checkpoint_root,
            "different-operation",
            identity.public_key(),
            b"chosen password"
        )
        .is_err()
    );
    assert!(!checkpoint_root.exists());
    let receipt = checkpoint::save(
        &handle,
        &checkpoint_root,
        binding,
        identity.public_key(),
        b"chosen password",
    )
    .unwrap();
    assert_eq!(receipt.accounts(), 2);
    assert!(
        handle
            .validate_artifact_destination(&checkpoint_root.join("ordinary-export.sealed"))
            .is_err()
    );
    store.record_account_checkpoint(&receipt, None).unwrap();
    let DraftStatus::Ready(ready) = store.review().unwrap() else {
        panic!("acknowledged draft missing");
    };
    assert_eq!(ready.account_checkpoint(), Some(&receipt));
    assert_eq!(
        std::fs::read(accounts::vault_dir_in(&source).join("vault.enc")).unwrap(),
        original
    );
}

#[cfg(target_os = "linux")]
#[test]
fn unavailable_native_nextcloud_sign_in_offers_explicit_reconnect_with_bound_server_and_user() {
    use std::os::unix::fs::PermissionsExt;
    let temporary = tempfile::tempdir().unwrap();
    let mut environment = tilo_test_env::ScopedEnv::new();
    environment.set(
        "DBUS_SESSION_BUS_ADDRESS",
        format!(
            "unix:path={}",
            temporary.path().join("absent-owner-bus").display()
        ),
    );
    let home = temporary.path().join("original-home");
    let config = home.join(".config");
    std::fs::create_dir_all(config.join("Nextcloud")).unwrap();
    std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700)).unwrap();
    std::fs::set_permissions(&config, std::fs::Permissions::from_mode(0o700)).unwrap();
    let source = config.join("Nextcloud/nextcloud.cfg");
    let original = "[Accounts]\n0\\url=https://cloud.example.test/base\n0\\http_user=ada\n0\\authType=http\n0\\displayName=My files\n";
    std::fs::write(&source, original).unwrap();
    let roots = SourceRoots { home, config };
    let inventory = super::super::super::account_transfer::discover(&roots);
    let candidate = inventory
        .candidates
        .iter()
        .find(|candidate| candidate.can_import_credentials)
        .unwrap()
        .clone();
    let browser = Browser::new(false);
    let mut accounts = Accounts::default();
    accounts.opener = browser.clone();
    accounts.update_sources(roots, inventory).unwrap();
    accounts
        .apply(AccountRequest::from_parts("choose-source", Some(candidate.id)).unwrap())
        .unwrap();
    accounts
        .apply(AccountRequest::from_parts("confirm", None).unwrap())
        .unwrap();
    settle(&mut accounts);
    assert!(
        matches!(&accounts.step, Step::Failed { message, .. } if *message == "installer-accounts-nextcloud-source-reconnect")
    );
    assert!(accounts.prepared.is_empty());
    assert!(!accounts.can_continue());
    assert_eq!(
        accounts.apply(AccountRequest::from_parts("retry", None).unwrap()),
        Err("installer-accounts-invalid-action")
    );
    let reconnect = AccountRequest::from_parts("reconnect", None).unwrap();
    assert_eq!(reconnect.parts(), ("reconnect", None));
    accounts.apply(reconnect).unwrap();
    assert!(matches!(accounts.step, Step::Dav { field: 0, .. }));
    assert_eq!(
        accounts.dav_fields[0].get(),
        "https://cloud.example.test/base"
    );
    assert_eq!(accounts.dav_fields[1].get(), "ada");
    accounts
        .apply(AccountRequest::from_parts("confirm", None).unwrap())
        .unwrap();
    assert!(matches!(accounts.step, Step::NextcloudMethod(_)));
    assert!(browser.opened.lock().unwrap().is_empty());
    assert!(!accounts.actions.busy());
    accounts
        .apply(AccountRequest::from_parts("nextcloud-app-password", None).unwrap())
        .unwrap();
    assert!(matches!(accounts.step, Step::Dav { field: 1, .. }));
    assert_eq!(accounts.dav_fields[1].get(), "ada");
    assert!(browser.opened.lock().unwrap().is_empty());
    assert_eq!(std::fs::read_to_string(&source).unwrap(), original);
    assert_eq!(
        std::fs::read_dir(source.parent().unwrap()).unwrap().count(),
        1
    );
}

#[cfg(target_os = "linux")]
struct SavedBatchFixture {
    _temporary: tempfile::TempDir,
    roots: SourceRoots,
    accounts: Accounts,
    browser: Arc<Browser>,
    canonical_id: String,
    rclone_id: String,
    nextcloud_id: String,
    source_files: Vec<(std::path::PathBuf, Vec<u8>)>,
}
#[cfg(target_os = "linux")]
impl SavedBatchFixture {
    fn new() -> Self {
        let dav_server = crate::setup::account_transfer::dav_readiness::test_server();
        use std::os::unix::fs::PermissionsExt;
        let temporary = tempfile::tempdir().unwrap();
        let home = temporary.path().join("original-home");
        let config = home.join(".config");
        std::fs::create_dir_all(config.join("rclone")).unwrap();
        std::fs::create_dir_all(config.join("Nextcloud")).unwrap();
        std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::set_permissions(&config, std::fs::Permissions::from_mode(0o700)).unwrap();
        let rclone = config.join("rclone/rclone.conf");
        std::fs::write(&rclone, "[Team files]\ntype=webdav\nurl=https://team.example.test/dav\nuser=ada\npass=AAECAwQFBgcICQoLDA0OD2f2H1FyoCWnrFF4SHSBvFFHjlw9\n").unwrap();
        let nextcloud = config.join("Nextcloud/nextcloud.cfg");
        std::fs::write(&nextcloud, "[Accounts]\n0\\url=https://cloud.example.test/base\n0\\http_user=ada\n0\\authType=http\n").unwrap();
        let tilo = config.join("tilo");
        let canonical_id = Connector::new(tilo.clone())
            .connect_webdav(
                "Canonical files",
                &format!("{}/dav", dav_server.url()),
                "ada",
                zeroize::Zeroizing::new("private-canonical-password".into()),
            )
            .unwrap()
            .as_str()
            .to_owned();
        let roots = SourceRoots { home, config };
        let inventory = super::super::super::account_transfer::discover(&roots);
        let rclone_id = inventory
            .candidates
            .iter()
            .find(|c| c.origin == super::super::super::account_transfer::Origin::Rclone)
            .unwrap()
            .id
            .clone();
        let nextcloud_id = inventory
            .candidates
            .iter()
            .find(|c| c.origin == super::super::super::account_transfer::Origin::Nextcloud)
            .unwrap()
            .id
            .clone();
        assert_eq!(inventory.tilo_accounts.len(), 1);
        let source_files = [
            rclone,
            nextcloud,
            accounts::store_path_in(&tilo),
            accounts::vault_dir_in(&tilo).join("vault.enc"),
        ]
        .into_iter()
        .map(|path| {
            let bytes = std::fs::read(&path).unwrap();
            (path, bytes)
        })
        .collect();
        let browser = Browser::new(false);
        let mut accounts = Accounts::default();
        accounts.opener = browser.clone();
        accounts.update_sources(roots.clone(), inventory).unwrap();
        Self {
            _temporary: temporary,
            roots,
            accounts,
            browser,
            canonical_id,
            rclone_id,
            nextcloud_id,
            source_files,
        }
    }
    fn assert_sources_unchanged(&self) {
        for (path, original) in &self.source_files {
            assert_eq!(&std::fs::read(path).unwrap(), original);
        }
        assert!(self.browser.opened.lock().unwrap().is_empty());
    }
}

#[cfg(target_os = "linux")]
#[test]
fn saved_connections_bulk_consent_imports_all_available_and_retains_missing_keychain_attention() {
    let mut environment = tilo_test_env::ScopedEnv::new();
    environment.set(runtime_identity::FILE_SECRETS_ENV, "1");
    let mut fixture = SavedBatchFixture::new();
    environment.set(
        "DBUS_SESSION_BUS_ADDRESS",
        format!(
            "unix:path={}",
            fixture.roots.home.join("absent-bus").display()
        ),
    );
    assert!(!fixture.accounts.can_continue());
    assert!(fixture.accounts.stage.is_none());
    assert_eq!(
        fixture.accounts.navigation_action().unwrap().2,
        AccountRequest::ImportSaved
    );
    fixture
        .accounts
        .apply(AccountRequest::from_parts("import-saved", None).unwrap())
        .unwrap();
    assert!(!fixture.accounts.can_continue());
    settle(&mut fixture.accounts);
    assert!(matches!(fixture.accounts.step, Step::Overview));
    assert!(fixture.accounts.can_continue());
    assert_eq!(fixture.accounts.prepared.len(), 2);
    assert!(
        fixture
            .accounts
            .prepared
            .iter()
            .all(|entry| !entry.revoke_on_remove)
    );
    let staged = fixture.accounts.prepared_handle().unwrap();
    let store = accounts::AccountStore::load_in(&staged.config_root());
    assert_eq!(store.len(), 2);
    assert!(
        store
            .get(&accounts::AccountId::new(&fixture.canonical_id))
            .is_some()
    );
    assert!(
        store
            .all()
            .iter()
            .any(|account| account.config["server_url"] == "https://team.example.test/dav")
    );
    assert_eq!(
        account_custody::password_credential(
            &SecretVault::open_in(&staged.config_root())
                .get(&accounts::AccountId::new(&fixture.canonical_id))
                .unwrap()
        )
        .to_string(),
        "private-canonical-password"
    );
    assert!(
        fixture
            .accounts
            .saved
            .attention
            .contains_key(&fixture.nextcloud_id)
    );
    let overview = fixture.accounts.view(Rc::new(|_| {}), Rc::new(|_, _| {}));
    assert!(
        account_widget(
            &overview,
            &format!("installer.account.attention.{}", fixture.nextcloud_id)
        )
        .is_some()
    );
    fixture
        .accounts
        .apply(AccountRequest::from_parts("attention", Some(fixture.nextcloud_id.clone())).unwrap())
        .unwrap();
    fixture
        .accounts
        .apply(AccountRequest::from_parts("reconnect", None).unwrap())
        .unwrap();
    assert!(matches!(fixture.accounts.step, Step::Dav { field: 0, .. }));
    fixture.accounts.apply(AccountRequest::Overview).unwrap();
    fixture.accounts.apply(AccountRequest::ReviewSaved).unwrap();
    fixture.accounts.apply(AccountRequest::ImportSaved).unwrap();
    settle(&mut fixture.accounts);
    assert_eq!(
        accounts::AccountStore::load_in(&staged.config_root()).len(),
        2
    );
    fixture.assert_sources_unchanged();
}

#[cfg(target_os = "linux")]
#[test]
fn saved_connections_review_exclusions_and_explicit_skip_never_query_or_import_omitted_accounts() {
    let mut environment = tilo_test_env::ScopedEnv::new();
    environment.set(runtime_identity::FILE_SECRETS_ENV, "1");
    let mut fixture = SavedBatchFixture::new();
    fixture
        .accounts
        .apply(AccountRequest::from_parts("review-saved", None).unwrap())
        .unwrap();
    assert_eq!(fixture.accounts.navigation_step(), "review-saved");
    let review = fixture.accounts.view(Rc::new(|_| {}), Rc::new(|_, _| {}));
    assert!(
        account_widget(
            &review,
            &format!("installer.account.include.{}", fixture.rclone_id)
        )
        .is_some()
    );
    for id in [&fixture.rclone_id, &fixture.nextcloud_id] {
        fixture
            .accounts
            .apply(AccountRequest::from_parts("exclude-saved", Some(id.clone())).unwrap())
            .unwrap();
    }
    assert!(fixture.accounts.stage.is_none());
    fixture
        .accounts
        .apply(AccountRequest::from_parts("import-saved", None).unwrap())
        .unwrap();
    settle(&mut fixture.accounts);
    assert_eq!(fixture.accounts.prepared.len(), 1);
    assert_eq!(fixture.accounts.prepared[0].id, fixture.canonical_id);
    assert!(fixture.accounts.saved.attention.is_empty());
    assert!(fixture.accounts.can_continue());
    fixture.assert_sources_unchanged();
    let mut omitted = SavedBatchFixture::new();
    omitted
        .accounts
        .apply(AccountRequest::from_parts("skip-saved", None).unwrap())
        .unwrap();
    assert!(omitted.accounts.can_continue());
    assert_eq!(
        omitted.accounts.continue_label(),
        "installer-accounts-continue-without"
    );
    assert!(omitted.accounts.stage.is_none());
    assert!(omitted.accounts.prepared.is_empty());
    omitted.assert_sources_unchanged();
}

#[cfg(target_os = "linux")]
#[test]
fn saved_connections_review_refuses_changed_source_and_back_stops_remaining_imports() {
    let mut environment = tilo_test_env::ScopedEnv::new();
    environment.set(runtime_identity::FILE_SECRETS_ENV, "1");
    let mut changed = SavedBatchFixture::new();
    changed.accounts.apply(AccountRequest::ReviewSaved).unwrap();
    let tilo_source = changed.accounts.inventory.tilo_accounts[0]
        .source_id
        .clone();
    for id in [tilo_source, changed.nextcloud_id.clone()] {
        changed
            .accounts
            .apply(AccountRequest::ExcludeSaved(id))
            .unwrap();
    }
    let source = changed.roots.config.join("rclone/rclone.conf");
    let replacement = std::fs::read_to_string(&source)
        .unwrap()
        .replace("team.example.test", "different.example.test");
    std::fs::write(&source, &replacement).unwrap();
    changed.accounts.apply(AccountRequest::ImportSaved).unwrap();
    settle(&mut changed.accounts);
    assert!(changed.accounts.prepared.is_empty());
    assert!(
        changed
            .accounts
            .saved
            .attention
            .contains_key(&changed.rclone_id)
    );
    assert_eq!(
        changed.accounts.continue_label(),
        "installer-accounts-continue-without"
    );
    assert_eq!(std::fs::read_to_string(&source).unwrap(), replacement);
    assert!(changed.browser.opened.lock().unwrap().is_empty());
    let mut stopped = SavedBatchFixture::new();
    stopped.accounts.apply(AccountRequest::ImportSaved).unwrap();
    stopped.accounts.apply(AccountRequest::Back).unwrap();
    settle(&mut stopped.accounts);
    assert_eq!(stopped.accounts.prepared.len(), 1);
    assert_eq!(stopped.accounts.prepared[0].id, stopped.canonical_id);
    assert!(stopped.accounts.saved.attention.is_empty());
    stopped.assert_sources_unchanged();
}

#[cfg(unix)]
#[test]
fn unavailable_saved_rclone_cloud_grants_offer_explicit_reconnect_without_opening_browser() {
    let mut environment = tilo_test_env::ScopedEnv::new();
    environment.set(runtime_identity::FILE_SECRETS_ENV, "1");
    let temporary = tempfile::tempdir().unwrap();
    let home = temporary.path().join("source");
    let config = home.join(".config");
    std::fs::create_dir_all(config.join("rclone")).unwrap();
    let source = config.join("rclone/rclone.conf");
    let original = "[Drive]\ntype=drive\ntoken=invalid-source-grant\n[Microsoft]\ntype=onedrive\ntoken={}\n[Dropbox]\ntype=dropbox\ntoken={}\n";
    std::fs::write(&source, original).unwrap();
    let roots = SourceRoots { home, config };
    let inventory = super::super::super::account_transfer::discover(&roots);
    let issuer = tilo_oauth::fake::FakeAuthorizationServer::start();
    let browser = Browser::new(false);
    for candidate in &inventory.candidates {
        let cloud = match candidate.provider {
            Provider::Google => account_custody::OAuthCloud::GoogleDrive,
            Provider::Microsoft => account_custody::OAuthCloud::OneDrive,
            Provider::Dropbox => account_custody::OAuthCloud::Dropbox,
            _ => panic!("unexpected candidate"),
        };
        let mut accounts = Accounts::default();
        accounts.opener = browser.clone();
        accounts.actions = AccountActions::default().with_custodian(
            account_custody::Custodian::for_root(None).with_provider(
                cloud,
                issuer.provider(),
                "new-tilo-client",
            ),
        );
        accounts
            .update_sources(roots.clone(), inventory.clone())
            .unwrap();
        accounts
            .apply(AccountRequest::Source(candidate.id.clone()))
            .unwrap();
        accounts.apply(AccountRequest::Confirm).unwrap();
        settle(&mut accounts);
        assert!(matches!(&accounts.step, Step::Failed { message, .. }
            if *message == "installer-accounts-rclone-source-reconnect"));
        assert!(accounts.prepared.is_empty());
        assert_eq!(
            accounts.apply(AccountRequest::Retry),
            Err("installer-accounts-invalid-action")
        );
        let failed = accounts.view(Rc::new(|_| {}), Rc::new(|_, _| {}));
        assert!(account_widget(&failed, "installer.account.reconnect").is_some());
        assert!(account_widget(&failed, "installer.account.retry").is_none());
        accounts
            .apply(AccountRequest::from_parts("reconnect", None).unwrap())
            .unwrap();
        assert!(
            matches!(&accounts.step, Step::Confirm(Selection::OAuth { candidate: Some(chosen), .. }) if chosen.id == candidate.id)
        );
        assert!(!accounts.actions.busy());
        accounts.apply(AccountRequest::Back).unwrap();
        assert!(accounts.prepared.is_empty());
    }
    assert!(browser.opened.lock().unwrap().is_empty());
    assert!(issuer.requests().is_empty());
    assert_eq!(std::fs::read_to_string(&source).unwrap(), original);
}

#[cfg(target_os = "linux")]
#[test]
fn unavailable_selected_goa_credentials_offer_explicit_bound_reconnect_without_keyring_or_browser()
{
    use std::os::unix::fs::PermissionsExt;
    let mut environment = tilo_test_env::ScopedEnv::new();
    environment.set(runtime_identity::FILE_SECRETS_ENV, "1");
    let temporary = tempfile::tempdir().unwrap();
    let home = temporary.path().join("source");
    let config = home.join(".config");
    std::fs::create_dir_all(config.join("goa-1.0")).unwrap();
    let source = config.join("goa-1.0/accounts.conf");
    let original = "[Account account_1]\nProvider=google\nIdentity=person@example.test\nPresentationIdentity=Personal Drive\nFilesEnabled=true\n";
    std::fs::write(&source, original).unwrap();
    // An uncontrolled config is refused before asking the actual owner keyring.
    std::fs::set_permissions(&source, std::fs::Permissions::from_mode(0o666)).unwrap();
    let roots = SourceRoots { home, config };
    let inventory = super::super::super::account_transfer::discover(&roots);
    let selected = inventory.candidates[0].clone();
    assert!(selected.can_import_credentials);
    let issuer = tilo_oauth::fake::FakeAuthorizationServer::start();
    let browser = Browser::new(false);
    let mut accounts = Accounts::default();
    accounts.opener = browser.clone();
    accounts.actions = AccountActions::default().with_custodian(
        account_custody::Custodian::for_root(None).with_provider(
            account_custody::OAuthCloud::GoogleDrive,
            issuer.provider(),
            "new-tilo-client",
        ),
    );
    accounts.update_sources(roots, inventory).unwrap();
    accounts
        .apply(AccountRequest::Source(selected.id.clone()))
        .unwrap();
    accounts.apply(AccountRequest::Confirm).unwrap();
    settle(&mut accounts);
    assert!(matches!(&accounts.step, Step::Failed { message, .. }
        if *message == "installer-accounts-goa-source-reconnect"));
    assert!(accounts.prepared.is_empty());
    assert_eq!(
        accounts.apply(AccountRequest::Retry),
        Err("installer-accounts-invalid-action")
    );
    let failed = accounts.view(Rc::new(|_| {}), Rc::new(|_, _| {}));
    assert!(account_widget(&failed, "installer.account.reconnect").is_some());
    assert!(account_widget(&failed, "installer.account.retry").is_none());
    accounts
        .apply(AccountRequest::from_parts("reconnect", None).unwrap())
        .unwrap();
    assert!(
        matches!(&accounts.step, Step::Confirm(Selection::OAuth { candidate: Some(candidate), .. })
        if candidate.id == selected.id && candidate.identity == selected.identity)
    );
    assert!(!accounts.actions.busy());
    assert!(browser.opened.lock().unwrap().is_empty());
    assert!(issuer.requests().is_empty());
    assert_eq!(std::fs::read_to_string(source).unwrap(), original);
}

#[cfg(target_os = "linux")]
#[test]
fn saved_account_overview_names_real_sources_without_importing_or_changing_consent() {
    let mut environment = tilo_test_env::ScopedEnv::new();
    environment.set(runtime_identity::FILE_SECRETS_ENV, "1");
    let mut fixture = SavedBatchFixture::new();
    let overview = fixture.accounts.view(Rc::new(|_| {}), Rc::new(|_, _| {}));
    let nextcloud = account_widget(
        &overview,
        &format!("installer.account.detected.{}", fixture.nextcloud_id),
    )
    .unwrap();
    let name = nextcloud.accessibility().unwrap().name.unwrap();
    assert!(name.contains(&t("installer-accounts-nextcloud")));
    assert!(name.contains("ada"));
    let canonical = account_widget(
        &overview,
        &format!(
            "installer.account.detected.{}",
            fixture.accounts.inventory.tilo_accounts[0].source_id
        ),
    )
    .unwrap();
    let name = canonical.accessibility().unwrap().name.unwrap();
    assert!(name.contains(&t("installer-accounts-webdav")));
    assert!(name.contains("Canonical files"));
    assert!(!fixture.accounts.can_continue());
    assert!(!fixture.accounts.saved.reviewed);
    assert!(fixture.accounts.prepared.is_empty());
    assert!(fixture.accounts.stage.is_none());
    fixture.assert_sources_unchanged();
    fixture.accounts.apply(AccountRequest::SkipSaved).unwrap();
    assert!(fixture.accounts.can_continue());
    assert!(fixture.accounts.prepared.is_empty());
    let declined = fixture.accounts.view(Rc::new(|_| {}), Rc::new(|_, _| {}));
    let nextcloud = account_widget(
        &declined,
        &format!("installer.account.detected.{}", fixture.nextcloud_id),
    )
    .unwrap();
    assert_eq!(
        nextcloud.accessibility().unwrap().value,
        Some(t("installer-accounts-left-out"))
    );
    fixture.assert_sources_unchanged();
}

#[test]
fn detected_account_label_preserves_provider_display_name_and_known_identity() {
    let candidate = Candidate {
        id: "metadata-source".into(),
        provider: Provider::Nextcloud,
        origin: super::super::super::account_transfer::Origin::Nextcloud,
        label: "Team files".into(),
        identity: Some("ada@example.test".into()),
        server: Some("https://cloud.example.test".into()),
        profile: "unused-metadata-only".into(),
        needs_sign_in: false,
        can_import_credentials: true,
        services: Vec::new(),
    };
    let label = candidate_label(&candidate);
    assert!(label.contains(&t("installer-accounts-nextcloud")));
    assert!(label.contains("Team files"));
    assert!(label.contains("ada@example.test"));
    assert!(!label.contains("https://cloud.example.test"));
}

#[test]
fn detected_spotify_profiles_offer_configured_playback_without_moving_the_source_session() {
    for relative in [
        ".config/spotify",
        ".var/app/com.spotify.Client/config/spotify",
    ] {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let config = home.join(".config");
        let profile = home.join(relative);
        std::fs::create_dir_all(&config).unwrap();
        std::fs::create_dir_all(&profile).unwrap();
        let session = profile.join("session");
        let original = b"opaque desktop profile, not an OAuth grant";
        std::fs::write(&session, original).unwrap();
        let roots = SourceRoots { home, config };
        let inventory = super::super::super::account_transfer::discover(&roots);
        let candidate = inventory
            .candidates
            .iter()
            .find(|candidate| candidate.provider == Provider::Spotify)
            .unwrap()
            .clone();
        assert!(!candidate.can_import_credentials);
        let browser = Browser::new(false);
        let mut accounts = Accounts::default();
        accounts.opener = browser.clone();
        accounts.actions = AccountActions::default().with_custodian(
            account_custody::Custodian::for_root(None).with_client_table(
                account_custody::ClientTable::parse(
                    r#"
[spotify]
client_id = "tilo-test-registered-client"
[spotify.scopes]
music_streaming = ["streaming", "user-read-private"]
"#,
                )
                .unwrap(),
            ),
        );
        accounts.update_sources(roots, inventory).unwrap();
        let view = accounts.view(Rc::new(|_| {}), Rc::new(|_, _| {}));
        let row =
            account_widget(&view, &format!("installer.account.source.{}", candidate.id)).unwrap();
        let accessible = row.accessibility().unwrap();
        assert!(!accessible.state.disabled);
        assert!(
            accessible
                .value
                .unwrap()
                .contains(&t("installer-accounts-spotify-source-sign-in"))
        );
        accounts
            .apply(AccountRequest::Source(candidate.id.clone()))
            .unwrap();
        assert_eq!(accounts.projection()["state"], "spotify-purpose");
        assert_eq!(
            accounts.subheading(),
            t("installer-accounts-spotify-source-reconnect")
        );
        let purpose = accounts.view(Rc::new(|_| {}), Rc::new(|_, _| {}));
        assert!(account_widget(&purpose, "installer.account.spotify.spotify").is_none());
        assert!(account_widget(&purpose, "installer.account.spotify.spotify-streaming").is_some());
        assert_eq!(
            accounts.apply(AccountRequest::Provider("google-drive".into())),
            Err("installer-accounts-invalid-action")
        );
        assert_eq!(
            accounts.apply(AccountRequest::Provider("spotify".into())),
            Err("installer-accounts-unavailable")
        );
        accounts
            .apply(AccountRequest::Provider("spotify-streaming".into()))
            .unwrap();
        assert!(matches!(&accounts.step, Step::Confirm(Selection::OAuth {
            provider: ConnectionProvider::SpotifyStreaming,
            candidate: Some(selected),
        }) if selected.id == candidate.id));
        assert_eq!(
            accounts.subheading(),
            t("installer-accounts-spotify-playback-intro")
        );
        accounts.apply(AccountRequest::Back).unwrap();
        assert!(
            matches!(&accounts.step, Step::SpotifyPurpose(Some(selected)) if selected.id == candidate.id)
        );
        // Withdrawal is checked by the same semantic action before it starts any OAuth work.
        accounts.actions = AccountActions::default().with_custodian(
            account_custody::Custodian::for_root(None)
                .with_client_table(account_custody::ClientTable::parse("").unwrap()),
        );
        assert_eq!(
            accounts.apply(AccountRequest::Provider("spotify-streaming".into())),
            Err("installer-accounts-unavailable")
        );
        assert!(
            matches!(&accounts.step, Step::SpotifyPurpose(Some(selected)) if selected.id == candidate.id)
        );
        accounts.apply(AccountRequest::Back).unwrap();
        assert!(matches!(accounts.step, Step::Overview));
        assert!(accounts.stage.is_none());
        assert!(accounts.prepared.is_empty());
        assert!(accounts.prepared_handle().is_none());
        assert!(browser.opened.lock().unwrap().is_empty());
        assert_eq!(std::fs::read(&session).unwrap(), original);
    }
}
