#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "external contract assertions"
)]

use cloud_source::{CloudSource, Credential, ItemId, StaticCredential, UploadTarget};
use cloud_source_gdrive::{DriveConfig, GDrive, Url};
use fs::cloud::volume::{DEFAULT_IDLE_POLL_MAX, ROOT_INO};
use fs::cloud::{CloudVolume, VolumeConfig};
use std::{io::Read, path::Path, sync::Arc, time::Duration};

fn config(root: Option<&ItemId>) -> DriveConfig {
    let endpoint = std::env::var("TILO_RYSTIC_GDRIVE_URL")
        .expect("dedicated local Google Drive twin required");
    let url = Url::parse(&endpoint).unwrap();
    assert_eq!(url.scheme(), "http");
    assert!(matches!(
        url.host_str(),
        Some("localhost" | "127.0.0.1" | "[::1]")
    ));
    let mut config = DriveConfig {
        trash: false,
        ..DriveConfig::my_drive().with_api(endpoint)
    };
    if let Some(root) = root {
        config = config.with_root(root.as_str());
    }
    config
}
fn source(root: Option<&ItemId>) -> GDrive {
    GDrive::new(
        config(root),
        Arc::new(StaticCredential(Credential::Bearer {
            token: zeroize::Zeroizing::new("ya29.tilo-rystic-gdrive".into()),
            expires: None,
        })),
    )
    .unwrap()
}

struct Fixture {
    provider: GDrive,
    root: ItemId,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = self.provider.delete(&self.root, None);
    }
}
fn fixture() -> Fixture {
    let provider = source(None);
    let root = provider
        .create_folder(
            &provider.root(),
            &format!("volume-{}", uuid::Uuid::new_v4()),
        )
        .unwrap()
        .id;
    Fixture { provider, root }
}
fn volume(root: &ItemId, dir: &Path) -> Arc<CloudVolume> {
    CloudVolume::open(
        Arc::new(source(Some(root))),
        VolumeConfig {
            dir: dir.into(),
            cache_budget: 64 << 20,
            poll_interval: Duration::from_secs(30),
            idle_poll_max: DEFAULT_IDLE_POLL_MAX,
        },
        Box::new(|_| {}),
    )
    .unwrap()
}
fn read(volume: &CloudVolume, path: &str) -> Vec<u8> {
    let ino = volume.resolve(path).unwrap();
    volume.read_ino(ino, 0, 4096).unwrap()
}
// This is setup only: mutations asserted by the sync test use real upload.
fn seed_import_file(fixture: &Fixture) -> ItemId {
    let id = uuid::Uuid::new_v4().to_string();
    let payload = serde_json::json!({
        "state_file": { &id: {
            "id": id, "name": "import.txt", "mimeType": "text/plain",
            "parents": [fixture.root.as_str()], "size": "10", "version": "1",
            "headRevisionId": "r1", "md5Checksum": "781e5e245d69b566979b86e28d23f2c7"
        }},
        "state_revision": { format!("{id}/r1"): {
            "fileId": id, "id": "r1", "mimeType": "text/plain", "size": "10",
            "body_b64": "MDEyMzQ1Njc4OQ=="
        }}
    });
    let status = std::process::Command::new("curl")
        .args([
            "--fail",
            "--silent",
            "--show-error",
            "--max-time",
            "15",
            "-o",
            "/dev/null",
            "-H",
            "Content-Type: application/json",
            "--data",
        ])
        .arg(payload.to_string())
        .arg(format!(
            "{}/_rystic/seed",
            config(None).api.trim_end_matches('/')
        ))
        .status()
        .unwrap();
    assert!(status.success(), "setup seed failed");
    ItemId::new(id)
}

struct Published(accounts::HandoffStore);
impl accounts::CredentialRefresher for Published {
    fn refresh(
        &self,
        id: &accounts::AccountId,
        _force: bool,
    ) -> Result<(), accounts::RefreshError> {
        match self.0.read(id) {
            Ok(Some(_)) => Ok(()),
            _ => Err(accounts::RefreshError::NeedsAuthentication),
        }
    }
}

