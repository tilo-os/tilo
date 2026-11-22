use cloud_source::{CloudSource, Credential, ItemId, StaticCredential, UploadTarget};
use cloud_source_gdrive::{DriveConfig, GDrive, Url};
use std::{io::Read, path::Path, sync::Arc, time::Duration};
use tilo_fs::cloud::volume::{DEFAULT_IDLE_POLL_MAX, ROOT_INO};
use tilo_fs::cloud::{CloudVolume, VolumeConfig};

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

fn cursor(dir: &Path) -> serde_json::Value {
    let tree: serde_json::Value =
        serde_json::from_slice(&std::fs::read(dir.join("tree.json")).unwrap()).unwrap();
    let cursor = tree.get("cursor").expect("persisted sync cursor");
    assert!(!cursor.is_null());
    cursor.clone()
}

fn durable_cached_read(root: &ItemId, dir: &Path, expected: &[u8]) {
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
    assert!(
        mount
            .open_file(native_attr.ino, true, false, false)
            .is_err()
    );
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
