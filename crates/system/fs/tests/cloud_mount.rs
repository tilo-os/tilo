//! Cloud accounts as mounted folders, end to end against an in-process
//! WebDAV/Nextcloud server (`cloud_source_dav::fake::FakeDav` on the shared
//! test responder).
//!
//! The volume tests drive the same inode operations the FUSE adapter calls;
//! the service tests run the account service over an explicit config root
//! (never the real user config); the last test mounts through the kernel
//! when FUSE is available.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test assertions"
)]

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use cloud_source::{CloudSource, Credential, StaticCredential};
use cloud_source_dav::DavSource;
use cloud_source_dav::fake::{FakeDav, PASSWORD, USER};
use fs::cloud::volume::{Health, ROOT_INO};
use fs::cloud::{CloudService, CloudVolume, InProcessLauncher, ServiceConfig, VolumeConfig};

fn credential() -> Credential {
    Credential::Basic {
        username: USER.into(),
        password: zeroize::Zeroizing::new(PASSWORD.into()),
    }
}

struct Mount {
    volume: Arc<CloudVolume>,
    health: Arc<Mutex<Vec<Health>>>,
}

fn mount_volume(server: &FakeDav, dir: &Path) -> Mount {
    let source = DavSource::new(server.config(), Arc::new(StaticCredential(credential()))).unwrap();
    let _ = source.probe();
    let health = Arc::new(Mutex::new(Vec::new()));
    let seen = Arc::clone(&health);
    let volume = CloudVolume::open(
        Arc::new(source) as Arc<dyn CloudSource>,
        VolumeConfig {
            dir: dir.to_path_buf(),
            cache_budget: 64 << 20,
            poll_interval: Duration::from_secs(30),
        },
        Box::new(move |h| seen.lock().unwrap().push(h)),
    )
    .unwrap();
    Mount { volume, health }
}

fn names(volume: &CloudVolume, path: &str) -> Vec<String> {
    let ino = volume.resolve(path).unwrap();
    volume
        .readdir(ino)
        .unwrap()
        .into_iter()
        .map(|(name, _)| name)
        .collect()
}

fn read_all(volume: &CloudVolume, path: &str) -> Vec<u8> {
    let ino = volume.resolve(path).unwrap();
    let size = volume.getattr(ino).unwrap().size;
    volume
        .read_ino(ino, 0, u32::try_from(size).unwrap())
        .unwrap()
}

fn write_file(volume: &CloudVolume, path: &str, bytes: &[u8]) {
    let (parent, name) = path.rsplit_once('/').unwrap_or(("", path));
    let parent_ino = volume.resolve(parent).unwrap();
    let fh = match volume.lookup(parent_ino, name) {
        Ok(attr) => volume.open_file(attr.ino, true, true).unwrap(),
        Err(_) => volume.create(parent_ino, name).unwrap().1,
    };
    volume.write(fh, 0, bytes).unwrap();
    volume.release(fh).unwrap();
}

fn pattern(len: usize) -> Vec<u8> {
    (0..len).map(|i| u8::try_from(i % 251).unwrap()).collect()
}

#[test]
fn listing_mirrors_the_server_tree() {
    let server = FakeDav::nextcloud();
    server.put("Notes/plan.md", b"# plan");
    server.put("Photos/2024/beach.jpg", b"jpeg");
    server.put("readme.txt", b"hello");
    let dir = tempfile::tempdir().unwrap();
    let mount = mount_volume(&server, dir.path());
    mount.volume.sync_once().unwrap();
    assert_eq!(names(&mount.volume, ""), ["Notes", "Photos", "readme.txt"]);
    assert_eq!(names(&mount.volume, "Photos/2024"), ["beach.jpg"]);
    let attr = mount
        .volume
        .getattr(mount.volume.resolve("readme.txt").unwrap())
        .unwrap();
    assert_eq!(attr.size, 5);
    assert_eq!(mount.health.lock().unwrap().last(), Some(&Health::Healthy));

    // Remote changes show up on the next sync; unchanged subtrees are not
    // listed again (Nextcloud folder ETags are recursive).
    server.put("Notes/todo.md", b"- ship");
    server.remove("readme.txt");
    let _ = server.requests();
    mount.volume.sync_once().unwrap();
    assert_eq!(names(&mount.volume, ""), ["Notes", "Photos"]);
    assert_eq!(names(&mount.volume, "Notes"), ["plan.md", "todo.md"]);
    let walked: Vec<String> = server
        .requests()
        .into_iter()
        .filter(|r| r.method == "PROPFIND")
        .map(|r| r.target)
        .collect();
    assert!(
        !walked.iter().any(|t| t.contains("/Photos/")),
        "the unchanged Photos subtree was walked again: {walked:?}"
    );
}

