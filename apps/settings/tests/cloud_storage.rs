//! Cloud storage end to end, for every provider: the person connects the
//! account in Settings (through the fake authorization server, or Apple's
//! sign-in on the fake iCloud), custody publishes the short-lived handoff
//! credential, the storage daemon's cloud service mounts the account
//! against the provider's fake server, a file is written and read back, and
//! removing the account in Settings unmounts it and deletes its cache.
//!
//! Settings and the daemon meet only on disk: the account store, the vault
//! (Settings' side) and the handoff store (the daemon's side), all under
//! one private config root. The daemon's credential refresher is the same
//! custodian the credential helper runs.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::io::Read as _;
use std::path::Path;
use std::sync::Arc;

use account_custody::icloud::WebICloud;
use account_custody::{Custodian, OAuthCloud, Registration};
use automerge::AutoCommit;
use automerge::transaction::Transactable as _;
use cloud_source::StaticCredential;
use fs::cloud::{CloudService, CloudVolume, InProcessLauncher, ServiceConfig};
use settings::accounts::{AccountKind, AccountRoot};
use settings::panes::accounts::{
    AccountViewCommand, AccountsPane, BrowserOpener, REMOVE_CONFIRM_KEY,
};
use tilo_oauth::fake::FakeAuthorizationServer;
use ui::widget::Widget as _;

/// Plays the system browser, following redirects back to the loopback
/// listener.
struct Browser;

impl BrowserOpener for Browser {
    fn open(&self, url: &str) -> ui::portal::LaunchChannel {
        let url = url.to_string();
        std::thread::spawn(move || {
            let _ = tilo_oauth::fake::browse(&url);
        });
        let (sender, receiver) = std::sync::mpsc::channel();
        let _ = sender.send(ui::portal::LaunchOutcome::Opened);
        receiver
    }
}

/// Records the authorization URL instead of following it.
struct Recorder(std::sync::Mutex<Vec<String>>);

impl BrowserOpener for Recorder {
    fn open(&self, url: &str) -> ui::portal::LaunchChannel {
        self.0.lock().unwrap().push(url.to_string());
        let (sender, receiver) = std::sync::mpsc::channel();
        let _ = sender.send(ui::portal::LaunchOutcome::Opened);
        receiver
    }
}

