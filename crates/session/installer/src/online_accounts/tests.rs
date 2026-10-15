use super::*;
use std::os::unix::fs::PermissionsExt;
use std::sync::atomic::AtomicUsize;
use std::time::{Duration, Instant};
use tilo_oauth::fake::{FakeAuthorizationServer, browse};

struct FixtureBrowser {
    visit: bool,
    urls: mpsc::SyncSender<String>,
    closes: AtomicUsize,
}

impl BrowserOpener for FixtureBrowser {
    fn open(&self, url: &str) -> Result<(), browser::BrowserError> {
        self.urls.try_send(url.to_string()).unwrap();
        if self.visit {
            let url = url.to_string();
            std::thread::spawn(move || {
                let _ = browse(&url);
            });
        }
        Ok(())
    }

    fn close(&self) -> Result<(), browser::BrowserError> {
        self.closes.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

fn setup(
    visit: bool,
) -> (
    tempfile::TempDir,
    FakeAuthorizationServer,
    Arc<FixtureBrowser>,
    mpsc::Receiver<String>,
    AccountsController,
) {
    let directory = tempfile::tempdir().unwrap();
    std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let server = FakeAuthorizationServer::start();
    let (sender, urls) = mpsc::sync_channel(1);
    let opener = Arc::new(FixtureBrowser {
        visit,
        urls: sender,
        closes: AtomicUsize::new(0),
    });
    let provider = server.provider();
    let controller = AccountsController::fixture(directory.path(), opener.clone(), |custody| {
        custody.with_provider(
            OAuthCloud::GoogleDrive,
            provider,
            account_custody::Registration::public("installer-fixture"),
        )
    })
    .unwrap();
    (directory, server, opener, urls, controller)
}

fn request() -> AccountRequest {
    AccountRequest {
        provider: Provider::Google,
        email: String::new(),
        server: String::new(),
        files: true,
        mail: false,
        calendar: false,
    }
}

fn until(mut predicate: impl FnMut() -> bool) {
    let deadline = Instant::now().checked_add(Duration::from_secs(10)).unwrap();
    while !predicate() {
        assert!(Instant::now() < deadline, "account worker did not finish");
        std::thread::yield_now();
    }
}

#[test]
fn local_pkce_callback_saves_only_private_canonical_custody_and_stages_selection() {
    let (_directory, server, opener, urls, mut controller) = setup(true);
    controller.command(AccountsCommand::Add(request())).unwrap();
    let _ = urls.recv_timeout(Duration::from_secs(10)).unwrap();
    until(|| controller.poll() != AccountsStatus::SigningIn);
    assert!(controller.status() == AccountsStatus::Added);
    assert_eq!(controller.selected().len(), 1);
    assert_eq!(AccountStore::load_in(&controller.root).len(), 1);
    assert_eq!(server.verifiers().len(), 1);
    let selected = controller.selected().to_vec();
    assert!(
        SecretVault::open_read_only_in(&controller.root)
            .get(&selected[0])
            .is_some()
    );
    assert!(opener.closes.load(Ordering::SeqCst) >= 2);
    let binding = PortableBinding::new("install", "disk", "alice", 1000).unwrap();
    let password = b"an explicit transport password";
    let artifact = controller
        .stage_selected(&selected, password, &binding)
        .unwrap();
    artifact
        .verify_selection(password, binding.as_bytes(), &selected)
        .unwrap();
    let wrong = PortableBinding::new("install", "disk", "bob", 1001).unwrap();
    assert!(
        artifact
            .verify_selection(password, wrong.as_bytes(), &selected)
            .is_err()
    );
    assert!(
        controller
            .stage_selected(&[AccountId::new("foreign")], password, &binding)
            .is_err()
    );
    let key = tilo_seal::SymmetricKey::generate().unwrap();
    let sealed = controller
        .stage_selected_with_key(&selected, &key, &binding)
        .unwrap();
    let destination = tempfile::tempdir().unwrap();
    assert!(
        portable::import_sealed_accounts(destination.path(), &sealed, &key, wrong.as_bytes())
            .is_err()
    );
    let foreign_key = tilo_seal::SymmetricKey::generate().unwrap();
    assert!(
        portable::import_sealed_accounts(
            destination.path(),
            &sealed,
            &foreign_key,
            binding.as_bytes(),
        )
        .is_err()
    );
    assert_eq!(
        portable::import_sealed_accounts(destination.path(), &sealed, &key, binding.as_bytes())
            .unwrap(),
        selected
    );
    assert!(
        controller
            .stage_selected_with_key(&[], &key, &binding)
            .is_err()
    );
    assert!(
        controller
            .stage_selected_with_key(&[AccountId::new("foreign")], &key, &binding)
            .is_err()
    );
    let root = controller.root.clone();
    drop(controller);
    assert!(!root.exists());
}

#[test]
fn cancelling_a_pending_callback_closes_the_listener_without_writing() {
    let (_directory, _server, opener, urls, mut controller) = setup(false);
    controller.command(AccountsCommand::Add(request())).unwrap();
    let url = url::Url::parse(&urls.recv_timeout(Duration::from_secs(10)).unwrap()).unwrap();
    let redirect = url
        .query_pairs()
        .find(|(key, _)| key == "redirect_uri")
        .unwrap()
        .1
        .into_owned();
    let redirect = url::Url::parse(&redirect).unwrap();
    controller.command(AccountsCommand::Cancel).unwrap();
    until(|| !controller.worker_alive.load(Ordering::Acquire));
    assert!(std::net::TcpStream::connect(("127.0.0.1", redirect.port().unwrap())).is_err());
    assert!(controller.poll() == AccountsStatus::Ready);
    assert!(controller.selected().is_empty());
    assert_eq!(AccountStore::load_in(&controller.root).len(), 0);
    assert!(opener.closes.load(Ordering::SeqCst) >= 2);
}

#[test]
fn late_exchanged_tokens_after_cancel_are_never_persisted() {
    let (_directory, server, _opener, urls, mut controller) = setup(true);
    let gate = server.hold_token_exchange();
    controller.command(AccountsCommand::Add(request())).unwrap();
    let _ = urls.recv_timeout(Duration::from_secs(10)).unwrap();
    assert!(gate.wait_for_exchange(Duration::from_secs(10)));
    controller.command(AccountsCommand::SkipOffline).unwrap();
    gate.release();
    until(|| !controller.worker_alive.load(Ordering::Acquire));
    assert!(controller.poll() == AccountsStatus::Ready);
    assert!(controller.selected().is_empty());
    assert!(!accounts::store_path_in(&controller.root).exists());
    assert!(!accounts::vault_dir_in(&controller.root).exists());
}

#[test]
fn completed_but_unpolled_authorization_is_discarded_on_cancel() {
    let (_directory, _server, _opener, urls, mut controller) = setup(true);
    controller.command(AccountsCommand::Add(request())).unwrap();
    let _ = urls.recv_timeout(Duration::from_secs(10)).unwrap();
    until(|| !controller.worker_alive.load(Ordering::Acquire));
    controller.command(AccountsCommand::Cancel).unwrap();
    assert!(controller.poll() == AccountsStatus::Ready);
    assert!(controller.selected().is_empty());
    assert!(!accounts::store_path_in(&controller.root).exists());
}

#[test]
fn dropping_a_pending_authorization_closes_the_listener_and_removes_custody() {
    let (_directory, _server, opener, urls, mut controller) = setup(false);
    controller.command(AccountsCommand::Add(request())).unwrap();
    let url = url::Url::parse(&urls.recv_timeout(Duration::from_secs(10)).unwrap()).unwrap();
    let redirect = url
        .query_pairs()
        .find(|(key, _)| key == "redirect_uri")
        .unwrap()
        .1
        .into_owned();
    let redirect = url::Url::parse(&redirect).unwrap();
    let root = controller.root.clone();
    let alive = controller.worker_alive.clone();
    drop(controller);
    until(|| !alive.load(Ordering::Acquire));
    assert!(!root.exists());
    assert!(std::net::TcpStream::connect(("127.0.0.1", redirect.port().unwrap())).is_err());
    assert!(opener.closes.load(Ordering::SeqCst) >= 2);
}

#[test]
fn forged_state_never_saves_an_account() {
    let (_directory, server, _opener, _urls, mut controller) = setup(true);
    server.forge_state("wrong-state");
    controller.command(AccountsCommand::Add(request())).unwrap();
    until(|| controller.poll() != AccountsStatus::SigningIn);
    assert!(controller.status() == AccountsStatus::Failed);
    assert!(server.verifiers().is_empty());
    assert!(controller.selected().is_empty());
}

#[test]
fn unsupported_capability_rejects_the_entire_request_before_browser_launch() {
    let (_directory, _server, _opener, urls, mut controller) = setup(true);
    for provider in [Provider::Dropbox, Provider::Nextcloud] {
        for files in [false, true] {
            let mut requested = request();
            requested.provider = provider;
            requested.mail = true;
            requested.files = files;
            requested.server = "https://cloud.example.org".into();
            assert_eq!(
                controller.command(AccountsCommand::Add(requested)),
                Err(AccountsError::InvalidRequest)
            );
            assert!(urls.try_recv().is_err());
            assert!(controller.selected().is_empty());
            assert!(controller.status() == AccountsStatus::Failed);
        }
    }
}

#[test]
fn private_root_validation_refuses_shared_or_symlinked_roots() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(
        validate_session(directory.path()),
        Err(AccountsError::InvalidSession)
    );
    let private = directory.path().join("private");
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&private)
        .unwrap();
    let link = directory.path().join("link");
    std::os::unix::fs::symlink(&private, &link).unwrap();
    assert_eq!(validate_session(&link), Err(AccountsError::InvalidSession));
}