fn import_service(root_id: &ItemId, dirs: &Path) -> fs::cloud::CloudService {
    use fs::cloud::{CloudService, InProcessLauncher, ServiceConfig};
    let root = dirs.join("config");
    std::fs::create_dir(&root).unwrap();
    let account_id = accounts::AccountId::from("rystic-import");
    let mut account =
        accounts::Account::new(account_id.clone(), accounts::cloud_types::GOOGLE_DRIVE);
    account.enabled = true;
    let mut account_config = serde_json::json!({});
    account_config.as_object_mut().unwrap().insert(
        accounts::cloud_types::MOUNT_CONFIG_KEY.into(),
        serde_json::to_value(config(Some(root_id))).unwrap(),
    );
    accounts::cloud_types::set_files_mode(
        &mut account_config,
        accounts::cloud_types::FilesMode::Import,
    );
    account.config = account_config;
    let mut store = accounts::AccountStore::load_in(&root);
    store.upsert(account);
    store.save_in(&root).unwrap();
    let handoff = accounts::HandoffStore::open_in(&root);
    handoff
        .publish(
            &account_id,
            &accounts::HandoffCredential::Bearer {
                token: zeroize::Zeroizing::new("ya29.tilo-rystic-gdrive".into()),
                expires_unix: None,
            },
        )
        .unwrap();
    let launcher = InProcessLauncher::default();
    let mut service = CloudService::new(
        ServiceConfig {
            config_root: Some(root),
            mounts_root: dirs.join("Remote"),
            state_root: dirs.join("state"),
            refresher: Arc::new(Published(handoff)),
            control_socket: None,
            file_socket: None,
        },
        Box::new(launcher),
    );
    service.reconcile();
    assert!(
        service.mounted().is_empty(),
        "import must not start a sync mount"
    );
    service
}

#[test]
fn production_service_and_files_engine_independent_import() {
    use runtime_fs::cloud::{CloudRequest, CloudResponse, ImportReply, ImportRequest};
    let fixture = fixture();
    let id = seed_import_file(&fixture);
    let dirs = tempfile::tempdir().unwrap();
    let mut service = import_service(&fixture.root, dirs.path());
    let mut request = |request| match service.request(
        CloudRequest::Import {
            account: "rystic-import".into(),
            request,
        },
        Duration::from_secs(15),
    ) {
        CloudResponse::Import(reply) => reply,
        other => panic!("unexpected service reply: {other:?}"),
    };
    let ImportReply::Page { entries, .. } = request(ImportRequest::Browse {
        folder: None,
        offset: 0,
    }) else {
        panic!("missing import page")
    };
    let entry = entries
        .iter()
        .find(|entry| entry.id == id.as_str())
        .expect("seeded binary import entry");
    // Run the production socket and descriptor-pinned publication engine too.
    // This does not claim coverage of the Files app's UI/action admission.
    let selected = vec![entry.clone()];
    let destination = dirs.path().join("independent");
    std::fs::create_dir(&destination).unwrap();
    // The import engine intentionally refuses symlink ancestors; resolve the
    // test runner's temporary-root alias during setup, before admission.
    let destination = destination.canonicalize().unwrap();
    let socket = dirs.path().join("files/control.sock");
    service
        .serve_control(&socket, runtime_fs::cloud::CloudSocket::Files)
        .unwrap();
    let destination_identity =
        tilo_cloud_files::pin_destination(destination.to_str().unwrap(), std::iter::empty())
            .unwrap();
    let mut authority = tilo_cloud_files::ImportAuthority::default();
    let folder = destination.to_str().unwrap();
    authority
        .approve(
            "import",
            "rystic-import",
            &selected,
            folder,
            &destination_identity,
            "test-device",
        )
        .unwrap();
    let consent = authority
        .consume(
            "import",
            "rystic-import",
            &selected,
            folder,
            &destination_identity,
            "test-device",
        )
        .unwrap();
    let worker = std::thread::spawn(move || {
        tilo_cloud_files::execute_at(
            &socket,
            consent,
            &std::sync::atomic::AtomicBool::new(false),
            |_| {},
            &[],
        )
    });
    let deadline = tilo_test_env::hang_deadline();
    while !worker.is_finished() {
        service.pump();
        assert!(
            std::time::Instant::now() < deadline,
            "bounded independent import engine"
        );
        std::thread::yield_now();
    }
    let outcome = worker.join().unwrap();
    let tilo_cloud_files::TransferOutcome::Done(receipts) = outcome else {
        panic!("import engine failed: {outcome:?}")
    };
    assert_eq!(receipts.len(), 1);
    let copy = destination.join("import.txt");
    assert_eq!(std::fs::read(&copy).unwrap(), b"0123456789");
    std::fs::write(&copy, b"independent local edit").unwrap();
    let mut remote = Vec::new();
    fixture
        .provider
        .read(&id, None, None)
        .unwrap()
        .body
        .read_to_end(&mut remote)
        .unwrap();
    assert_eq!(remote, b"0123456789");
    assert!(!dirs.path().join("Remote").exists());
    service.shutdown();
}