#[test]
fn cold_and_ranged_reads_hydrate_only_what_is_read() {
    let server = FakeDav::generic();
    let big = pattern(8 << 20);
    server.put("video.bin", &big);
    server.put("small.txt", b"tiny");
    let dir = tempfile::tempdir().unwrap();
    let mount = mount_volume(&server, dir.path());
    mount.volume.sync_once().unwrap();
    assert_eq!(
        server.stats().bytes_served,
        0,
        "listing never reads content"
    );

    assert_eq!(read_all(&mount.volume, "small.txt"), b"tiny");

    let ino = mount.volume.resolve("video.bin").unwrap();
    let offset = 5 << 20;
    let chunk = mount.volume.read_ino(ino, offset, 4096).unwrap();
    assert_eq!(chunk, &big[5 << 20..(5 << 20) + 4096]);
    let stats = server.stats();
    assert!(stats.ranged_gets >= 1);
    assert!(
        stats.bytes_served <= (1 << 20) + 4,
        "a 4 KiB read fetched {} bytes",
        stats.bytes_served
    );

    // Cached ranges are served locally.
    let fetched_before = server.stats().bytes_served;
    assert_eq!(
        mount.volume.read_ino(ino, offset + 100, 100).unwrap(),
        &big[(5 << 20) + 100..(5 << 20) + 200]
    );
    assert_eq!(server.stats().bytes_served, fetched_before);

    // A sequential reader through a handle gets the whole file right.
    let fh = mount.volume.open_file(ino, false, false).unwrap();
    let mut got = Vec::new();
    while got.len() < big.len() {
        let part = mount.volume.read(fh, got.len() as u64, 128 * 1024).unwrap();
        assert!(!part.is_empty());
        got.extend_from_slice(&part);
    }
    mount.volume.release(fh).unwrap();
    assert_eq!(got, big);
}

#[test]
fn local_writes_upload_with_revision_preconditions() {
    let server = FakeDav::nextcloud();
    server.put("doc.txt", b"version one");
    let dir = tempfile::tempdir().unwrap();
    let mount = mount_volume(&server, dir.path());
    mount.volume.sync_once().unwrap();
    let _ = server.requests();

    write_file(&mount.volume, "doc.txt", b"version two, edited here");
    assert_eq!(mount.volume.pending(), 1);
    assert_eq!(
        read_all(&mount.volume, "doc.txt"),
        b"version two, edited here",
        "reads see local writes before they upload"
    );
    mount.volume.upload_pending().unwrap();
    assert_eq!(mount.volume.pending(), 0);
    assert_eq!(server.get("doc.txt").unwrap(), b"version two, edited here");
    let put = server
        .requests()
        .into_iter()
        .find(|r| r.method == "PUT")
        .expect("an upload");
    assert!(put.header("if-match").is_some(), "uploads are conditional");

    // Creating, making folders, renaming and deleting all write back.
    let root = ROOT_INO;
    mount.volume.mkdir(root, "Reports").unwrap();
    write_file(&mount.volume, "Reports/q3.txt", b"numbers");
    let big = pattern(200 * 1024);
    write_file(&mount.volume, "Reports/big.bin", &big);
    let reports = mount.volume.resolve("Reports").unwrap();
    mount
        .volume
        .rename(reports, "q3.txt", reports, "q3-final.txt")
        .unwrap();
    mount.volume.unlink(root, "doc.txt").unwrap();
    mount.volume.upload_pending().unwrap();
    assert_eq!(server.get("Reports/q3-final.txt").unwrap(), b"numbers");
    assert_eq!(
        server.get("Reports/big.bin").unwrap(),
        big,
        "chunked upload"
    );
    assert!(server.get("doc.txt").is_none());
    assert!(server.get("Reports/q3.txt").is_none());

    // The server's view and the local one agree after a sync.
    mount.volume.sync_once().unwrap();
    assert_eq!(names(&mount.volume, "Reports"), ["big.bin", "q3-final.txt"]);
    assert_eq!(read_all(&mount.volume, "Reports/big.bin"), big);
}

#[test]
fn an_editor_save_by_rename_replaces_the_file_in_place() {
    let server = FakeDav::generic();
    server.put("essay.md", b"draft");
    let dir = tempfile::tempdir().unwrap();
    let mount = mount_volume(&server, dir.path());
    mount.volume.sync_once().unwrap();
    write_file(&mount.volume, ".essay.md.swp", b"final text");
    mount
        .volume
        .rename(ROOT_INO, ".essay.md.swp", ROOT_INO, "essay.md")
        .unwrap();
    mount.volume.upload_pending().unwrap();
    assert_eq!(server.get("essay.md").unwrap(), b"final text");
    assert_eq!(
        server.paths(),
        ["essay.md"],
        "no temp file or conflict copy"
    );
}

