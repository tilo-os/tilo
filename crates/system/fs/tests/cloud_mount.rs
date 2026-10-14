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
use fs::cloud::credential::{MountReply, ToMount};
use fs::cloud::service::dispatch;
use fs::cloud::volume::{DEFAULT_IDLE_POLL_MAX, ROOT_INO};
use fs::cloud::{CloudService, CloudVolume, InProcessLauncher, ServiceConfig, VolumeConfig};
use runtime_fs::cloud::{
    CloudClient, CloudMount, CloudRequest, CloudResponse, CloudSocket, FileReply, FileRequest,
    MountHealth,
};

fn credential() -> Credential {
    Credential::Basic {
        username: USER.into(),
        password: zeroize::Zeroizing::new(PASSWORD.into()),
    }
}

struct Mount {
    volume: Arc<CloudVolume>,
    health: Arc<Mutex<Vec<MountHealth>>>,
}

fn mount_volume(server: &FakeDav, dir: &Path) -> Mount {
    mount_polling(server, dir, Duration::from_secs(30), DEFAULT_IDLE_POLL_MAX)
}

fn mount_polling(
    server: &FakeDav,
    dir: &Path,
    poll_interval: Duration,
    idle_poll_max: Duration,
) -> Mount {
    let source = DavSource::new(server.config(), Arc::new(StaticCredential(credential()))).unwrap();
    let _ = source.probe();
    let health = Arc::new(Mutex::new(Vec::new()));
    let seen = Arc::clone(&health);
    let volume = CloudVolume::open(
        Arc::new(source) as Arc<dyn CloudSource>,
        VolumeConfig {
            dir: dir.to_path_buf(),
            cache_budget: 64 << 20,
            poll_interval,
            idle_poll_max,
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

fn oauth_volume(source: Arc<dyn CloudSource>, dir: &Path) -> Arc<CloudVolume> {
    CloudVolume::open(
        source,
        VolumeConfig {
            dir: dir.to_path_buf(),
            cache_budget: 64 << 20,
            poll_interval: Duration::from_secs(30),
            idle_poll_max: DEFAULT_IDLE_POLL_MAX,
        },
        Box::new(|_| {}),
    )
    .unwrap()
}

fn bearer(token: &str) -> Arc<StaticCredential> {
    Arc::new(StaticCredential(Credential::Bearer {
        token: zeroize::Zeroizing::new(token.to_owned()),
        expires: None,
    }))
}

#[test]
fn dropbox_mount_syncs_remote_edits_and_local_writes() {
    use cloud_source_dropbox::{
        DropboxSource,
        fake::{FakeDropbox, TOKEN},
    };
    let server = FakeDropbox::start();
    server.put("/document.txt", b"initial");
    let dir = tempfile::tempdir().unwrap();
    let source = DropboxSource::new(server.config(""), bearer(TOKEN)).unwrap();
    let volume = oauth_volume(Arc::new(source), dir.path());
    volume.sync_once().unwrap();
    assert_eq!(read_all(&volume, "document.txt"), b"initial");
    server.put("/document.txt", b"remote edit");
    volume.sync_once().unwrap();
    assert_eq!(read_all(&volume, "document.txt"), b"remote edit");
    write_file(&volume, "document.txt", b"local edit");
    volume.upload_pending().unwrap();
    assert_eq!(server.get("/document.txt").unwrap(), b"local edit");
    volume.sync_once().unwrap();
    assert_eq!(read_all(&volume, "document.txt"), b"local edit");
}

#[test]
fn drive_mount_syncs_remote_edits_and_local_writes() {
    use cloud_source_gdrive::{
        GDrive,
        fake::{FakeDrive, SOURCE_TOKEN},
    };
    let server = FakeDrive::start();
    let id = server.put_file(&server.root_id(), "document.txt", b"initial");
    server.put_native(
        &server.root_id(),
        "Essay",
        "application/vnd.google-apps.document",
    );
    let dir = tempfile::tempdir().unwrap();
    let source = GDrive::new(server.config(), bearer(SOURCE_TOKEN)).unwrap();
    let volume = oauth_volume(Arc::new(source), dir.path());
    volume.sync_once().unwrap();
    assert_eq!(read_all(&volume, "document.txt"), b"initial");
    server.write_file(&id, b"remote edit");
    volume.sync_once().unwrap();
    assert_eq!(read_all(&volume, "document.txt"), b"remote edit");
    write_file(&volume, "document.txt", b"local edit");
    volume.upload_pending().unwrap();
    assert_eq!(server.bytes(&id).unwrap(), b"local edit");
    volume.sync_once().unwrap();
    assert_eq!(read_all(&volume, "document.txt"), b"local edit");
    let link_name = names(&volume, "")
        .into_iter()
        .find(|name| name.starts_with("Essay."))
        .unwrap();
    let link: serde_json::Value = serde_json::from_slice(&read_all(&volume, &link_name)).unwrap();
    assert_eq!(link["mime"], "application/vnd.google-apps.document");
    assert!(link["url"].as_str().unwrap().starts_with("https://"));
    assert!(
        link["exports"]
            .as_array()
            .unwrap()
            .iter()
            .any(|ext| ext == "docx")
    );
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
    assert_eq!(
        mount.health.lock().unwrap().last(),
        Some(&MountHealth::Healthy)
    );

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
    assert_eq!(
        mount.health.lock().unwrap().last(),
        Some(&MountHealth::Offline)
    );
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

/// The identity of the `tree.json` on disk: every write replaces it with a
/// new file (temp + rename), so an unchanged inode means it was not written.
fn tree_json(dir: &Path) -> Option<u64> {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata(dir.join("tree.json"))
        .ok()
        .map(|m| m.ino())
}

#[test]
fn idle_sync_does_not_rewrite_tree_json() {
    // A walking provider (generic WebDAV) and a cursor feed (sync-collection).
    for server in [FakeDav::generic(), FakeDav::generic_with_sync_collection()] {
        server.put("Notes/plan.md", b"# plan");
        server.put("readme.txt", b"hello");
        let dir = tempfile::tempdir().unwrap();
        let mount = mount_volume(&server, dir.path());
        mount.volume.sync_once().unwrap();
        let written = tree_json(dir.path()).expect("the first sync writes tree.json");

        mount.volume.sync_once().unwrap();
        mount.volume.sync_once().unwrap();
        assert_eq!(
            tree_json(dir.path()),
            Some(written),
            "a pass that changed nothing rewrote tree.json"
        );

        server.put("Notes/todo.md", b"- ship");
        mount.volume.sync_once().unwrap();
        assert_ne!(
            tree_json(dir.path()),
            Some(written),
            "a remote change was not persisted"
        );
        let saved = std::fs::read_to_string(dir.path().join("tree.json")).unwrap();
        assert!(saved.contains("todo.md"), "{saved}");

        // What was persisted is what a restart lists, without the server.
        mount.volume.shutdown();
        drop(mount);
        server.set_offline(true);
        let mount = mount_volume(&server, dir.path());
        assert_eq!(names(&mount.volume, "Notes"), ["plan.md", "todo.md"]);
    }
}

#[test]
fn failed_sync_does_not_rewrite_tree_json() {
    let server = FakeDav::generic();
    server.put("readme.txt", b"hello");
    let dir = tempfile::tempdir().unwrap();
    let mount = mount_volume(&server, dir.path());

    server.set_offline(true);
    assert!(mount.volume.sync_once().is_err());
    assert_eq!(
        tree_json(dir.path()),
        None,
        "a failed first sync wrote tree.json"
    );

    server.set_offline(false);
    mount.volume.sync_once().unwrap();
    let written = tree_json(dir.path()).unwrap();
    server.set_offline(true);
    assert!(mount.volume.sync_once().is_err());
    assert_eq!(
        mount.health.lock().unwrap().last(),
        Some(&MountHealth::Offline)
    );
    assert_eq!(tree_json(dir.path()), Some(written));
}

#[test]
fn idle_polls_back_off_and_reset_on_activity() {
    const POLL: Duration = Duration::from_millis(10);
    let server = FakeDav::generic();
    server.put("readme.txt", b"hello");
    let dir = tempfile::tempdir().unwrap();
    let mount = mount_polling(&server, dir.path(), POLL, Duration::from_secs(60));
    mount.volume.sync_once().unwrap();
    // What one pass costs, so passes can be counted from the server side.
    let before = server.stats().propfinds;
    mount.volume.sync_once().unwrap();
    let per_pass = server.stats().propfinds.saturating_sub(before);
    assert!(per_pass > 0);
    let base = server.stats().propfinds;
    let passes = || {
        server
            .stats()
            .propfinds
            .saturating_sub(base)
            .checked_div(per_pass)
            .unwrap()
    };
    let wait_for_passes = |count: usize| {
        let waiting = std::time::Instant::now();
        while passes() < count {
            assert!(
                waiting.elapsed() < tilo_test_env::hang_bound(),
                "only {} sync passes ran",
                passes()
            );
            std::thread::sleep(Duration::from_millis(2));
        }
    };

    let started = std::time::Instant::now();
    let workers = mount.volume.spawn_workers();
    // Nothing changes: after pass k the loop waits POLL * 2^k (±10 %), so
    // seven passes take at least 0.9 * (20 + 40 + … + 640) ms. Without the
    // back-off they would take about 70 ms.
    wait_for_passes(7);
    let elapsed = started.elapsed();
    assert!(
        elapsed >= Duration::from_millis(1100),
        "seven idle passes ran in {elapsed:?}: the poll did not back off"
    );

    // The loop now waits at least 0.9 * 1280 ms. Listing the mount brings
    // the next pass back within the poll interval.
    let touched = std::time::Instant::now();
    mount.volume.readdir(ROOT_INO).unwrap();
    wait_for_passes(8);
    let reset = touched.elapsed();
    assert!(
        reset < Duration::from_millis(800),
        "the next pass came {reset:?} after local activity"
    );

    mount.volume.shutdown();
    for worker in workers {
        worker.join().unwrap();
    }
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
        control_socket: None,
        file_socket: None,
    }
}

fn account_store(root: &Path, url: &str, enabled: bool) {
    account_of_type(
        root,
        "webdav",
        serde_json::json!({"server_url": url, "username": USER}),
        enabled,
    );
}

fn account_of_type(root: &Path, type_id: &str, config: serde_json::Value, enabled: bool) {
    let mut store = accounts::AccountStore::load_in(root);
    let mut account = accounts::Account::new("files-1", type_id);
    account.display_name = "Work files".into();
    account.enabled = enabled;
    account.config = config;
    store.upsert(account);
    store.save_in(root).unwrap();
}

/// A service with `server`'s account mounted in this process, its volume
/// synced once.
struct Served {
    service: CloudService,
    launcher: InProcessLauncher,
    volume: Arc<CloudVolume>,
    config_root: tempfile::TempDir,
    home: tempfile::TempDir,
}

fn serve(server: &FakeDav) -> Served {
    let config_root = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let root = config_root.path();
    let type_id = if server.is_nextcloud() {
        "nextcloud"
    } else {
        "webdav"
    };
    account_of_type(root, type_id, server.account_config(), true);
    publish(root, "files-1", PASSWORD);
    let launcher = InProcessLauncher::default();
    let mut service = CloudService::new(
        service_config(root, home.path()),
        Box::new(launcher.clone()),
    );
    service.reconcile();
    let volume = launcher.volume("files-1").expect("a running volume");
    volume.sync_once().unwrap();
    Served {
        service,
        launcher,
        volume,
        config_root,
        home,
    }
}

fn no_reply() -> Arc<dyn Fn(MountReply) + Send + Sync> {
    Arc::new(|_| {})
}

/// Wait until `done` holds, failing after the hang bound.
fn wait_until(what: &str, done: impl Fn() -> bool) {
    let started = std::time::Instant::now();
    while !done() {
        assert!(
            started.elapsed() < tilo_test_env::hang_bound(),
            "timed out waiting for {what}"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn sync_now_cuts_backoff_and_syncs_immediately() {
    let server = FakeDav::generic();
    server.put("readme.txt", b"hello");
    let dir = tempfile::tempdir().unwrap();
    // A 30 s poll: after one failure the loop waits 60 s, after two 120 s.
    let mount = mount_volume(&server, dir.path());
    server.set_offline(true);
    let workers = mount.volume.spawn_workers();
    wait_until("the first failed pass", || {
        mount.health.lock().unwrap().contains(&MountHealth::Offline)
    });
    // A second failure, from a sync asked for while still offline: the
    // loop now backs off 120 s.
    let _ = server.requests();
    dispatch(&mount.volume, ToMount::SyncNow, &no_reply());
    wait_until("the second failed pass", || !server.requests().is_empty());

    server.set_offline(false);
    let asked = std::time::Instant::now();
    dispatch(&mount.volume, ToMount::SyncNow, &no_reply());
    wait_until("a healthy pass", || {
        mount.health.lock().unwrap().last() == Some(&MountHealth::Healthy)
    });
    let took = asked.elapsed();
    assert!(
        took < Duration::from_secs(1),
        "sync now took {took:?}; the back-off was not cut short"
    );
    assert_eq!(names(&mount.volume, ""), ["readme.txt"]);

    mount.volume.shutdown();
    for worker in workers {
        worker.join().unwrap();
    }
}

#[test]
fn pause_stops_uploads_until_resume() {
    let server = FakeDav::generic();
    server.put("readme.txt", b"hello");
    let dir = tempfile::tempdir().unwrap();
    let mount = mount_volume(&server, dir.path());
    mount.volume.sync_once().unwrap();
    let workers = mount.volume.spawn_workers();

    dispatch(&mount.volume, ToMount::Pause, &no_reply());
    assert!(mount.volume.is_paused());
    write_file(&mount.volume, "new.txt", b"written while paused");
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(server.get("new.txt"), None, "a paused mount uploaded");
    assert_eq!(mount.volume.pending(), 1, "the edit is queued, not lost");
    // Reads still work while paused.
    assert_eq!(read_all(&mount.volume, "readme.txt"), b"hello");

    dispatch(&mount.volume, ToMount::Resume, &no_reply());
    assert!(!mount.volume.is_paused());
    wait_until("the queued upload", || server.get("new.txt").is_some());
    assert_eq!(server.get("new.txt").unwrap(), b"written while paused");

    mount.volume.shutdown();
    for worker in workers {
        worker.join().unwrap();
    }
}

#[test]
fn revision_request_resolves_path_and_returns_provider_revisions() {
    let server = FakeDav::nextcloud();
    server.put("Notes/plan.md", b"first draft");
    server.put("Notes/plan.md", b"second draft");
    let mut account = serve(&server);

    let CloudResponse::Mounts(mounts) = account
        .service
        .request(CloudRequest::List, tilo_test_env::hang_bound())
    else {
        panic!("a mount list");
    };
    assert_eq!(mounts.len(), 1);
    assert_eq!(mounts[0].account, "files-1");
    assert_eq!(
        mounts[0].mount_point,
        account.home.path().join("Remote/Work files")
    );
    assert!(mounts[0].capabilities.revisions);
    assert!(!mounts[0].capabilities.share_links);
    assert!(!mounts[0].paused);

    let response = account.service.request(
        CloudRequest::File {
            account: "files-1".into(),
            request: FileRequest::Revisions {
                path: "Notes/plan.md".into(),
            },
        },
        tilo_test_env::hang_bound(),
    );
    let CloudResponse::File(FileReply::Revisions(revisions)) = response else {
        panic!("revisions, not {response:?}");
    };
    assert_eq!(revisions.len(), 1, "{revisions:?}");

    let restored = account.service.request(
        CloudRequest::File {
            account: "files-1".into(),
            request: FileRequest::RestoreRevision {
                path: "Notes/plan.md".into(),
                revision: revisions[0].revision.clone(),
            },
        },
        tilo_test_env::hang_bound(),
    );
    assert_eq!(restored, CloudResponse::File(FileReply::Restored));
    assert_eq!(server.get("Notes/plan.md").unwrap(), b"first draft");
    assert_eq!(read_all(&account.volume, "Notes/plan.md"), b"first draft");

    // Nextcloud has no share links here: the request is refused, not sent.
    let link = account.service.request(
        CloudRequest::File {
            account: "files-1".into(),
            request: FileRequest::ShareLink {
                path: "Notes/plan.md".into(),
            },
        },
        tilo_test_env::hang_bound(),
    );
    assert!(
        matches!(&link, CloudResponse::Error(error) if error.contains("not supported")),
        "{link:?}"
    );
    // A path the mount does not know is refused.
    let missing = account.service.request(
        CloudRequest::File {
            account: "files-1".into(),
            request: FileRequest::Revisions {
                path: "Notes/absent.md".into(),
            },
        },
        tilo_test_env::hang_bound(),
    );
    assert!(matches!(missing, CloudResponse::Error(_)), "{missing:?}");
    account.service.shutdown();
}

#[test]
fn restore_revision_request_is_refused_when_capability_absent() {
    let server = FakeDav::generic();
    server.put("readme.txt", b"hello");
    let mut account = serve(&server);
    let _ = server.requests();
    for request in [
        FileRequest::Revisions {
            path: "readme.txt".into(),
        },
        FileRequest::RestoreRevision {
            path: "readme.txt".into(),
            revision: "1".into(),
        },
    ] {
        let response = account.service.request(
            CloudRequest::File {
                account: "files-1".into(),
                request,
            },
            tilo_test_env::hang_bound(),
        );
        assert!(
            matches!(&response, CloudResponse::Error(error) if error.contains("not supported")),
            "{response:?}"
        );
    }
    assert!(
        server.requests().is_empty(),
        "a refused request reached the server"
    );
    assert_eq!(server.get("readme.txt").unwrap(), b"hello");
    let CloudResponse::Mounts(mounts) = account
        .service
        .request(CloudRequest::List, tilo_test_env::hang_bound())
    else {
        panic!("a mount list");
    };
    assert!(!mounts[0].capabilities.revisions);
    account.service.shutdown();
}

#[test]
fn pause_is_remembered_and_sync_now_reaches_the_mount() {
    let server = FakeDav::generic();
    server.put("readme.txt", b"hello");
    let mut account = serve(&server);
    let paused = account.service.request(
        CloudRequest::Pause {
            account: "files-1".into(),
        },
        tilo_test_env::hang_bound(),
    );
    assert_eq!(paused, CloudResponse::Done);
    assert!(account.volume.is_paused());
    let unknown = account.service.request(
        CloudRequest::SyncNow {
            account: "nobody".into(),
        },
        tilo_test_env::hang_bound(),
    );
    assert!(matches!(unknown, CloudResponse::Error(_)));

    // A restarted service pauses the account's new mount again.
    account.service.shutdown();
    let mut service = CloudService::new(
        service_config(account.config_root.path(), account.home.path()),
        Box::new(account.launcher.clone()),
    );
    service.reconcile();
    let volume = account.launcher.volume("files-1").unwrap();
    assert!(volume.is_paused());
    let CloudResponse::Mounts(mounts) =
        service.request(CloudRequest::List, tilo_test_env::hang_bound())
    else {
        panic!("a mount list");
    };
    assert!(mounts[0].paused);

    let resumed = service.request(
        CloudRequest::Resume {
            account: "files-1".into(),
        },
        tilo_test_env::hang_bound(),
    );
    assert_eq!(resumed, CloudResponse::Done);
    assert!(!volume.is_paused());
    assert_eq!(
        service.request(
            CloudRequest::SyncNow {
                account: "files-1".into(),
            },
            tilo_test_env::hang_bound(),
        ),
        CloudResponse::Done
    );
    service.shutdown();
}

#[test]
fn the_control_socket_lists_mounts_without_credentials() {
    let server = FakeDav::nextcloud();
    server.put("a.txt", b"a");
    let mut account = serve(&server);
    let runtime = tempfile::tempdir().unwrap();
    let socket = runtime_fs::cloud::socket_in(runtime.path());
    let files_socket = runtime_fs::cloud::files_socket_in(runtime.path());
    account
        .service
        .serve_control(&socket, CloudSocket::Accounts)
        .unwrap();
    account
        .service
        .serve_control(&files_socket, CloudSocket::Files)
        .unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        for path in [&socket, &files_socket] {
            let mode = std::fs::metadata(path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }
    let client = std::thread::spawn(move || {
        let mut client = CloudClient::connect(&socket).unwrap();
        let mounts = client.mounts().unwrap();
        let raw = client.request(&CloudRequest::List).unwrap();
        client
            .control(&CloudRequest::SyncNow {
                account: "files-1".into(),
            })
            .unwrap();
        // The account socket, which Settings' sandbox is granted, never
        // reads, restores or shares a file.
        let refused = [
            FileRequest::Revisions {
                path: "a.txt".into(),
            },
            FileRequest::RestoreRevision {
                path: "a.txt".into(),
                revision: "1".into(),
            },
            FileRequest::ShareLink {
                path: "a.txt".into(),
            },
        ]
        .map(|request| match client.file("files-1", request) {
            Err(error) => error.to_string(),
            Ok(reply) => format!("served: {reply:?}"),
        });
        // The file socket answers them.
        let revisions = CloudClient::connect(&files_socket).unwrap().file(
            "files-1",
            FileRequest::Revisions {
                path: "a.txt".into(),
            },
        );
        (mounts, raw, refused, revisions)
    });
    let started = std::time::Instant::now();
    while !client.is_finished() {
        assert!(
            started.elapsed() < tilo_test_env::hang_bound(),
            "the control client did not finish"
        );
        account.service.pump();
        std::thread::sleep(Duration::from_millis(5));
    }
    let (mounts, raw, refused, revisions) = client.join().unwrap();
    let mounts: Vec<CloudMount> = mounts;
    for answer in &refused {
        assert_eq!(
            answer, "this socket does not serve that request",
            "the account socket served a file request"
        );
    }
    assert!(
        matches!(revisions, Ok(FileReply::Revisions(_))),
        "the file socket answers revisions: {revisions:?}"
    );
    assert_eq!(mounts.len(), 1);
    assert_eq!(mounts[0].account, "files-1");
    assert_eq!(mounts[0].health, Some(MountHealth::Healthy));
    let bytes = postcard::to_stdvec(&raw).unwrap();
    let json = serde_json::to_string(&raw).unwrap();
    let secret = PASSWORD.as_bytes();
    assert!(
        !bytes.windows(secret.len()).any(|window| window == secret),
        "the mount list carries a credential"
    );
    assert!(!json.contains(PASSWORD));
    assert!(!json.to_lowercase().contains("password"));
    assert!(!json.to_lowercase().contains("token"));
    account.service.shutdown();
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
    server.put("Docs/hello.txt", b"first draft");
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
    assert!(service.wait_for_health("cloud-1", MountHealth::Healthy, tilo_test_env::hang_bound()));
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

    // Control reaches the mount process down its stdin, and its answers
    // come back on stdout.
    let CloudResponse::Mounts(mounts) =
        service.request(CloudRequest::List, tilo_test_env::hang_bound())
    else {
        panic!("a mount list");
    };
    assert!(mounts[0].capabilities.revisions);
    let response = service.request(
        CloudRequest::File {
            account: "cloud-1".into(),
            request: FileRequest::Revisions {
                path: "Docs/hello.txt".into(),
            },
        },
        tilo_test_env::hang_bound(),
    );
    let CloudResponse::File(FileReply::Revisions(revisions)) = response else {
        panic!("revisions, not {response:?}");
    };
    assert_eq!(revisions.len(), 1);
    assert_eq!(
        service.request(
            CloudRequest::Pause {
                account: "cloud-1".into()
            },
            tilo_test_env::hang_bound()
        ),
        CloudResponse::Done
    );

    service.shutdown();
    assert!(!mount.exists());
}