fn cursor(dir: &Path) -> serde_json::Value {
    let tree: serde_json::Value =
        serde_json::from_slice(&std::fs::read(dir.join("tree.json")).unwrap()).unwrap();
    let cursor = tree.get("cursor").expect("persisted sync cursor");
    assert!(!cursor.is_null());
    cursor.clone()
}

fn durable_cached_read(root: &ItemId, dir: &Path, expected: &[u8]) {
    // A rejected credential distinguishes a durable cache read from silently
    // downloading the same bytes again after restart.
    let offline = GDrive::new(
        config(Some(root)),
        Arc::new(StaticCredential(Credential::Bearer {
            token: zeroize::Zeroizing::new("invalid-test-token".into()),
            expires: None,
        })),
    )
    .unwrap();
    let cached = CloudVolume::open(
        Arc::new(offline),
        VolumeConfig {
            dir: dir.into(),
            cache_budget: 64 << 20,
            poll_interval: Duration::from_secs(30),
            idle_poll_max: DEFAULT_IDLE_POLL_MAX,
        },
        Box::new(|_| {}),
    )
    .unwrap();
    assert_eq!(
        read(&cached, "external-renamed.txt"),
        expected,
        "durable cached bytes without valid provider access"
    );
    cached.shutdown();
}

#[test]
fn production_volume_bothway_bytes_metadata_and_restart() {
    let fixture = fixture();
    let dir = tempfile::tempdir().unwrap();
    let mount = volume(&fixture.root, dir.path());
    mount.sync_once().unwrap();
    let initial_cursor = cursor(dir.path());
    mount.sync_once().unwrap();
    assert_eq!(
        cursor(dir.path()),
        initial_cursor,
        "idle sync reuses cursor"
    );
    let native = fixture
        .provider
        .create_native_document(
            &fixture.root,
            "Read only",
            "application/vnd.google-apps.document",
        )
        .unwrap();
    mount.sync_once().unwrap();
    let native_attr = mount.lookup(ROOT_INO, "Read only.docx").unwrap();
    assert_eq!(native_attr.perm, 0o444);
    assert!(mount.open_file(native_attr.ino, true, false).is_err());
    let (_, handle) = mount.create(ROOT_INO, "local.txt").unwrap();
    mount.write(handle, 0, b"local exact bytes").unwrap();
    mount.release(handle).unwrap();
    mount
        .upload_pending()
        .expect("strict full sync requires real resumable upload support");
    assert_eq!(mount.pending(), 0);
    let remote = fixture
        .provider
        .list(&fixture.root)
        .unwrap()
        .into_iter()
        .find(|i| i.name == "local.txt")
        .unwrap();
    let mut body = Vec::new();
    fixture
        .provider
        .read(&remote.id, None, None)
        .unwrap()
        .body
        .read_to_end(&mut body)
        .unwrap();
    assert_eq!(body, b"local exact bytes");
    let external = b"external provider edit";
    cloud_source::upload(
        &fixture.provider,
        UploadTarget::Replace {
            id: remote.id.clone(),
            expect: remote.revision.clone(),
        },
        external.len() as u64,
        |_| Ok(Box::new(std::io::Cursor::new(external.to_vec()))),
    )
    .unwrap();
    mount.sync_once().unwrap();
    assert_eq!(read(&mount, "local.txt"), external);
    mount
        .rename(ROOT_INO, "local.txt", ROOT_INO, "local-renamed.txt")
        .unwrap();
    mount.upload_pending().unwrap();
    assert_eq!(
        fixture.provider.stat(&remote.id).unwrap().name,
        "local-renamed.txt"
    );
    fixture
        .provider
        .move_item(&remote.id, &fixture.root, "external-renamed.txt", None)
        .unwrap();
    mount.sync_once().unwrap();
    assert!(mount.resolve("local-renamed.txt").is_err());
    assert_eq!(read(&mount, "external-renamed.txt"), external);
    let saved = cursor(dir.path());
    mount.shutdown();
    drop(mount);
    durable_cached_read(&fixture.root, dir.path(), external);
    let mount = volume(&fixture.root, dir.path());
    assert_eq!(read(&mount, "external-renamed.txt"), external);
    mount.sync_once().unwrap();
    assert_eq!(
        cursor(dir.path()),
        saved,
        "restart resumes persisted terminal cursor"
    );
    fixture.provider.delete(&remote.id, None).unwrap();
    mount.sync_once().unwrap();
    assert!(mount.resolve("external-renamed.txt").is_err());
    mount.unlink(ROOT_INO, "Read only.docx").unwrap();
    mount.upload_pending().unwrap();
    assert!(fixture.provider.stat(&native.id).is_err());
    mount.shutdown();
}