#[test]
fn a_remote_edit_during_a_local_edit_keeps_both_versions() {
    let server = FakeDav::nextcloud();
    server.put("Shared/budget.csv", b"a,b\n1,2\n");
    let dir = tempfile::tempdir().unwrap();
    let mount = mount_volume(&server, dir.path());
    mount.volume.sync_once().unwrap();

    write_file(&mount.volume, "Shared/budget.csv", b"a,b\n1,3\n");
    server.put("Shared/budget.csv", b"a,b\n9,9\n");
    mount.volume.upload_pending().unwrap();

    let paths = server.paths();
    let copy = paths
        .iter()
        .find(|p| p.starts_with("Shared/budget.csv.sync-conflict-"))
        .unwrap_or_else(|| panic!("no conflict copy in {paths:?}"));
    assert_eq!(server.get("Shared/budget.csv").unwrap(), b"a,b\n9,9\n");
    assert_eq!(server.get(copy).unwrap(), b"a,b\n1,3\n");
    let suffix = copy.rsplit('-').next().unwrap();
    assert_eq!(suffix.len(), 32, "the storage daemon's conflict-copy name");

    mount.volume.sync_once().unwrap();
    let listed = names(&mount.volume, "Shared");
    assert_eq!(listed.len(), 2, "{listed:?}");
    assert_eq!(read_all(&mount.volume, "Shared/budget.csv"), b"a,b\n9,9\n");
}

#[test]
fn offline_reads_come_from_cache_and_writes_queue_across_restarts() {
    let server = FakeDav::nextcloud();
    server.put("cached.txt", b"read before going offline");
    server.put("cold.txt", b"never read");
    let dir = tempfile::tempdir().unwrap();
    {
        let mount = mount_volume(&server, dir.path());
        mount.volume.sync_once().unwrap();
        assert_eq!(
            read_all(&mount.volume, "cached.txt"),
            b"read before going offline"
        );
        mount.volume.shutdown();
    }

    server.set_offline(true);
    let mount = mount_volume(&server, dir.path());
    // Metadata and cached bytes survive the restart without the server.
    assert!(mount.volume.sync_once().is_err());
    assert_eq!(mount.health.lock().unwrap().last(), Some(&Health::Offline));
    assert_eq!(names(&mount.volume, ""), ["cached.txt", "cold.txt"]);
    assert_eq!(
        read_all(&mount.volume, "cached.txt"),
        b"read before going offline"
    );
    let cold = mount.volume.resolve("cold.txt").unwrap();
    assert_eq!(mount.volume.read_ino(cold, 0, 10), Err(libc::EIO));

    write_file(&mount.volume, "offline-note.txt", b"written offline");
    assert!(mount.volume.upload_pending().is_err());
    assert_eq!(mount.volume.pending(), 1);
    mount.volume.shutdown();
    drop(mount);

    // Queued writes survive a restart and go out once the server is back.
    server.set_offline(false);
    let mount = mount_volume(&server, dir.path());
    assert_eq!(mount.volume.pending(), 1);
    assert_eq!(
        read_all(&mount.volume, "offline-note.txt"),
        b"written offline"
    );
    mount.volume.upload_pending().unwrap();
    assert_eq!(server.get("offline-note.txt").unwrap(), b"written offline");
}

/// Settings publishing the account's handoff credential, the way custody
/// does after a sign-in or a password change.
fn publish(root: &Path, id: &str, password: &str) {
    accounts::HandoffStore::open_in(root)
        .publish(
            &accounts::AccountId::from(id),
            &accounts::HandoffCredential::Basic {
                username: USER.into(),
                password: zeroize::Zeroizing::new(password.into()),
            },
        )
        .unwrap();
}

/// Stands in for Settings' credential helper: whatever Settings published
/// is current; without an entry the account needs sign-in.
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

fn service_config(root: &Path, home: &Path) -> ServiceConfig {
    ServiceConfig {
        config_root: Some(root.to_path_buf()),
        mounts_root: home.join("Remote"),
        state_root: home.join("state"),
        refresher: std::sync::Arc::new(Published(accounts::HandoffStore::open_in(root))),
    }
}

fn account_store(root: &Path, url: &str, enabled: bool) {
    let mut store = accounts::AccountStore::load_in(root);
    let mut account = accounts::Account::new("files-1", "webdav");
    account.display_name = "Work files".into();
    account.enabled = enabled;
    account.config = serde_json::json!({"server_url": url, "username": USER});
    store.upsert(account);
    store.save_in(root).unwrap();
}