/// One provider's fakes and how the person signs in to it.
struct Case {
    kind: AccountKind,
    custodian: Custodian,
    /// Apple ID, password and code for iCloud; `None` for `OAuth`.
    icloud: Option<(&'static str, &'static str, &'static str)>,
    /// A file already on the server, when the fake can hold one.
    seeded: Option<(&'static str, &'static [u8])>,
    /// Keeps the fake servers running.
    _servers: Box<dyn std::any::Any>,
}

/// The fake authorization server, this app's registration with it, and the
/// storage configuration pointing at the provider's fake.
struct OAuthFake {
    auth: FakeAuthorizationServer,
    registration: Registration,
    storage: serde_json::Value,
}

fn oauth_case(
    kind: AccountKind,
    cloud: OAuthCloud,
    fake: OAuthFake,
    root: &AccountRoot,
    seeded: Option<(&'static str, &'static [u8])>,
    server: Box<dyn std::any::Any>,
) -> Case {
    let OAuthFake {
        auth,
        registration,
        storage,
    } = fake;
    let custodian = root
        .custodian()
        .with_provider(cloud, auth.provider(), registration)
        .with_storage_config(cloud, storage);
    Case {
        kind,
        custodian,
        icloud: None,
        seeded,
        _servers: Box::new((auth, server)),
    }
}

fn google_drive(root: &AccountRoot) -> Case {
    let drive = cloud_source_gdrive::fake::FakeDrive::start();
    drive.put_file(&drive.root_id(), "hello.txt", b"from Google Drive");
    let auth = FakeAuthorizationServer::start();
    auth.issue_access_token(cloud_source_gdrive::fake::SOURCE_TOKEN);
    // Google's installed-app clients must send their client secret.
    auth.require_client_secret("desktop-client-secret");
    let registration = Registration {
        client_secret: Some("desktop-client-secret".into()),
        ..Registration::public("drive-client")
    };
    let storage = serde_json::to_value(drive.config()).unwrap();
    oauth_case(
        AccountKind::GoogleDrive,
        OAuthCloud::GoogleDrive,
        OAuthFake {
            auth,
            registration,
            storage,
        },
        root,
        Some(("hello.txt", b"from Google Drive")),
        Box::new(drive),
    )
}

fn onedrive(root: &AccountRoot) -> Case {
    let graph = cloud_source_onedrive::fake::FakeGraph::personal();
    graph.put("hello.txt", b"from OneDrive");
    let auth = FakeAuthorizationServer::start();
    auth.issue_access_token(cloud_source_onedrive::fake::TOKEN);
    let storage =
        serde_json::to_value(graph.config(cloud_source_onedrive::RootSelector::DriveRoot)).unwrap();
    oauth_case(
        AccountKind::OneDrive,
        OAuthCloud::OneDrive,
        OAuthFake {
            auth,
            registration: Registration::public("onedrive-client"),
            storage,
        },
        root,
        Some(("hello.txt", b"from OneDrive")),
        Box::new(graph),
    )
}

fn dropbox(root: &AccountRoot) -> Case {
    let dropbox = cloud_source_dropbox::fake::FakeDropbox::team();
    dropbox.put("/hello.txt", b"from Dropbox");
    let auth = FakeAuthorizationServer::start();
    auth.issue_access_token(cloud_source_dropbox::fake::TOKEN);
    // Dropbox matches redirect URIs exactly: the listener takes one of the
    // registered ports.
    let port = std::net::TcpListener::bind(("127.0.0.1", 0))
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let registration = Registration {
        redirect_ports: tilo_oauth::RedirectPorts::Registered(vec![port]),
        paste_code_fallback: true,
        ..Registration::public("dropbox-key")
    };
    let mut config = cloud_source_dropbox::DropboxConfig::new("");
    config.endpoints = dropbox.endpoints();
    let storage = serde_json::to_value(config).unwrap();
    oauth_case(
        AccountKind::Dropbox,
        OAuthCloud::Dropbox,
        OAuthFake {
            auth,
            registration,
            storage,
        },
        root,
        Some(("hello.txt", b"from Dropbox")),
        Box::new(dropbox),
    )
}

fn icloud(root: &AccountRoot) -> Case {
    use cloud_source_icloud::fake::{APPLE_ID, DEVICE_CODE, FakeICloud, PASSWORD};
    let fake = FakeICloud::start();
    let custodian = root
        .custodian()
        .with_icloud(Arc::new(WebICloud::new(fake.endpoints()).unwrap()));
    Case {
        kind: AccountKind::ICloudDrive,
        custodian,
        icloud: Some((APPLE_ID, PASSWORD, DEVICE_CODE)),
        seeded: None,
        _servers: Box::new(fake),
    }
}

fn command(
    pane: &mut AccountsPane,
    commands: &ui::LocalState<Option<AccountViewCommand>>,
    doc: &mut AutoCommit,
    command: AccountViewCommand,
) {
    commands.set(Some(command));
    pane.sync(doc, &automerge::ROOT);
}

/// Connect the account through the Settings pane, as the controls do.
fn sign_in(
    case: &Case,
    pane: &mut AccountsPane,
    commands: &ui::LocalState<Option<AccountViewCommand>>,
    doc: &mut AutoCommit,
) {
    command(pane, commands, doc, AccountViewCommand::Add(case.kind));
    assert!(pane.sign_in_open());
    if let Some((apple_id, password, _)) = case.icloud {
        pane.set_sign_in_fields_for_test("", "", apple_id, password);
    }
    command(pane, commands, doc, AccountViewCommand::Submit);
    assert!(
        pane.finish_sign_in_step_for_test(tilo_test_env::hang_bound()),
        "the sign-in step never reported back within the hang bound"
    );
    if let Some((_, _, code)) = case.icloud {
        assert!(
            pane.sign_in_awaiting_code_for_test(),
            "{:?}",
            pane.sign_in_message_for_test()
        );
        pane.set_sign_in_code_for_test(code);
        command(pane, commands, doc, AccountViewCommand::Submit);
        assert!(
            pane.finish_sign_in_step_for_test(tilo_test_env::hang_bound()),
            "the sign-in step never reported back within the hang bound"
        );
    }
    assert!(
        !pane.sign_in_open(),
        "{:?}",
        pane.sign_in_message_for_test()
    );
}

fn read_all(volume: &CloudVolume, path: &str) -> Vec<u8> {
    let ino = volume.resolve(path).unwrap();
    let size = volume.getattr(ino).unwrap().size;
    volume
        .read_ino(ino, 0, u32::try_from(size).unwrap())
        .unwrap()
}

fn write_file(volume: &CloudVolume, name: &str, bytes: &[u8]) {
    let root = volume.resolve("").unwrap();
    let (_, fh) = volume.create(root, name).unwrap();
    volume.write(fh, 0, bytes).unwrap();
    volume.release(fh).unwrap();
}

/// What another device sees on the server: the file read through a fresh
/// connection with the account's stored configuration.
fn on_server(
    account: &accounts::Account,
    credential: cloud_source::Credential,
    name: &str,
) -> Vec<u8> {
    let spec = fs::cloud::providers::spec_for(account).expect("a mountable account");
    let source = fs::cloud::providers::open(&spec, Arc::new(StaticCredential(credential))).unwrap();
    let item = source
        .list(&source.root())
        .unwrap()
        .into_iter()
        .find(|item| item.name == name)
        .unwrap_or_else(|| panic!("{name} is on the server"));
    let mut bytes = Vec::new();
    source
        .read(&item.id, None, None)
        .unwrap()
        .body
        .read_to_end(&mut bytes)
        .unwrap();
    bytes
}

fn run(case: &Case, root_dir: &Path, root: &AccountRoot) {
    let home = tempfile::tempdir().unwrap();
    let commands = ui::LocalState::new(None);
    let mut pane = AccountsPane::in_root(root.clone())
        .with_command_state(&commands)
        .with_custodian(case.custodian.clone())
        .with_browser_opener(Arc::new(Browser));
    let mut doc = AutoCommit::new();

    sign_in(case, &mut pane, &commands, &mut doc);
    let store = root.store();
    let account = store
        .list(&accounts::AccountTypeId::from(case.kind.type_id()))
        .first()
        .copied()
        .cloned()
        .expect("the signed-in account");
    assert!(
        account
            .config
            .get(accounts::cloud_types::MOUNT_CONFIG_KEY)
            .is_some_and(|mount| !mount.is_null()),
        "the provider's storage is resolved into the account's configuration: {}",
        account.config
    );
    let store_text = std::fs::read_to_string(root_dir.join("accounts/store.json")).unwrap();
    assert!(
        !store_text.contains("refresh-"),
        "no grant in account metadata"
    );

    // The daemon: one mount per account, credentials only from the handoff.
    let launcher = InProcessLauncher::default();
    let mut service = CloudService::new(
        ServiceConfig {
            config_root: Some(root_dir.to_path_buf()),
            mounts_root: home.path().join("Remote"),
            state_root: home.path().join("state"),
            refresher: Arc::new(case.custodian.clone()),
        },
        Box::new(launcher.clone()),
    );
    service.reconcile();
    let mounted = service.mounted();
    assert_eq!(mounted.len(), 1, "{:?}", root.statuses());
    assert_eq!(
        mounted[0].1,
        home.path().join("Remote").join(case.kind.label())
    );
    let volume = launcher
        .volume(account.id.as_str())
        .expect("a running volume");
    volume.sync_once().unwrap();
    if let Some((name, bytes)) = case.seeded {
        assert_eq!(read_all(&volume, name), bytes);
    }
    write_file(&volume, "note.txt", b"written through the mount");
    volume.upload_pending().unwrap();
    assert_eq!(read_all(&volume, "note.txt"), b"written through the mount");

    let handoff = accounts::HandoffStore::open_in(root_dir)
        .read(&account.id)
        .unwrap()
        .expect("custody published a handoff credential");
    assert!(
        !matches!(&handoff, accounts::HandoffCredential::Bearer { token, .. } if token.starts_with("refresh-")),
        "the daemon never sees a refresh token"
    );
    assert_eq!(
        on_server(&account, handoff.to_cloud(), "note.txt"),
        b"written through the mount"
    );
    service.pump();

    // Removing the account in Settings unmounts it and drops its cache.
    let state_dir = home.path().join("state");
    assert!(state_dir.read_dir().unwrap().next().is_some());
    doc.put(
        &automerge::ROOT,
        format!("settings.accounts.remove.{}", account.id.as_str()),
        true,
    )
    .unwrap();
    pane.sync(&mut doc, &automerge::ROOT);
    doc.put(&automerge::ROOT, REMOVE_CONFIRM_KEY, true).unwrap();
    pane.sync(&mut doc, &automerge::ROOT);
    assert!(
        root.store().get(&account.id).is_none(),
        "removed in Settings"
    );
    service.reconcile();
    assert!(service.mounted().is_empty());
    assert!(launcher.volume(account.id.as_str()).is_none());
    assert!(!home.path().join("Remote").join(case.kind.label()).exists());
    assert!(
        state_dir.read_dir().unwrap().next().is_none(),
        "the account's cache and queue are gone"
    );
    service.shutdown();
}

fn scratch() -> (tempfile::TempDir, std::path::PathBuf, AccountRoot) {
    let dir = tempfile::tempdir().unwrap();
    let root_dir = dir.path().join("tilo");
    let root = AccountRoot::at(&root_dir);
    (dir, root_dir, root)
}

#[test]
fn google_drive_signs_in_mounts_writes_and_unmounts() {
    let (_dir, root_dir, root) = scratch();
    run(&google_drive(&root), &root_dir, &root);
}

#[test]
fn onedrive_signs_in_mounts_writes_and_unmounts() {
    let (_dir, root_dir, root) = scratch();
    run(&onedrive(&root), &root_dir, &root);
}

#[test]
fn dropbox_signs_in_mounts_writes_and_unmounts() {
    let (_dir, root_dir, root) = scratch();
    run(&dropbox(&root), &root_dir, &root);
}

#[test]
fn icloud_drive_signs_in_mounts_writes_and_unmounts() {
    let (_dir, root_dir, root) = scratch();
    run(&icloud(&root), &root_dir, &root);
}

#[test]
fn a_dropbox_code_can_be_pasted_when_every_registered_port_is_taken() {
    let (_dir, _root_dir, root) = scratch();
    let dropbox = cloud_source_dropbox::fake::FakeDropbox::start();
    let auth = FakeAuthorizationServer::start();
    auth.issue_access_token(cloud_source_dropbox::fake::TOKEN);
    let taken = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let registration = Registration {
        redirect_ports: tilo_oauth::RedirectPorts::Registered(vec![
            taken.local_addr().unwrap().port(),
        ]),
        paste_code_fallback: true,
        ..Registration::public("dropbox-key")
    };
    let mut config = cloud_source_dropbox::DropboxConfig::new("");
    config.endpoints = dropbox.endpoints();
    let custodian = root
        .custodian()
        .with_provider(OAuthCloud::Dropbox, auth.provider(), registration)
        .with_storage_config(OAuthCloud::Dropbox, serde_json::to_value(config).unwrap());
    let browser = Arc::new(Recorder(std::sync::Mutex::new(Vec::new())));
    let commands = ui::LocalState::new(None);
    let mut pane = AccountsPane::in_root(root.clone())
        .with_command_state(&commands)
        .with_custodian(custodian)
        .with_browser_opener(Arc::clone(&browser) as Arc<dyn BrowserOpener>);
    let mut doc = AutoCommit::new();
    command(
        &mut pane,
        &commands,
        &mut doc,
        AccountViewCommand::Add(AccountKind::Dropbox),
    );
    command(&mut pane, &commands, &mut doc, AccountViewCommand::Submit);
    assert!(pane.sign_in_awaiting_code_for_test());
    let url = browser.0.lock().unwrap()[0].clone();
    assert!(
        !url.contains("redirect_uri"),
        "Dropbox shows the code: {url}"
    );
    // The person reads the code off Dropbox's page and pastes it.
    let code = tilo_oauth::fake::browse(&url).unwrap();
    pane.set_sign_in_code_for_test(&code);
    command(&mut pane, &commands, &mut doc, AccountViewCommand::Submit);
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
        root.store()
            .list(&accounts::AccountTypeId::from(
                AccountKind::Dropbox.type_id()
            ))
            .len(),
        1
    );
    drop(taken);
}