#[test]
fn accounts_mount_and_unmount_with_the_account_store() {
    let server = FakeDav::generic();
    server.put("hello.txt", b"from the server");
    let config_root = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let root = config_root.path();
    account_store(root, server.root_url().as_str(), true);
    publish(root, "files-1", PASSWORD);

    let launcher = InProcessLauncher::default();
    let mut service = CloudService::new(
        service_config(root, home.path()),
        Box::new(launcher.clone()),
    );
    service.reconcile();
    let mounted = service.mounted();
    assert_eq!(mounted.len(), 1);
    assert_eq!(mounted[0].1, home.path().join("Remote/Work files"));
    assert!(mounted[0].1.is_dir());
    let volume = launcher.volume("files-1").expect("a running volume");
    volume.sync_once().unwrap();
    assert_eq!(read_all(&volume, "hello.txt"), b"from the server");
    service.pump();
    let status = accounts::AccountSyncStatusStore::load_in(root);
    assert_eq!(
        status
            .get(&accounts::AccountId::from("files-1"))
            .map(|s| s.health),
        Some(accounts::AccountSyncHealth::Healthy),
        "mount health reaches the status store Settings and Files read"
    );

    // Disabling unmounts but keeps the account's cache.
    account_store(root, server.root_url().as_str(), false);
    service.reconcile();
    assert!(service.mounted().is_empty());
    assert!(!home.path().join("Remote/Work files").exists());
    assert!(launcher.volume("files-1").is_none());
    assert!(
        home.path()
            .join("state")
            .read_dir()
            .unwrap()
            .next()
            .is_some()
    );

    // Removing the account drops its state too.
    account_store(root, server.root_url().as_str(), true);
    service.reconcile();
    assert_eq!(service.mounted().len(), 1);
    let mut store = accounts::AccountStore::load_in(root);
    store.remove(&accounts::AccountId::from("files-1"));
    store.save_in(root).unwrap();
    service.reconcile();
    assert!(service.mounted().is_empty());
    assert!(
        home.path()
            .join("state")
            .read_dir()
            .unwrap()
            .next()
            .is_none()
    );
    assert!(!home.path().join("Remote/Work files").exists());
}

#[test]
fn a_rejected_password_asks_the_service_for_a_fresh_one() {
    let server = FakeDav::generic();
    server.put("a.txt", b"a");
    let config_root = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let root = config_root.path();
    account_store(root, server.root_url().as_str(), true);
    publish(root, "files-1", "stale password");

    let launcher = InProcessLauncher::default();
    let mut service = CloudService::new(
        service_config(root, home.path()),
        Box::new(launcher.clone()),
    );
    service.reconcile();
    let volume = launcher.volume("files-1").unwrap();
    assert!(volume.sync_once().is_err());
    // Settings publishes the new password; the service hands it over.
    publish(root, "files-1", PASSWORD);
    service.pump();
    volume.sync_once().unwrap();
    assert_eq!(read_all(&volume, "a.txt"), b"a");
}

fn fuse_available() -> bool {
    Path::new("/dev/fuse").exists()
}

#[test]
fn posix_tools_see_the_account_through_the_kernel_mount() {
    tilo_test_env::require_capability!(
        Fuse,
        fuse_available(),
        "requires /dev/fuse and fusermount3"
    );
    let server = FakeDav::nextcloud();
    server.put("Docs/hello.txt", b"hello from the cloud");
    let config_root = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let root = config_root.path();
    let mut store = accounts::AccountStore::new();
    let mut account = accounts::Account::new("cloud-1", "nextcloud");
    account.display_name = "Cloud".into();
    account.config =
        serde_json::json!({"server_url": server.server_url().as_str(), "username": USER});
    store.upsert(account);
    store.save_in(root).unwrap();
    publish(root, "cloud-1", PASSWORD);

    let mut service = CloudService::new(
        service_config(root, home.path()),
        Box::new(fs::cloud::ProcessLauncher {
            executable: env!("CARGO_BIN_EXE_fs").into(),
            ready_timeout: Duration::from_secs(30),
        }),
    );
    service.reconcile();
    let mount = home.path().join("Remote/Cloud");
    assert_eq!(service.mounted(), [("cloud-1".to_owned(), mount.clone())]);

    // The first sync runs in the mount's worker; it reports health when done.
    assert!(service.wait_for_health("cloud-1", Health::Healthy, tilo_test_env::hang_bound()));
    assert_eq!(
        std::fs::read_to_string(mount.join("Docs/hello.txt")).unwrap(),
        "hello from the cloud"
    );
    std::fs::write(mount.join("Docs/new.txt"), b"written by a POSIX tool").unwrap();
    server.wait_for(|r| r.method == "PUT" && r.target.ends_with("/Docs/new.txt"));
    assert_eq!(
        server.get("Docs/new.txt").unwrap(),
        b"written by a POSIX tool"
    );

    service.shutdown();
    assert!(!mount.exists());
}
