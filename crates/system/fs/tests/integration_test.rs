#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::sync::{Arc, Mutex};
use std::time::Duration;
use std::{
    io::{Read, Seek, SeekFrom, Write},
    os::unix::ffi::OsStringExt,
    os::unix::fs::{MetadataExt, PermissionsExt as _},
};
use tilo_test_env::SpawnUnderEnvLock as _;

use collab_types::{Access as CapAccess, CapOverride, Ed25519PublicKey, Effect, Scope, Subject};
use fs::namespace::ROOT_INO;
use fs::{Access, ContentStore, Heuristics, Hydrator, Namespace, QuickFS, WorkspaceAcl};
use fuser::{Config, MountOption};
use iroh::endpoint::{Endpoint, presets};
use tempfile::tempdir;

fn fuse_available() -> bool {
    std::path::Path::new("/dev/fuse").exists()
}
macro_rules! require_fuse {
    () => {
        tilo_test_env::require_capability!(
            Fuse,
            fuse_available(),
            "requires /dev/fuse and fusermount3"
        );
    };
}

async fn wait_for_mount(mount: impl AsRef<std::path::Path>) {
    let mount = mount.as_ref().to_path_buf();
    tokio::task::spawn_blocking(move || {
        let parent = mount.parent().expect("a mount point has a parent");
        let outside = std::fs::metadata(parent).expect("stat mount parent").dev();
        let deadline = tilo_test_env::hang_deadline();
        while !std::fs::metadata(&mount).is_ok_and(|meta| meta.dev() != outside) {
            assert!(
                std::time::Instant::now() < deadline,
                "FUSE mount at {} did not start answering",
                mount.display()
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    })
    .await
    .expect("mount readiness check");
}

async fn shutdown_fuse_fixture(
    session: fuser::BackgroundSession,
    mount: &std::path::Path,
    endpoint: Endpoint,
    store: Arc<ContentStore>,
) {
    fs::fuse_session::unmount_and_join(session, mount).expect("unmount and join FUSE session");
    endpoint.close().await;
    store
        .store()
        .shutdown()
        .await
        .expect("shut down blob store");
}

struct AbortErrorFs;

impl fuser::Filesystem for AbortErrorFs {
    fn init(
        &mut self,
        _req: &fuser::Request,
        config: &mut fuser::KernelConfig,
    ) -> std::io::Result<()> {
        config
            .add_capabilities(fuser::InitFlags::FUSE_ABORT_ERROR)
            .map_err(|_| std::io::Error::other("the kernel does not offer FUSE_ABORT_ERROR"))
    }
}

fn fusectl_abort(mount: &std::path::Path) -> Option<std::path::PathBuf> {
    let mountinfo = std::fs::read_to_string("/proc/self/mountinfo").ok()?;
    let device = mountinfo.lines().rev().find_map(|line| {
        let mut fields = line.split(' ');
        let device = fields.nth(2)?;
        (std::path::Path::new(fields.nth(1)?) == mount).then_some(device)
    })?;
    let (major, minor) = device.split_once(':')?;
    let (major, minor): (u64, u64) = (major.parse().ok()?, minor.parse().ok()?);
    let abort = std::path::PathBuf::from(format!(
        "/sys/fs/fuse/connections/{}/abort",
        (major << 20) | minor
    ));
    abort.exists().then_some(abort)
}

#[test]
fn teardown_reads_an_aborted_fuse_connection_as_a_clean_end() {
    require_fuse!();
    let mount_dir = tempdir().expect("mount temp dir");
    let mount = mount_dir
        .path()
        .canonicalize()
        .expect("canonical mount path");
    let mut config = Config::default();
    config.mount_options = vec![MountOption::FSName("quickfs_abort_error".into())];
    let session = fuser::spawn_mount(AbortErrorFs, &mount, &config).expect("spawn FUSE session");
    let abort = fusectl_abort(&mount);
    if let Some(abort) = &abort {
        std::fs::write(abort, b"1").expect("abort the FUSE connection");
    }
    let ended = fs::fuse_session::unmount_and_join(session, &mount);
    let _ = std::process::Command::new("fusermount3")
        .args(["-u", "-q", "-z", "--"])
        .arg(&mount)
        .status_locked();
    tilo_test_env::require_capability!(
        FuseControl,
        abort.is_some(),
        "aborting the connection needs fusectl mounted at /sys/fs/fuse/connections"
    );
    ended.expect("an aborted connection ends the session cleanly");
}

struct IdleFs;

impl fuser::Filesystem for IdleFs {}

fn mounted(mount: &std::path::Path) -> bool {
    std::fs::read_to_string("/proc/self/mountinfo")
        .expect("read mountinfo")
        .lines()
        .any(|line| line.split(' ').nth(4).map(std::path::Path::new) == Some(mount))
}

#[test]
fn stopping_a_held_auto_unmount_session_ends_it() {
    use std::os::unix::fs::OpenOptionsExt as _;
    require_fuse!();
    tilo_test_env::require_capability!(
        Fuse,
        rustix::process::getuid().is_root(),
        "an auto-unmount mount needs root"
    );
    let mount_dir = tempdir().expect("mount temp dir");
    let mount = mount_dir
        .path()
        .canonicalize()
        .expect("canonical mount path");
    let mut config = Config::default();
    config.mount_options = vec![
        MountOption::FSName("quickfs_held".into()),
        MountOption::AutoUnmount,
    ];
    config.acl = fuser::SessionACL::All;
    let session = fuser::spawn_mount(IdleFs, &mount, &config).expect("spawn FUSE session");
    assert!(mounted(&mount));
    let holder = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_PATH)
        .open(&mount)
        .expect("hold the mount");
    let (done, ended) = std::sync::mpsc::channel();
    let stop_at = mount.clone();
    std::thread::spawn(move || {
        let _ = done.send(fs::fuse_session::unmount_and_join(session, &stop_at));
    });
    let ended = ended
        .recv_timeout(tilo_test_env::hang_bound())
        .expect("the stopped session never ended");
    match ended {
        Ok(()) => {
            assert!(!mounted(&mount), "the stopped session is still mounted");
            drop(holder);
        }
        Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
            let identity = std::fs::read_to_string("/proc/self/uid_map").is_ok_and(|map| {
                map.split_whitespace().collect::<Vec<_>>() == ["0", "0", "4294967295"]
            });
            assert!(
                !identity,
                "refused to end the connection as real root: {error}"
            );
            drop(holder);
            let _ = std::process::Command::new("fusermount3")
                .args(["-u", "-q", "-z", "--"])
                .arg(&mount)
                .status_locked();
        }
        Err(error) => panic!("stopping the session failed: {error}"),
    }
}

async fn shutdown_runtime_fixture(endpoint: Endpoint, store: Arc<ContentStore>) {
    endpoint.close().await;
    store
        .store()
        .shutdown()
        .await
        .expect("shut down blob store");
}

const ALICE: [u8; 32] = [0xA1; 32];
const BOB: [u8; 32] = [0xB0; 32];

struct CapFixture {
    fs: QuickFS,
    endpoint: Endpoint,
    store: Arc<ContentStore>,
    docs: u64,
    docs_child: u64,
    other_child: u64,
    look_alike_child: u64,
}

impl CapFixture {
    async fn shutdown(self) {
        let Self {
            fs,
            endpoint,
            store,
            ..
        } = self;
        drop(fs);
        shutdown_runtime_fixture(endpoint, store).await;
    }
}

async fn cap_fixture(identity: [u8; 32], caps: &[CapOverride], now: Option<u64>) -> CapFixture {
    cap_fixture_with(
        identity,
        fs::AclHandle::new(WorkspaceAcl::from_capabilities(identity, caps, &[], now)),
    )
    .await
}

async fn cap_fixture_with(identity: [u8; 32], acl: fs::AclHandle) -> CapFixture {
    let endpoint = Endpoint::bind(presets::Minimal)
        .await
        .expect("bind endpoint");
    let store = Arc::new(ContentStore::memory());
    let body = b"contents";
    let hash = store.write_content(body).await.unwrap().unwrap();
    let namespace = Arc::new(Mutex::new(Namespace::new("cap-peer").unwrap()));

    let (docs, docs_child, other_child, look_alike_child) = {
        let mut ns = namespace.lock().unwrap();
        let w = ns.create_dir(ROOT_INO, "w", 1).unwrap();
        let docs = ns.create_dir(w, "docs", 1).unwrap();
        let docs_child = ns
            .create_file(docs, "report.md", Some(&hash), body.len() as u64, 1)
            .unwrap();
        let other = ns.create_dir(w, "other", 1).unwrap();
        let other_child = ns
            .create_file(other, "private.md", Some(&hash), body.len() as u64, 1)
            .unwrap();
        let look_alike = ns.create_dir(w, "docsSECRET", 1).unwrap();
        let look_alike_child = ns
            .create_file(look_alike, "leak.md", Some(&hash), body.len() as u64, 1)
            .unwrap();
        drop(ns);
        (docs, docs_child, other_child, look_alike_child)
    };

    let shutdown_endpoint = endpoint.clone();
    let shutdown_store = store.clone();
    let hydrator = Arc::new(Hydrator::new(endpoint, store));
    let heuristics = Arc::new(Heuristics::new(hydrator.clone()));
    let fs = QuickFS::new(namespace, hydrator, heuristics).with_acl(acl, identity);

    CapFixture {
        fs,
        endpoint: shutdown_endpoint,
        store: shutdown_store,
        docs,
        docs_child,
        other_child,
        look_alike_child,
    }
}

fn read_cap(who: [u8; 32], path: &str) -> CapOverride {
    CapOverride::new(
        Subject::User(Ed25519PublicKey(who)),
        Scope::FsPath(path.into()),
        Effect::Grant,
        CapAccess::Read,
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn normal_permissions_scope_enforces_native_namespace_paths() {
    let scoped = read_cap(ALICE, "/var/workspace/w/docs");
    let f = cap_fixture(ALICE, &[scoped], Some(1_000)).await;
    assert_eq!(f.fs.check_access(f.docs_child, Access::Read), Ok(()));
    assert_eq!(
        f.fs.check_access(f.docs_child, Access::Write),
        Err(libc::EROFS)
    );
    assert_eq!(
        f.fs.check_access(f.other_child, Access::Read),
        Err(libc::ENOENT)
    );
    assert_eq!(
        f.fs.check_access(f.look_alike_child, Access::Read),
        Err(libc::ENOENT)
    );
    f.shutdown().await;
    let mut root = read_cap(ALICE, "/var/workspace");
    root.access = CapAccess::Write;
    let f = cap_fixture(ALICE, &[root], Some(1_000)).await;
    assert_eq!(f.fs.check_access(ROOT_INO, Access::Read), Ok(()));
    assert_eq!(f.fs.check_access(f.docs_child, Access::Write), Ok(()));
    assert_eq!(f.fs.check_access(f.other_child, Access::Write), Ok(()));
    f.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(
    clippy::too_many_lines,
    reason = "each phase keeps one former test's body verbatim"
)]
async fn capabilities_grant_exactly_their_path_verb_and_lifetime() {
    {
        let f = cap_fixture(ALICE, &[read_cap(ALICE, "/w/docs")], Some(1_000)).await;

        assert_eq!(
            f.fs.check_access(f.docs_child, Access::Read),
            Ok(()),
            "the granted subtree is readable"
        );
        assert_eq!(
            f.fs.check_access(f.other_child, Access::Read),
            Err(libc::ENOENT),
            "a capability on /w/docs must NOT reach /w/other/private.md"
        );
        assert_eq!(
            f.fs.check_access(f.other_child, Access::Write),
            Err(libc::ENOENT),
            "nor may it write there"
        );
        f.shutdown().await;
    }
    {
        let f = cap_fixture(ALICE, &[read_cap(ALICE, "/w/docs")], Some(1_000)).await;

        assert_eq!(
            f.fs.check_access(f.docs, Access::Read),
            Ok(()),
            "the granted directory itself is readable"
        );
        assert_eq!(
            f.fs.check_access(f.look_alike_child, Access::Read),
            Err(libc::ENOENT),
            "/w/docs must NOT cover /w/docsSECRET/leak.md — component boundary"
        );
        f.shutdown().await;
    }
    {
        let f = cap_fixture(ALICE, &[read_cap(ALICE, "/w/docs")], Some(1_000)).await;

        assert_eq!(
            f.fs.check_access(f.docs_child, Access::Read),
            Ok(()),
            "read is what was granted"
        );
        assert_eq!(
            f.fs.check_access(f.docs_child, Access::Write),
            Err(libc::EROFS),
            "a read-only capability sees a read-only filesystem, not a refusal"
        );
        f.shutdown().await;
    }
    {
        let expired = read_cap(ALICE, "/w/docs").with_expiry(1_000);

        let live = cap_fixture(ALICE, std::slice::from_ref(&expired), Some(999)).await;
        assert_eq!(
            live.fs.check_access(live.docs_child, Access::Read),
            Ok(()),
            "the capability allows the read while it is live"
        );

        let lapsed = cap_fixture(ALICE, &[expired], Some(1_001)).await;
        assert_eq!(
            lapsed.fs.check_access(lapsed.docs_child, Access::Read),
            Err(libc::ENOENT),
            "a capability whose AtUnix deadline has passed must be denied"
        );
        assert_eq!(
            lapsed.fs.check_access(lapsed.docs, Access::Read),
            Err(libc::ENOENT),
            "including on the granted directory itself"
        );
        live.shutdown().await;
        lapsed.shutdown().await;
    }
    {
        let f = cap_fixture(BOB, &[read_cap(ALICE, "/w/docs")], Some(1_000)).await;

        for (ino, what) in [
            (f.docs, "/w/docs"),
            (f.docs_child, "/w/docs/report.md"),
            (f.other_child, "/w/other/private.md"),
            (f.look_alike_child, "/w/docsSECRET/leak.md"),
        ] {
            assert_eq!(
                f.fs.check_access(ino, Access::Read),
                Err(libc::ENOENT),
                "an identity with no capability must not see {what} at all"
            );
            assert_eq!(
                f.fs.check_access(ino, Access::Write),
                Err(libc::ENOENT),
                "nor may it write to {what}"
            );
        }
        assert_eq!(
            f.fs.check_access(ROOT_INO, Access::Read),
            Err(libc::ENOENT),
            "the root holds nothing readable for an identity with no capability"
        );
        assert_eq!(
            f.fs.check_access(ROOT_INO, Access::Write),
            Err(libc::EROFS),
            "and it is read-only rather than forbidden"
        );
        f.shutdown().await;
    }
    {
        let workspace_cap = CapOverride::new(
            Subject::workspace("another-workspace"),
            Scope::FsPath("/w/docs".into()),
            Effect::Grant,
            CapAccess::Write,
        );
        let f = cap_fixture(ALICE, &[workspace_cap], Some(1_000)).await;

        assert_eq!(
            f.fs.check_access(f.docs_child, Access::Read),
            Err(libc::ENOENT),
            "a grant to another workspace's members must not authorise a path for this mount"
        );
        f.shutdown().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(
    clippy::too_many_lines,
    reason = "each phase keeps one former test's body verbatim"
)]
async fn the_live_fuse_access_path_enforces_current_identity_grants() {
    async fn fuse_mount_enforces_identity_acl_end_to_end() {
        require_fuse!();
        tilo_test_env::within("the identity-ACL FUSE mounts", async {
            let endpoint = Endpoint::bind(presets::Minimal)
                .await
                .expect("bind endpoint");
            let store = Arc::new(ContentStore::memory());
            let body = b"members only";
            let hash = store.write_content(body).await.unwrap().unwrap();

            let namespace = Arc::new(Mutex::new(Namespace::new("acl-mount-peer").unwrap()));
            {
                let mut ns = namespace.lock().unwrap();
                ns.create_file(ROOT_INO, "secret.txt", Some(&hash), body.len() as u64, 1)
                    .unwrap();
                drop(ns);
            }
            let shutdown_endpoint = endpoint.clone();
            let shutdown_store = store.clone();
            let hydrator = Arc::new(Hydrator::new(endpoint, store));
            let heuristics = Arc::new(Heuristics::new(hydrator.clone()));

            let mut acl = WorkspaceAcl::new();
            acl.grant_read(BOB, "/secret.txt");

            let alice_dir = tempdir().unwrap();
            let alice_fs = QuickFS::new(namespace.clone(), hydrator.clone(), heuristics.clone())
                .with_acl(acl.clone(), ALICE);
            let mut cfg_a = Config::default();
            cfg_a.mount_options = vec![MountOption::FSName("quickfs_acl_alice".into())];
            let sa = fuser::spawn_mount(alice_fs, alice_dir.path(), &cfg_a)
                .expect("spawn alice session");
            wait_for_mount(alice_dir.path()).await;

            let alice_read = std::fs::read(alice_dir.path().join("secret.txt"));
            assert!(
                matches!(&alice_read, Err(e) if e.kind() == std::io::ErrorKind::NotFound),
                "Alice (no cap) must see no such file through the kernel, got {alice_read:?}"
            );
            let alice_entries: Vec<String> = std::fs::read_dir(alice_dir.path())
                .expect("Alice can list her (empty) view")
                .map(|e| e.unwrap().file_name().into_string().unwrap())
                .collect();
            assert!(
                alice_entries.is_empty(),
                "Alice's view of the workspace must be an empty directory, got {alice_entries:?}"
            );

            let bob_dir = tempdir().unwrap();
            let bob_fs = QuickFS::new(namespace.clone(), hydrator, heuristics).with_acl(acl, BOB);
            let mut cfg_b = Config::default();
            cfg_b.mount_options = vec![MountOption::FSName("quickfs_acl_bob".into())];
            let sb = fuser::spawn_mount(bob_fs, bob_dir.path(), &cfg_b).expect("spawn bob session");
            wait_for_mount(bob_dir.path()).await;

            let bob_read = std::fs::read(bob_dir.path().join("secret.txt")).expect("Bob may read");
            assert_eq!(bob_read, body, "Bob (holds cap) reads the real content");
            fs::fuse_session::unmount_and_join(sa, alice_dir.path())
                .expect("unmount Alice session");
            fs::fuse_session::unmount_and_join(sb, bob_dir.path()).expect("unmount Bob session");
            shutdown_endpoint.close().await;
            shutdown_store
                .store()
                .shutdown()
                .await
                .expect("shut down blob store");
        })
        .await;
    }
    {
        async fn mount_as(
            namespace: Arc<Mutex<Namespace>>,
            acl: Option<(WorkspaceAcl, [u8; 32])>,
        ) -> (QuickFS, Endpoint, Arc<ContentStore>) {
            let endpoint = Endpoint::bind(presets::Minimal)
                .await
                .expect("bind endpoint");
            let store = Arc::new(ContentStore::memory());
            let shutdown_endpoint = endpoint.clone();
            let shutdown_store = store.clone();
            let hydrator = Arc::new(Hydrator::new(endpoint, store));
            let heuristics = Arc::new(Heuristics::new(hydrator.clone()));
            let fs = QuickFS::new(namespace, hydrator, heuristics);
            let fs = match acl {
                Some((a, id)) => fs.with_acl(a, id),
                None => fs,
            };
            (fs, shutdown_endpoint, shutdown_store)
        }

        let store = Arc::new(ContentStore::memory());
        let body = b"top secret";
        let hash = store.write_content(body).await.unwrap().unwrap();

        let namespace = Arc::new(Mutex::new(Namespace::new("acl-peer").unwrap()));
        let (shared_ino, secret_dir_ino, secret_child_ino) = {
            let mut ns = namespace.lock().unwrap();
            let shared = ns
                .create_file(ROOT_INO, "shared.txt", Some(&hash), body.len() as u64, 1)
                .unwrap();
            let secret_dir = ns.create_dir(ROOT_INO, "secret", 1).unwrap();
            let secret_child = ns
                .create_file(secret_dir, "leak.txt", Some(&hash), body.len() as u64, 1)
                .unwrap();
            drop(ns);
            (shared, secret_dir, secret_child)
        };

        let mut acl = WorkspaceAcl::new();
        acl.grant_read(ALICE, "/shared.txt");
        acl.grant_read_write(BOB, "/secret");

        let (alice_fs, alice_endpoint, alice_store) =
            mount_as(namespace.clone(), Some((acl.clone(), ALICE))).await;
        assert_eq!(
            alice_fs.check_access(shared_ino, Access::Read),
            Ok(()),
            "Alice holds read on /shared.txt"
        );
        assert_eq!(
            alice_fs.check_access(secret_dir_ino, Access::Read),
            Err(libc::ENOENT),
            "Alice has NO read cap on /secret → it is not in her view at all"
        );
        assert_eq!(
            alice_fs.check_access(secret_child_ino, Access::Read),
            Err(libc::ENOENT),
            "Alice denied read on /secret/leak.txt too"
        );
        assert_eq!(
            alice_fs.check_access(shared_ino, Access::Write),
            Err(libc::EROFS),
            "Alice's grant is read-only, so the file is on a read-only mount"
        );

        let (bob_fs, bob_endpoint, bob_store) = mount_as(namespace.clone(), Some((acl, BOB))).await;
        assert_eq!(
            bob_fs.check_access(secret_child_ino, Access::Read),
            Ok(()),
            "Bob holds read on /secret/**"
        );
        assert_eq!(
            bob_fs.check_access(secret_child_ino, Access::Write),
            Ok(()),
            "Bob holds write on /secret/**"
        );
        assert_eq!(
            bob_fs.check_access(shared_ino, Access::Read),
            Err(libc::ENOENT),
            "Bob has NO cap on /shared.txt → absent from his view"
        );

        let (open_fs, open_endpoint, open_store) = mount_as(namespace, None).await;
        assert_eq!(
            open_fs.check_access(shared_ino, Access::Write),
            Ok(()),
            "a mount without an ACL allows access (compat veneer)"
        );
        drop((alice_fs, bob_fs, open_fs));
        shutdown_runtime_fixture(alice_endpoint, alice_store).await;
        shutdown_runtime_fixture(bob_endpoint, bob_store).await;
        shutdown_runtime_fixture(open_endpoint, open_store).await;
        store.store().shutdown().await.unwrap();
    }
    fuse_mount_enforces_identity_acl_end_to_end().await;
    {
        use fs::CapabilityFile;

        let dir = tempdir().unwrap();
        let caps_path = dir.path().join("caps.json");
        put_caps(&caps_path, &[read_cap(ALICE, "/w/docs")]);

        let file = CapabilityFile::load(&caps_path, ALICE, Vec::new()).expect("loads");
        let f = cap_fixture_with(ALICE, file.handle()).await;

        assert_eq!(
            f.fs.check_access(f.other_child, Access::Read),
            Err(libc::ENOENT),
            "the sibling subtree must be denied before it is granted, or the reload proves nothing"
        );
        assert_eq!(f.fs.check_access(f.docs_child, Access::Read), Ok(()));

        put_caps(
            &caps_path,
            &[read_cap(ALICE, "/w/docs"), read_cap(ALICE, "/w/other")],
        );
        assert_eq!(file.refresh(), fs::Refresh::Reloaded(2));

        assert_eq!(
            f.fs.check_access(f.other_child, Access::Read),
            Ok(()),
            "a capability granted after the mount came up must be honoured by the same live mount"
        );
        f.shutdown().await;
    }
    {
        use fs::CapabilityFile;

        let dir = tempdir().unwrap();
        let caps_path = dir.path().join("caps.json");
        put_caps(
            &caps_path,
            &[read_cap(ALICE, "/w/docs"), read_cap(ALICE, "/w/other")],
        );

        let file = CapabilityFile::load(&caps_path, ALICE, Vec::new()).expect("loads");
        let f = cap_fixture_with(ALICE, file.handle()).await;
        assert_eq!(f.fs.check_access(f.other_child, Access::Read), Ok(()));

        put_caps(&caps_path, &[read_cap(ALICE, "/w/docs")]);
        assert_eq!(file.refresh(), fs::Refresh::Reloaded(1));

        assert_eq!(
            f.fs.check_access(f.other_child, Access::Read),
            Err(libc::ENOENT),
            "a revoked capability must stop working on the live mount, not at the next remount"
        );
        assert_eq!(
            f.fs.check_access(f.docs_child, Access::Read),
            Ok(()),
            "revoking one capability must not disturb the others"
        );
        f.shutdown().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(
    clippy::too_many_lines,
    reason = "each phase keeps one former test's body verbatim"
)]
async fn fuse_projects_reads_writes_and_reports_sync_state() {
    {
        require_fuse!();
        tilo_test_env::within("the projected FUSE tree reads", async {
            let mount_dir = tempdir().expect("mount temp dir");
            let mount_path = mount_dir.path().to_path_buf();

            let endpoint = Endpoint::bind(presets::Minimal)
                .await
                .expect("bind endpoint");

            let store = Arc::new(ContentStore::memory());
            let body = b"hello from the collaborative filesystem";
            let hash = store
                .write_content(body)
                .await
                .expect("write_content")
                .expect("non-empty");

            let namespace = Arc::new(Mutex::new(Namespace::new("test-peer").unwrap()));
            {
                let mut ns = namespace.lock().unwrap();
                ns.create_file(ROOT_INO, "test_file.txt", Some(&hash), body.len() as u64, 1)
                    .unwrap();
                let subdir = ns.create_dir(ROOT_INO, "subdir", 1).unwrap();
                ns.create_file(subdir, "nested.txt", Some(&hash), body.len() as u64, 1)
                    .unwrap();
                drop(ns);
            }

            let shutdown_endpoint = endpoint.clone();
            let shutdown_store = store.clone();
            let hydrator = Arc::new(Hydrator::new(endpoint, store.clone()));
            let heuristics = Arc::new(Heuristics::new(hydrator.clone()));
            let fs = QuickFS::new(namespace, hydrator, heuristics);

            let options = vec![
                MountOption::RO,
                MountOption::FSName("quickfs_test".to_string()),
            ];
            let mut config = Config::default();
            config.mount_options = options;

            let session = fuser::spawn_mount(fs, &mount_path, &config).expect("spawn FUSE session");
            wait_for_mount(&mount_path).await;

            let entries = std::fs::read_dir(&mount_path)
                .expect("read_dir")
                .map(|e| e.unwrap().file_name().into_string().unwrap())
                .collect::<Vec<_>>();
            assert!(entries.contains(&"test_file.txt".to_string()));
            assert!(entries.contains(&"subdir".to_string()));

            let nested = std::fs::read_dir(mount_path.join("subdir"))
                .expect("read subdir")
                .map(|e| e.unwrap().file_name().into_string().unwrap())
                .collect::<Vec<_>>();
            assert!(nested.contains(&"nested.txt".to_string()));

            let read_back = std::fs::read(mount_path.join("test_file.txt")).expect("read file");
            assert_eq!(read_back, body, "FUSE read must return the real content");
            shutdown_fuse_fixture(session, &mount_path, shutdown_endpoint, shutdown_store).await;
        })
        .await;
    }
    {
        require_fuse!();
        tilo_test_env::within(
            "the FUSE write, create, rename and unlink round trip",
            async {
                let mount_dir = tempdir().expect("mount temp dir");
                let path = mount_dir.path().to_path_buf();

                let endpoint = Endpoint::bind(presets::Minimal)
                    .await
                    .expect("bind endpoint");
                let store = Arc::new(ContentStore::memory());
                let namespace = Arc::new(Mutex::new(Namespace::new("rw-peer").unwrap()));
                let shutdown_endpoint = endpoint.clone();
                let shutdown_store = store.clone();
                let hydrator = Arc::new(Hydrator::new(endpoint, store));
                let heuristics = Arc::new(Heuristics::new(hydrator.clone()));
                let fs = QuickFS::new(namespace, hydrator, heuristics);

                let mut config = Config::default();
                config.mount_options = vec![MountOption::FSName("quickfs_rw".to_string())];
                let session = fuser::spawn_mount(fs, &path, &config).expect("spawn FUSE session");
                wait_for_mount(&path).await;

                std::fs::write(path.join("hello.txt"), b"first").expect("write hello");
                assert_eq!(std::fs::read(path.join("hello.txt")).unwrap(), b"first");

                std::fs::write(path.join("hello.txt"), b"second longer content")
                    .expect("overwrite");
                assert_eq!(
                    std::fs::read(path.join("hello.txt")).unwrap(),
                    b"second longer content"
                );

                std::fs::create_dir(path.join("d")).expect("mkdir");
                std::fs::write(path.join("d/n.txt"), b"nested").expect("write nested");
                assert_eq!(std::fs::read(path.join("d/n.txt")).unwrap(), b"nested");

                std::fs::rename(path.join("hello.txt"), path.join("renamed.txt")).expect("rename");
                assert!(!path.join("hello.txt").exists(), "old name gone");
                assert_eq!(
                    std::fs::read(path.join("renamed.txt")).unwrap(),
                    b"second longer content"
                );

                std::fs::remove_file(path.join("renamed.txt")).expect("unlink");
                assert!(!path.join("renamed.txt").exists(), "unlinked file gone");
                shutdown_fuse_fixture(session, &path, shutdown_endpoint, shutdown_store).await;
            },
        )
        .await;
    }
    {
        require_fuse!();
        tilo_test_env::within("the non-UTF8 real-FUSE restart test", async {
            let first_mount = tempdir().unwrap();
            let second_mount = tempdir().unwrap();
            let store = Arc::new(ContentStore::memory());
            let namespace = Arc::new(Mutex::new(Namespace::new("byte-name-peer").unwrap()));
            let endpoint = Endpoint::bind(presets::Minimal).await.unwrap();
            let shutdown_endpoint = endpoint.clone();
            let hydrator = Arc::new(Hydrator::new(endpoint, store.clone()));
            let heuristics = Arc::new(Heuristics::new(hydrator.clone()));
            let fs = QuickFS::new(namespace.clone(), hydrator, heuristics);
            let mut config = Config::default();
            config.mount_options = vec![MountOption::FSName("quickfs_non_utf8".into())];
            let session = fuser::spawn_mount(fs, first_mount.path(), &config).unwrap();
            wait_for_mount(first_mount.path()).await;

            let mut original = first_mount.path().to_path_buf();
            original.push(std::ffi::OsString::from_vec(b"raw-\xFF.txt".to_vec()));
            let mut renamed = first_mount.path().to_path_buf();
            renamed.push(std::ffi::OsString::from_vec(b"renamed-\xFE.txt".to_vec()));
            std::fs::write(&original, b"exact bytes").unwrap();
            std::fs::rename(&original, &renamed).unwrap();
            assert_eq!(std::fs::read(&renamed).unwrap(), b"exact bytes");
            let listed = std::fs::read_dir(first_mount.path())
                .unwrap()
                .map(|entry| entry.unwrap().file_name().into_vec())
                .collect::<Vec<_>>();
            assert!(listed.contains(&b"renamed-\xFE.txt".to_vec()));

            let snapshot = namespace.lock().unwrap().save();
            fs::fuse_session::unmount_and_join(session, first_mount.path()).unwrap();
            shutdown_endpoint.close().await;
            let reopened = Arc::new(Mutex::new(
                Namespace::from_bytes("reopened", &snapshot).unwrap(),
            ));
            let endpoint = Endpoint::bind(presets::Minimal).await.unwrap();
            let shutdown_endpoint = endpoint.clone();
            let hydrator = Arc::new(Hydrator::new(endpoint, store.clone()));
            let heuristics = Arc::new(Heuristics::new(hydrator.clone()));
            let fs = QuickFS::new(reopened, hydrator, heuristics);
            let session = fuser::spawn_mount(fs, second_mount.path(), &config).unwrap();
            wait_for_mount(second_mount.path()).await;
            let mut after_restart = second_mount.path().to_path_buf();
            after_restart.push(std::ffi::OsString::from_vec(b"renamed-\xFE.txt".to_vec()));
            assert_eq!(std::fs::read(after_restart).unwrap(), b"exact bytes");
            fs::fuse_session::unmount_and_join(session, second_mount.path()).unwrap();
            shutdown_endpoint.close().await;
            store.store().shutdown().await.unwrap();
        })
        .await;
    }
    {
        require_fuse!();
        tilo_test_env::within("the FUSE rmdir and unlink checks", async {
            let mount_dir = tempdir().expect("mount temp dir");
            let path = mount_dir.path().to_path_buf();

            let endpoint = Endpoint::bind(presets::Minimal)
                .await
                .expect("bind endpoint");
            let store = Arc::new(ContentStore::memory());
            let namespace = Arc::new(Mutex::new(Namespace::new("rm-peer").unwrap()));
            let shutdown_endpoint = endpoint.clone();
            let shutdown_store = store.clone();
            let hydrator = Arc::new(Hydrator::new(endpoint, store));
            let heuristics = Arc::new(Heuristics::new(hydrator.clone()));
            let fs = QuickFS::new(namespace, hydrator, heuristics);

            let mut config = Config::default();
            config.mount_options = vec![MountOption::FSName("quickfs_rm".to_string())];
            let session = fuser::spawn_mount(fs, &path, &config).expect("spawn FUSE session");
            wait_for_mount(&path).await;

            std::fs::create_dir(path.join("dir")).expect("mkdir");
            std::fs::write(path.join("dir/inner.txt"), b"keepme").expect("write inner");

            assert!(
                std::fs::remove_dir(path.join("dir")).is_err(),
                "rmdir of a non-empty dir must fail"
            );
            assert_eq!(
                std::fs::read(path.join("dir/inner.txt")).unwrap(),
                b"keepme",
                "contents must survive a failed rmdir"
            );

            assert!(
                std::fs::remove_file(path.join("dir")).is_err(),
                "unlink of a directory must fail"
            );

            std::fs::remove_file(path.join("dir/inner.txt")).expect("unlink inner");
            std::fs::remove_dir(path.join("dir")).expect("rmdir empty dir");
            assert!(!path.join("dir").exists(), "empty dir removed");
            shutdown_fuse_fixture(session, &path, shutdown_endpoint, shutdown_store).await;
        })
        .await;
    }
    {
        require_fuse!();
        tilo_test_env::within("the FUSE document round trip", async {
            let mount_dir = tempdir().expect("mount temp dir");
            let path = mount_dir.path().to_path_buf();

            let endpoint = Endpoint::bind(presets::Minimal)
                .await
                .expect("bind endpoint");
            let store = Arc::new(ContentStore::memory());
            let namespace = Arc::new(Mutex::new(Namespace::new("doc-peer").unwrap()));
            let shutdown_endpoint = endpoint.clone();
            let shutdown_store = store.clone();
            let hydrator = Arc::new(Hydrator::new(endpoint, store));
            let heuristics = Arc::new(Heuristics::new(hydrator.clone()));
            let fs = QuickFS::new(namespace, hydrator, heuristics);

            let mut config = Config::default();
            config.mount_options = vec![MountOption::FSName("quickfs_doc".to_string())];
            let session = fuser::spawn_mount(fs, &path, &config).expect("spawn FUSE session");
            wait_for_mount(&path).await;

            std::fs::write(path.join("note.md"), b"# Title\n\nHello world\n").expect("write md");
            let back = std::fs::read_to_string(path.join("note.md")).expect("read md");
            assert!(
                back.contains("Title") && back.contains("Hello world"),
                "document round-trip lost content: {back:?}"
            );

            let question = b"Question X\nfirst line of thought\n";
            std::fs::write(path.join("question.md"), question).expect("write question");
            assert_eq!(
                std::fs::read(path.join("question.md")).unwrap(),
                question,
                "a document's file is the bytes written"
            );

            std::fs::write(path.join("data.bin"), b"\x00\x01raw-bytes\xff").expect("write bin");
            assert_eq!(
                std::fs::read(path.join("data.bin")).unwrap(),
                b"\x00\x01raw-bytes\xff"
            );

            {
                use std::os::unix::fs::PermissionsExt as _;
                let tool = path.join("tool");
                std::fs::write(&tool, b"#!/bin/sh\necho ran\n").expect("write tool");
                let mode = |file: &std::path::Path| {
                    std::fs::metadata(file).unwrap().permissions().mode() & 0o7777
                };
                assert_eq!(mode(&tool), 0o644, "a new file has the default mode");
                std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o755))
                    .expect("chmod +x");
                assert_eq!(mode(&tool), 0o755, "chmod is kept");
                let ran = std::process::Command::new(&tool)
                    .output_locked()
                    .expect("the tool runs");
                assert_eq!(ran.stdout, b"ran\n");
                std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o4755))
                    .expect("chmod u+s");
                assert_eq!(mode(&tool), 0o755, "setuid never sticks");
            }
            shutdown_fuse_fixture(session, &path, shutdown_endpoint, shutdown_store).await;
        })
        .await;
    }
    {
        require_fuse!();
        tilo_test_env::within("the FUSE sync-state xattrs", async {
            let mount_dir = tempdir().expect("mount temp dir");
            let path = mount_dir.path().to_path_buf();

            let endpoint = Endpoint::bind(presets::Minimal)
                .await
                .expect("bind endpoint");
            let store = Arc::new(ContentStore::memory());

            let body = b"content that is already here";
            let local_hash = store.write_content(body).await.unwrap().unwrap();

            let projected_hash = iroh_blobs::Hash::from([3u8; 32]);

            let namespace = Arc::new(Mutex::new(Namespace::new("xattr-peer").unwrap()));
            {
                let mut ns = namespace.lock().unwrap();
                ns.create_file(
                    ROOT_INO,
                    "here.txt",
                    Some(&local_hash),
                    body.len() as u64,
                    1,
                )
                .unwrap();
                ns.create_file(ROOT_INO, "remote.txt", Some(&projected_hash), 4096, 1)
                    .unwrap();
                ns.create_dir(ROOT_INO, "folder", 1).unwrap();
                drop(ns);
            }

            let shutdown_endpoint = endpoint.clone();
            let shutdown_store = store.clone();
            let hydrator = Arc::new(Hydrator::new(endpoint, store));
            let heuristics = Arc::new(Heuristics::new(hydrator.clone()));
            let fs = QuickFS::new(namespace, hydrator, heuristics);

            let mut config = Config::default();
            config.mount_options = vec![MountOption::FSName("quickfs_xattr".to_string())];
            let session = fuser::spawn_mount(fs, &path, &config).expect("spawn FUSE session");
            wait_for_mount(&path).await;

            assert_eq!(
                read_sync_xattr(&path.join("here.txt")).as_deref(),
                Some("local"),
                "a file with its content in the cache reads as local",
            );
            assert_eq!(
                read_sync_xattr(&path.join("remote.txt")).as_deref(),
                Some("projected"),
                "a file whose content is not local reads as projected",
            );
            assert_eq!(
                read_sync_xattr(&path.join("folder")).as_deref(),
                Some("local"),
                "a directory is always local",
            );
            shutdown_fuse_fixture(session, &path, shutdown_endpoint, shutdown_store).await;
        })
        .await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(
    clippy::too_many_lines,
    reason = "each phase keeps one former test's body verbatim"
)]
async fn fuse_open_handles_survive_replacement_unlink_and_concurrent_writes() {
    {
        require_fuse!();
        tilo_test_env::within("the real-FUSE replacement test", async {
            let mount_dir = tempdir().expect("mount temp dir");
            let path = mount_dir.path().to_path_buf();
            let endpoint = Endpoint::bind(presets::Minimal)
                .await
                .expect("bind endpoint");
            let store = Arc::new(ContentStore::memory());
            let namespace = Arc::new(Mutex::new(Namespace::new("replace-handle-peer").unwrap()));
            let shutdown_endpoint = endpoint.clone();
            let shutdown_store = store.clone();
            let hydrator = Arc::new(Hydrator::new(endpoint, store));
            let heuristics = Arc::new(Heuristics::new(hydrator.clone()));
            let fs = QuickFS::new(namespace, hydrator, heuristics);
            let mut config = Config::default();
            config.mount_options = vec![MountOption::FSName("quickfs_replace_handle".to_string())];
            let session = fuser::spawn_mount(fs, &path, &config).expect("spawn FUSE session");
            wait_for_mount(&path).await;

            let source = path.join("source.bin");
            let destination = path.join("destination.bin");
            std::fs::write(&source, b"source bytes").expect("write source");
            std::fs::write(&destination, b"replaced bytes").expect("write destination");
            let source_ino = std::fs::metadata(&source).unwrap().ino();
            let replaced_ino = std::fs::metadata(&destination).unwrap().ino();
            assert_ne!(source_ino, replaced_ino);

            let mut replaced_handle = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(&destination)
                .expect("open victim");
            std::fs::rename(&source, &destination).expect("replacement rename");

            assert!(!source.exists());
            assert_eq!(std::fs::metadata(&destination).unwrap().ino(), source_ino);
            assert_eq!(std::fs::read(&destination).unwrap(), b"source bytes");
            let mut old_bytes = Vec::new();
            replaced_handle
                .read_to_end(&mut old_bytes)
                .expect("read replaced open handle");
            assert_eq!(old_bytes, b"replaced bytes");
            replaced_handle.seek(SeekFrom::Start(0)).unwrap();
            replaced_handle.write_all(b"private").unwrap();
            replaced_handle.flush().unwrap();
            replaced_handle.seek(SeekFrom::Start(0)).unwrap();
            let mut private_bytes = Vec::new();
            replaced_handle.read_to_end(&mut private_bytes).unwrap();
            assert_eq!(private_bytes, b"privated bytes");
            assert_eq!(std::fs::read(&destination).unwrap(), b"source bytes");
            drop(replaced_handle);
            shutdown_fuse_fixture(session, &path, shutdown_endpoint, shutdown_store).await;
        })
        .await;
    }
    {
        require_fuse!();
        tilo_test_env::within("the real-FUSE unlink/recreate test", async {
            let mount_dir = tempdir().expect("mount temp dir");
            let endpoint = Endpoint::bind(presets::Minimal)
                .await
                .expect("bind endpoint");
            let store = Arc::new(ContentStore::memory());
            let namespace = Arc::new(Mutex::new(Namespace::new("unlink-handle-peer").unwrap()));
            let shutdown_endpoint = endpoint.clone();
            let shutdown_store = store.clone();
            let hydrator = Arc::new(Hydrator::new(endpoint, store));
            let heuristics = Arc::new(Heuristics::new(hydrator.clone()));
            let fs = QuickFS::new(namespace, hydrator, heuristics);
            let mut config = Config::default();
            config.mount_options = vec![MountOption::FSName("quickfs_unlink_handle".to_string())];
            let session =
                fuser::spawn_mount(fs, mount_dir.path(), &config).expect("spawn FUSE session");
            wait_for_mount(mount_dir.path()).await;

            let path = mount_dir.path().join("same-name.bin");
            std::fs::write(&path, b"old bytes").unwrap();
            let old_ino = std::fs::metadata(&path).unwrap().ino();
            let mut old = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(&path)
                .unwrap();

            std::fs::remove_file(&path).unwrap();
            assert!(!path.exists());
            std::fs::write(&path, b"new bytes").unwrap();
            let new_ino = std::fs::metadata(&path).unwrap().ino();
            assert_ne!(old_ino, new_ino);

            old.seek(SeekFrom::Start(0)).unwrap();
            let mut observed = Vec::new();
            old.read_to_end(&mut observed).unwrap();
            assert_eq!(observed, b"old bytes");
            old.seek(SeekFrom::Start(0)).unwrap();
            old.write_all(b"private").unwrap();
            old.flush().unwrap();
            old.seek(SeekFrom::Start(0)).unwrap();
            observed.clear();
            old.read_to_end(&mut observed).unwrap();
            assert_eq!(observed, b"privatees");
            assert_eq!(std::fs::read(&path).unwrap(), b"new bytes");
            drop(old);
            shutdown_fuse_fixture(session, mount_dir.path(), shutdown_endpoint, shutdown_store)
                .await;
        })
        .await;
    }
    {
        require_fuse!();
        tilo_test_env::within("the real-FUSE two-writer test", async {
            let mount_dir = tempdir().expect("mount temp dir");
            let endpoint = Endpoint::bind(presets::Minimal)
                .await
                .expect("bind endpoint");
            let store = Arc::new(ContentStore::memory());
            let namespace = Arc::new(Mutex::new(Namespace::new("two-writer-peer").unwrap()));
            let shutdown_endpoint = endpoint.clone();
            let shutdown_store = store.clone();
            let hydrator = Arc::new(Hydrator::new(endpoint, store));
            let heuristics = Arc::new(Heuristics::new(hydrator.clone()));
            let fs = QuickFS::new(namespace, hydrator, heuristics);
            let mut config = Config::default();
            config.mount_options = vec![MountOption::FSName("quickfs_two_writers".to_string())];
            let session =
                fuser::spawn_mount(fs, mount_dir.path(), &config).expect("spawn FUSE session");
            wait_for_mount(mount_dir.path()).await;

            let path = mount_dir.path().join("shared.bin");
            std::fs::write(&path, b"abcdef").unwrap();
            let mut left = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(&path)
                .unwrap();
            let mut right = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(&path)
                .unwrap();

            left.seek(SeekFrom::Start(0)).unwrap();
            left.write_all(b"ONE").unwrap();
            left.flush().unwrap();
            right.seek(SeekFrom::Start(3)).unwrap();
            right.write_all(b"TWO").unwrap();
            right.flush().unwrap();
            drop(left);
            drop(right);

            assert_eq!(std::fs::read(&path).unwrap(), b"ONETWO");
            shutdown_fuse_fixture(session, mount_dir.path(), shutdown_endpoint, shutdown_store)
                .await;
        })
        .await;
    }
}

fn read_sync_xattr(path: &std::path::Path) -> Option<String> {
    use std::os::unix::ffi::OsStrExt;
    let cpath = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
    let name = std::ffi::CString::new("user.tilo.sync").unwrap();
    let mut buf = [0u8; 64];
    let n = unsafe {
        libc::getxattr(
            cpath.as_ptr(),
            name.as_ptr(),
            buf.as_mut_ptr().cast::<libc::c_void>(),
            buf.len(),
        )
    };
    let Ok(n) = usize::try_from(n) else {
        return None;
    };
    Some(String::from_utf8_lossy(&buf[..n]).into_owned())
}

fn put_caps(path: &std::path::Path, caps: &[CapOverride]) {
    std::fs::write(
        path,
        serde_json::to_vec(&collab_types::FsAuthority::from(caps.to_vec())).unwrap(),
    )
    .unwrap();
    std::thread::sleep(Duration::from_millis(10));
}

struct ViewFixture {
    session: fuser::BackgroundSession,
    endpoint: Endpoint,
    store: Arc<ContentStore>,
    dir: tempfile::TempDir,
    namespace: Arc<Mutex<Namespace>>,
}

impl ViewFixture {
    fn path(&self) -> &std::path::Path {
        self.dir.path()
    }

    fn heads(&self) -> Vec<automerge::ChangeHash> {
        self.namespace.lock().unwrap().heads()
    }

    async fn shutdown(self) {
        shutdown_fuse_fixture(self.session, self.dir.path(), self.endpoint, self.store).await;
    }
}

async fn view_fixture(acl: Option<(WorkspaceAcl, [u8; 32])>, name: &str) -> ViewFixture {
    let endpoint = Endpoint::bind(presets::Minimal)
        .await
        .expect("bind endpoint");
    let store = Arc::new(ContentStore::memory());
    let body = b"contents";
    let hash = store.write_content(body).await.unwrap().unwrap();
    let namespace = Arc::new(Mutex::new(Namespace::new("view-peer").unwrap()));
    {
        let mut ns = namespace.lock().unwrap();
        let w = ns.create_dir(ROOT_INO, "w", 1).unwrap();
        let docs = ns.create_dir(w, "docs", 1).unwrap();
        ns.create_file(docs, "report.md", Some(&hash), body.len() as u64, 1)
            .unwrap();
        let sub = ns.create_dir(docs, "sub", 1).unwrap();
        ns.create_file(sub, "deep.md", Some(&hash), body.len() as u64, 1)
            .unwrap();
        let other = ns.create_dir(w, "other", 1).unwrap();
        ns.create_file(other, "private.md", Some(&hash), body.len() as u64, 1)
            .unwrap();
        let look_alike = ns.create_dir(w, "docsSECRET", 1).unwrap();
        ns.create_file(look_alike, "leak.md", Some(&hash), body.len() as u64, 1)
            .unwrap();
        let vault = ns.create_dir(ROOT_INO, "vault", 1).unwrap();
        ns.create_file(vault, "keys.txt", Some(&hash), body.len() as u64, 1)
            .unwrap();
        let archive = ns.create_dir(ROOT_INO, "archive", 1).unwrap();
        ns.create_file(archive, "old.txt", Some(&hash), body.len() as u64, 1)
            .unwrap();
        drop(ns);
    }
    let shutdown_endpoint = endpoint.clone();
    let shutdown_store = store.clone();
    let hydrator = Arc::new(Hydrator::new(endpoint, store));
    let heuristics = Arc::new(Heuristics::new(hydrator.clone()));
    let fs = QuickFS::new(namespace.clone(), hydrator, heuristics);
    let fs = match acl {
        Some((a, id)) => fs.with_acl(a, id),
        None => fs,
    };
    let dir = tempdir().unwrap();
    let mut config = Config::default();
    config.mount_options = vec![MountOption::FSName(name.to_string())];
    let session = fuser::spawn_mount(fs, dir.path(), &config).expect("spawn FUSE session");
    wait_for_mount(dir.path()).await;
    ViewFixture {
        session,
        endpoint: shutdown_endpoint,
        store: shutdown_store,
        dir,
        namespace,
    }
}

fn docs_read_only() -> (WorkspaceAcl, [u8; 32]) {
    (
        WorkspaceAcl::from_capabilities(ALICE, &[read_cap(ALICE, "/w/docs")], &[], None),
        ALICE,
    )
}

fn list(dir: &std::path::Path) -> Result<Vec<String>, i32> {
    match std::fs::read_dir(dir) {
        Ok(rd) => {
            let mut names: Vec<String> = rd
                .map(|e| e.unwrap().file_name().into_string().unwrap())
                .collect();
            names.sort();
            Ok(names)
        }
        Err(e) => Err(e.raw_os_error().unwrap_or(0)),
    }
}

fn stat_errno(path: &std::path::Path) -> Option<i32> {
    std::fs::symlink_metadata(path)
        .err()
        .map(|e| e.raw_os_error().unwrap_or(0))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(
    clippy::too_many_lines,
    reason = "each phase keeps one former test's body verbatim"
)]
async fn denied_paths_are_invisible_and_read_grants_are_read_only() {
    {
        require_fuse!();
        let f = view_fixture(Some(docs_read_only()), "quickfs_view_listing").await;
        let p = f.path();

        assert_eq!(
            list(p),
            Ok(vec!["w".to_string()]),
            "the root shows only the directory that leads to the grant"
        );
        assert_eq!(
            list(&p.join("w")),
            Ok(vec!["docs".to_string()]),
            "/w shows only the granted subtree — no `other`, no `docsSECRET`"
        );
        assert_eq!(
            list(&p.join("w/docs")),
            Ok(vec!["report.md".to_string(), "sub".to_string()]),
            "the granted subtree is listed in full"
        );
        assert_eq!(
            list(&p.join("w/docs/sub")),
            Ok(vec!["deep.md".to_string()]),
            "and so is everything beneath it"
        );

        let open = view_fixture(None, "quickfs_view_listing_open").await;
        assert_eq!(
            list(open.path()),
            Ok(vec![
                "archive".to_string(),
                "vault".to_string(),
                "w".to_string()
            ])
        );
        assert_eq!(
            list(&open.path().join("w")),
            Ok(vec![
                "docs".to_string(),
                "docsSECRET".to_string(),
                "other".to_string()
            ])
        );
        f.shutdown().await;
        open.shutdown().await;
    }
    {
        require_fuse!();
        let f = view_fixture(Some(docs_read_only()), "quickfs_view_oracle").await;
        let p = f.path();

        let pairs = [
            ("w/other", "w/never-existed"),
            ("w/other/private.md", "w/never-existed/nope.md"),
            ("w/docsSECRET", "w/docsPUBLIC"),
            ("w/docsSECRET/leak.md", "w/docsPUBLIC/leak.md"),
            ("vault", "cellar"),
            ("vault/keys.txt", "cellar/keys.txt"),
            ("archive/old.txt", "cellar/old.txt"),
        ];

        for (hidden, absent) in pairs {
            let h = p.join(hidden);
            let a = p.join(absent);

            assert_eq!(
                stat_errno(&h),
                Some(libc::ENOENT),
                "stat of the hidden {hidden} must be ENOENT"
            );
            assert_eq!(
                stat_errno(&h),
                stat_errno(&a),
                "stat distinguishes hidden {hidden} from absent {absent}"
            );
            assert_eq!(
                std::fs::read(&h).err().map(|e| e.raw_os_error()),
                std::fs::read(&a).err().map(|e| e.raw_os_error()),
                "read distinguishes hidden {hidden} from absent {absent}"
            );
            assert_eq!(
                list(&h),
                list(&a),
                "readdir distinguishes hidden {hidden} from absent {absent}"
            );
            assert_eq!(
                std::fs::write(&h, b"x").err().map(|e| e.raw_os_error()),
                std::fs::write(&a, b"x").err().map(|e| e.raw_os_error()),
                "write distinguishes hidden {hidden} from absent {absent}"
            );
            assert_eq!(
                std::fs::remove_file(&h).err().map(|e| e.raw_os_error()),
                std::fs::remove_file(&a).err().map(|e| e.raw_os_error()),
                "unlink distinguishes hidden {hidden} from absent {absent}"
            );
            assert_eq!(
                std::fs::create_dir(&h).err().map(|e| e.raw_os_error()),
                std::fs::create_dir(&a).err().map(|e| e.raw_os_error()),
                "mkdir distinguishes hidden {hidden} from absent {absent}"
            );
            assert_eq!(
                std::fs::rename(&h, p.join("w/docs/stolen"))
                    .err()
                    .map(|e| e.raw_os_error()),
                std::fs::rename(&a, p.join("w/docs/stolen"))
                    .err()
                    .map(|e| e.raw_os_error()),
                "rename distinguishes hidden {hidden} from absent {absent}"
            );
            assert_eq!(
                read_sync_xattr(&h),
                read_sync_xattr(&a),
                "the sync xattr distinguishes hidden {hidden} from absent {absent}"
            );
        }

        let before = f.heads();
        let _ = std::fs::write(p.join("vault/keys.txt"), b"x");
        let _ = std::fs::remove_file(p.join("vault/keys.txt"));
        assert_eq!(
            f.heads(),
            before,
            "probing for hidden paths must not modify the document"
        );
        f.shutdown().await;
    }
    {
        use std::os::unix::fs::MetadataExt;

        require_fuse!();

        let f = view_fixture(Some(docs_read_only()), "quickfs_view_nlink").await;
        let open = view_fixture(None, "quickfs_view_nlink_open").await;

        for (rel, expect) in [("", 3u64), ("w", 3), ("w/docs", 3), ("w/docs/sub", 2)] {
            let md = std::fs::metadata(f.path().join(rel)).expect("stat in the filtered view");
            assert_eq!(
                md.nlink(),
                expect,
                "nlink of /{rel} must count only the subdirectories the listing shows"
            );
            assert_eq!(md.len(), 0, "a directory's size never encodes its contents");
        }

        let root = std::fs::metadata(open.path()).expect("stat unfiltered root");
        assert_eq!(
            root.nlink(),
            5,
            "unfiltered, the root has three subdirectories"
        );
        let w = std::fs::metadata(open.path().join("w")).expect("stat unfiltered /w");
        assert_eq!(w.nlink(), 5, "unfiltered, /w has three subdirectories");
        f.shutdown().await;
        open.shutdown().await;
    }
    {
        require_fuse!();
        let f = view_fixture(Some(docs_read_only()), "quickfs_view_rofs").await;
        let p = f.path();
        let before = f.heads();

        let attempts: Vec<(&str, std::io::Result<()>)> = vec![
            (
                "overwrite a granted file",
                std::fs::write(p.join("w/docs/report.md"), b"tampered"),
            ),
            (
                "create a new file in a granted directory",
                std::fs::write(p.join("w/docs/new.md"), b"new"),
            ),
            (
                "mkdir in a granted directory",
                std::fs::create_dir(p.join("w/docs/newdir")),
            ),
            (
                "unlink a granted file",
                std::fs::remove_file(p.join("w/docs/report.md")),
            ),
            (
                "rmdir a granted directory",
                std::fs::remove_dir(p.join("w/docs/sub")),
            ),
            (
                "rename within a granted directory",
                std::fs::rename(p.join("w/docs/report.md"), p.join("w/docs/moved.md")),
            ),
            (
                "truncate a granted file",
                std::fs::OpenOptions::new()
                    .write(true)
                    .truncate(true)
                    .open(p.join("w/docs/report.md"))
                    .map(|_| ()),
            ),
        ];
        for (what, outcome) in attempts {
            assert_eq!(
                outcome.err().map(|e| e.raw_os_error().unwrap_or(0)),
                Some(libc::EROFS),
                "{what}: a read grant must present a read-only filesystem"
            );
        }

        assert_eq!(
            std::fs::read(p.join("w/docs/report.md")).unwrap(),
            b"contents",
            "the file the writes were aimed at is untouched"
        );
        assert_eq!(
            f.heads(),
            before,
            "no refused write may reach the CRDT — not even as an empty change"
        );
        assert_eq!(
            list(&p.join("w/docs")),
            Ok(vec!["report.md".to_string(), "sub".to_string()]),
            "and the directory is exactly as it was"
        );
        f.shutdown().await;
    }
    {
        require_fuse!();
        let mut acl = WorkspaceAcl::new();
        acl.grant(ALICE, "/w", false, true);
        acl.grant_read(ALICE, "/w/docs");
        let f = view_fixture(Some((acl, ALICE)), "quickfs_view_residual").await;
        let p = f.path();
        let before = f.heads();

        assert_eq!(
            list(&p.join("w")),
            Ok(vec!["docs".to_string()]),
            "the writable-but-unreadable siblings are still absent from the listing"
        );
        assert_eq!(
            std::fs::create_dir(p.join("w/other"))
                .err()
                .map(|e| e.raw_os_error().unwrap_or(0)),
            Some(libc::EROFS),
            "a name taken by something outside the view must not be created over"
        );
        assert_eq!(
            f.heads(),
            before,
            "and nothing about the hidden entry may change"
        );
        f.shutdown().await;
    }
    {
        require_fuse!();
        let acl = WorkspaceAcl::from_capabilities(ALICE, &[read_cap(ALICE, "/shared")], &[], None);
        let fixture = document_fixture(Some((acl, ALICE)), "quickfs_doc_ro").await;
        let doc = fixture.dir.path().join("shared/notes.md");

        let before = std::fs::read(&doc).expect("a read participant reads the shared document");
        assert!(
            String::from_utf8_lossy(&before).contains("Shared paragraph"),
            "the read must return the real document, not an empty stand-in: {:?}",
            String::from_utf8_lossy(&before)
        );
        let heads_before = fixture.namespace.lock().unwrap().heads();
        let nodes_before = fixture.namespace.lock().unwrap().live_node_ids();

        for (what, outcome) in [
            (
                "rewrite the document",
                std::fs::write(&doc, b"# Tampered\n\nMine now\n"),
            ),
            (
                "open it for writing",
                std::fs::OpenOptions::new()
                    .write(true)
                    .open(&doc)
                    .map(|_| ()),
            ),
            (
                "append to it",
                std::fs::OpenOptions::new()
                    .append(true)
                    .open(&doc)
                    .map(|_| ()),
            ),
            (
                "truncate it",
                std::fs::OpenOptions::new()
                    .write(true)
                    .truncate(true)
                    .open(&doc)
                    .map(|_| ()),
            ),
            (
                "replace it with a new document of the same name",
                std::fs::write(fixture.dir.path().join("shared/notes.md"), b"x"),
            ),
            (
                "add a second document beside it",
                std::fs::write(fixture.dir.path().join("shared/mine.md"), b"# Mine\n"),
            ),
            ("delete it", std::fs::remove_file(&doc)),
            (
                "rename it out of the way",
                std::fs::rename(&doc, fixture.dir.path().join("shared/moved.md")),
            ),
        ] {
            assert_eq!(
                outcome.err().map(|e| e.raw_os_error().unwrap_or(0)),
                Some(libc::EROFS),
                "{what}: a read participant must not be able to originate a change"
            );
        }

        assert_eq!(
            fixture.namespace.lock().unwrap().heads(),
            heads_before,
            "the CRDT change graph must not have advanced"
        );
        assert_eq!(
            fixture.namespace.lock().unwrap().live_node_ids(),
            nodes_before,
            "and no node was added or removed"
        );
        assert_eq!(
            std::fs::read(&doc).expect("still readable"),
            before,
            "the document reads exactly as it did before"
        );

        let rw = WorkspaceAcl::from_capabilities(
            BOB,
            &[CapOverride::new(
                Subject::User(Ed25519PublicKey(BOB)),
                Scope::FsPath("/shared".into()),
                Effect::Grant,
                CapAccess::Write,
            )],
            &[],
            None,
        );
        let fixture2 = document_fixture(Some((rw, BOB)), "quickfs_doc_rw").await;
        let heads2 = fixture2.namespace.lock().unwrap().heads();
        std::fs::write(
            fixture2.dir.path().join("shared/notes.md"),
            b"# Authored\n\nBy Bob\n",
        )
        .expect("an authoring participant may write the shared document");
        assert_ne!(
            fixture2.namespace.lock().unwrap().heads(),
            heads2,
            "an authored write must move the change graph — otherwise the assertions above prove \
             nothing about authorship"
        );
        fixture.shutdown().await;
        fixture2.shutdown().await;
    }
}

struct DocumentFixture {
    session: fuser::BackgroundSession,
    endpoint: Endpoint,
    store: Arc<ContentStore>,
    dir: tempfile::TempDir,
    namespace: Arc<Mutex<Namespace>>,
}

impl DocumentFixture {
    async fn shutdown(self) {
        shutdown_fuse_fixture(self.session, self.dir.path(), self.endpoint, self.store).await;
    }
}

async fn document_fixture(acl: Option<(WorkspaceAcl, [u8; 32])>, name: &str) -> DocumentFixture {
    let endpoint = Endpoint::bind(presets::Minimal)
        .await
        .expect("bind endpoint");
    let store = Arc::new(ContentStore::memory());
    let namespace = Arc::new(Mutex::new(Namespace::new("doc-view-peer").unwrap()));
    {
        let mut ns = namespace.lock().unwrap();
        let shared = ns.create_dir(ROOT_INO, "shared", 1).unwrap();
        let ino = ns.create_document(shared, "notes.md", "md", 1).unwrap();
        let block = ns
            .document_insert_block(ino, None, "paragraph")
            .expect("insert a block");
        assert!(ns.document_splice(ino, &block, 0, 0, "Shared paragraph"));
        drop(ns);
    }
    let shutdown_endpoint = endpoint.clone();
    let shutdown_store = store.clone();
    let hydrator = Arc::new(Hydrator::new(endpoint, store));
    let heuristics = Arc::new(Heuristics::new(hydrator.clone()));
    let fs = QuickFS::new(namespace.clone(), hydrator, heuristics);
    let fs = match acl {
        Some((a, id)) => fs.with_acl(a, id),
        None => fs,
    };
    let dir = tempdir().unwrap();
    let mut config = Config::default();
    config.mount_options = vec![MountOption::FSName(name.to_string())];
    let session = fuser::spawn_mount(fs, dir.path(), &config).expect("spawn FUSE session");
    wait_for_mount(dir.path()).await;
    DocumentFixture {
        session,
        endpoint: shutdown_endpoint,
        store: shutdown_store,
        dir,
        namespace,
    }
}

struct SignalledMount {
    session: fuser::BackgroundSession,
    endpoint: Endpoint,
    store: Arc<ContentStore>,
    dir: tempfile::TempDir,
    namespace: Arc<Mutex<Namespace>>,
    signal: Arc<fs::change_signal::ChangeSignal>,
    availability: Arc<Mutex<fs::AvailabilityRegistry>>,
    hydrator: Arc<Hydrator>,
    _registry_dir: tempfile::TempDir,
}

async fn signalled_mount() -> SignalledMount {
    let endpoint = Endpoint::bind(presets::Minimal)
        .await
        .expect("bind endpoint");
    let store = Arc::new(ContentStore::memory());
    let namespace = Arc::new(Mutex::new(Namespace::new("signalled").unwrap()));
    namespace
        .lock()
        .unwrap()
        .create_dir(ROOT_INO, "docs", 1)
        .unwrap();
    let hydrator = Arc::new(Hydrator::new(endpoint.clone(), store.clone()));
    let heuristics = Arc::new(Heuristics::new(hydrator.clone()));
    let signal = Arc::new(fs::change_signal::ChangeSignal::new().unwrap());
    let registry_dir = tempdir().unwrap();
    let availability = Arc::new(Mutex::new(
        fs::AvailabilityRegistry::open(registry_dir.path().join("availability.json")).unwrap(),
    ));
    let quick_fs = QuickFS::new(namespace.clone(), hydrator.clone(), heuristics)
        .with_change_signal(signal.clone())
        .with_availability(availability.clone());
    let dir = tempdir().unwrap();
    let mut config = Config::default();
    config.mount_options = vec![MountOption::FSName("quickfs_change_signal".to_string())];
    let session = fuser::spawn_mount(quick_fs, dir.path(), &config).expect("spawn FUSE session");
    wait_for_mount(dir.path()).await;
    SignalledMount {
        session,
        endpoint,
        store,
        dir,
        namespace,
        signal,
        availability,
        hydrator,
        _registry_dir: registry_dir,
    }
}

async fn start_signal(mount: &SignalledMount) -> fs::change_signal::ChangeSignalTask {
    let (signal, namespace, availability, path, notifier) = (
        mount.signal.clone(),
        mount.namespace.clone(),
        mount.availability.clone(),
        mount.dir.path().to_path_buf(),
        mount.session.notifier(),
    );
    let runtime = tokio::runtime::Handle::current();
    tokio::task::spawn_blocking(move || {
        signal.start(namespace, Some(availability), &path, notifier, &runtime)
    })
    .await
    .unwrap()
    .expect("the first signal goes through")
}

type WatchEvents = std::sync::mpsc::Receiver<Vec<tilo_files::native::WatchEvent>>;

fn watch_folder(dir: &std::path::Path) -> (tilo_files::native::ChangeWatch, WatchEvents) {
    let (events_tx, events) = std::sync::mpsc::channel();
    let watch = tilo_files::native::Watcher::entries(dir)
        .start(move |batch| {
            let _ = events_tx.send(batch.to_vec());
        })
        .unwrap();
    (watch, events)
}

fn next_change(
    events: &WatchEvents,
) -> Result<Vec<tilo_files::native::WatchEvent>, std::sync::mpsc::RecvTimeoutError> {
    let left = tilo_test_env::hang_deadline().saturating_duration_since(std::time::Instant::now());
    events.recv_timeout(left)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_merged_change_reaches_inotify_on_its_directory() {
    use tilo_files::native::CHANGE_SIGNAL_XATTR;

    require_fuse!();
    let mount = signalled_mount().await;
    let docs = mount.dir.path().join("docs");
    let read_signal = |path: &std::path::Path| {
        let mut value = [0_u8; 8];
        rustix::fs::getxattr(path, CHANGE_SIGNAL_XATTR, &mut value[..])
            .map(|len| value[..len].to_vec())
    };
    assert_eq!(
        read_signal(&docs),
        Err(rustix::io::Errno::NODATA),
        "no folder claims to signal before a signal went through"
    );

    let task = start_signal(&mount).await;
    assert_eq!(
        read_signal(&docs),
        Ok(b"1".to_vec()),
        "the mount says it signals"
    );
    assert_eq!(
        rustix::fs::setxattr(
            &docs,
            CHANGE_SIGNAL_XATTR,
            b"guessed",
            rustix::fs::XattrFlags::empty()
        ),
        Err(rustix::io::Errno::PERM),
        "nobody else raises the signal"
    );

    assert!(list(&docs).unwrap().is_empty());
    let (watcher, events) = watch_folder(&docs);

    let mut peer = Namespace::from_bytes("peer", &mount.namespace.lock().unwrap().save()).unwrap();
    let peer_docs = peer.resolve_path_ino("/docs").unwrap();
    peer.create_file(peer_docs, "from-peer.md", None, 0, 2)
        .unwrap();
    mount
        .namespace
        .lock()
        .unwrap()
        .merge_doc(&mut peer)
        .unwrap();

    let (event, events) = tokio::task::spawn_blocking(move || (next_change(&events), events))
        .await
        .unwrap();
    let event = event.expect("inotify reports the peer's change");
    assert!(
        event.iter().any(|event| event.path == docs),
        "the event names the changed folder: {event:?}"
    );
    assert_eq!(list(&docs), Ok(vec!["from-peer.md".to_string()]));

    tokio::time::sleep(Duration::from_secs(1)).await;
    let late: Vec<_> = events.try_iter().collect();
    assert!(late.is_empty(), "an idle mount raises no signal: {late:?}");

    drop(watcher);
    drop(task);
    let SignalledMount {
        session,
        endpoint,
        store,
        dir,
        ..
    } = mount;
    shutdown_fuse_fixture(session, dir.path(), endpoint, store).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_availability_change_reaches_inotify_on_its_directory() {
    use fs::availability::{RecordAvailability, RecordKind, TransferError};

    require_fuse!();
    let mount = signalled_mount().await;
    let docs = mount.dir.path().join("docs");
    let record = {
        let mut ns = mount.namespace.lock().unwrap();
        let docs_ino = ns.resolve_path_ino("/docs").unwrap();
        let missing = iroh_blobs::Hash::from([9; 32]);
        let ino = ns
            .create_file(docs_ino, "report.pdf", Some(&missing), 4, 1)
            .unwrap();
        ns.record_id_for(ino).unwrap()
    };
    let task = start_signal(&mount).await;

    assert_eq!(list(&docs), Ok(vec!["report.pdf".to_string()]));
    let report = docs.join("report.pdf");
    let availability = || {
        let mut value = vec![0_u8; usize::from(u16::MAX)];
        let len = rustix::fs::getxattr(&report, fs_protocol::AVAILABILITY_XATTR, &mut value[..])
            .expect("the mount projects the file's availability");
        fs_protocol::AvailabilityProjectionV1::parse_xattr(&value[..len]).unwrap()
    };
    assert!(!availability().unavailable());
    let (watcher, events) = watch_folder(&docs);

    let mut unavailable = RecordAvailability::new(record, RecordKind::File);
    unavailable.transfer.last_error = Some(TransferError {
        message: fs_protocol::NO_PEER_DELIVERED_ERROR.into(),
        occurred_at_ms: 12,
    });
    mount.availability.lock().unwrap().put(unavailable).unwrap();

    let (event, _events) = tokio::task::spawn_blocking(move || (next_change(&events), events))
        .await
        .unwrap();
    let event = event.expect("inotify reports the file's changed availability");
    assert!(
        event.iter().any(|event| event.path == docs),
        "the event names the file's folder: {event:?}"
    );
    assert!(
        availability().unavailable(),
        "the folder's watcher reads the file as unavailable"
    );

    drop(watcher);
    drop(task);
    let SignalledMount {
        session,
        endpoint,
        store,
        dir,
        ..
    } = mount;
    shutdown_fuse_fixture(session, dir.path(), endpoint, store).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_file_read_whole_is_stored_and_its_folder_hears_of_it() {
    use fs_protocol::Residency;

    require_fuse!();
    let mount = signalled_mount().await;
    let data: Vec<u8> = (0..3 * 1024 * 1024_u32)
        .map(|i| u8::try_from(i.wrapping_mul(2_654_435_761) >> 24).unwrap())
        .collect();
    let len = data.len() as u64;
    let keeper_store = Arc::new(ContentStore::memory());
    let hash = keeper_store.write_content(&data).await.unwrap().unwrap();
    let keeper = test_relay::direct_endpoint().await;
    let router = iroh::protocol::Router::builder(keeper.clone())
        .accept(iroh_blobs::ALPN, keeper_store.blobs_protocol(None))
        .spawn();
    mount.hydrator.add_peer(keeper.addr());
    let (record, version) = {
        let mut ns = mount.namespace.lock().unwrap();
        let docs_ino = ns.resolve_path_ino("/docs").unwrap();
        let ino = ns
            .create_file(docs_ino, "film.mkv", Some(&hash), len, 1)
            .unwrap();
        let record = ns.record_id_for(ino).unwrap();
        let revision = fs_protocol::ContentRevisionV1 {
            schema_version: fs_protocol::CONTENT_REVISION_VERSION,
            record_id: record,
            version_id: fs_protocol::FileVersionId::new(),
            content_hash: Some(fs_protocol::ContentHash(*hash.as_bytes())),
            byte_len: len,
            metadata_hash: fs_protocol::ContentHash([7; 32]),
        };
        assert!(ns.set_content_revision(ino, &revision, 1));
        drop(ns);
        (record, revision.version_id)
    };
    let task = start_signal(&mount).await;

    let docs = mount.dir.path().join("docs");
    let film = docs.join("film.mkv");
    let availability = |path: &std::path::Path| {
        let mut value = vec![0_u8; usize::from(u16::MAX)];
        let len = rustix::fs::getxattr(path, fs_protocol::AVAILABILITY_XATTR, &mut value[..])
            .expect("the mount projects the file's availability");
        fs_protocol::AvailabilityProjectionV1::parse_xattr(&value[..len]).unwrap()
    };
    assert_eq!(list(&docs), Ok(vec!["film.mkv".to_string()]));
    assert_eq!(availability(&film).residency, Residency::NotResident);
    let (watcher, events) = watch_folder(&docs);

    let read = {
        let film = film.clone();
        tokio::task::spawn_blocking(move || std::fs::read(film))
            .await
            .unwrap()
            .expect("the keeper serves the file")
    };
    assert!(read == data, "the file's bytes");

    let (event, _events) = tokio::task::spawn_blocking(move || (next_change(&events), events))
        .await
        .unwrap();
    let event = event.expect("inotify reports that the file is stored");
    assert!(
        event.iter().any(|event| event.path == docs),
        "the event names the file's folder: {event:?}"
    );
    let stored = mount
        .availability
        .lock()
        .unwrap()
        .get(record)
        .cloned()
        .expect("the read that completed the file wrote its record");
    assert_eq!(stored.residency, Residency::Resident { bytes: len });
    assert_eq!(stored.current_version, Some(version));
    assert_eq!(
        availability(&film).residency,
        Residency::Resident { bytes: len },
        "the folder's watcher reads the file as stored"
    );

    drop(watcher);
    drop(task);
    router.shutdown().await.unwrap();
    let SignalledMount {
        session,
        endpoint,
        store,
        dir,
        ..
    } = mount;
    shutdown_fuse_fixture(session, dir.path(), endpoint, store).await;
}

async fn mount_owned_by(
    owner: Option<(u32, u32)>,
    allow_other: bool,
) -> (
    tempfile::TempDir,
    fuser::BackgroundSession,
    Endpoint,
    Arc<ContentStore>,
) {
    let dir = if allow_other {
        let mut builder = tempfile::Builder::new();
        builder.prefix("tilo-fs-callers-");
        builder.permissions(std::fs::Permissions::from_mode(0o755));
        builder.tempdir_in("/tmp")
    } else {
        tempfile::tempdir()
    }
    .expect("mount temp dir");
    let endpoint = Endpoint::bind(presets::Minimal)
        .await
        .expect("bind endpoint");
    let store = Arc::new(ContentStore::memory());
    let namespace = Arc::new(Mutex::new(Namespace::new("owner-peer").unwrap()));
    namespace
        .lock()
        .unwrap()
        .create_dir(ROOT_INO, ".config", 1)
        .unwrap();
    let hydrator = Arc::new(Hydrator::new(endpoint.clone(), store.clone()));
    let heuristics = Arc::new(Heuristics::new(hydrator.clone()));
    let fs = QuickFS::new(namespace, hydrator, heuristics).with_owner(owner);
    let mut config = Config::default();
    config.mount_options = vec![MountOption::FSName("quickfs_owner".to_string())];
    if allow_other {
        config.acl = fuser::SessionACL::All;
    }
    let session = fuser::spawn_mount(fs, dir.path(), &config).expect("spawn FUSE session");
    wait_for_mount(dir.path()).await;
    (dir, session, endpoint, store)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_workspace_is_presented_as_the_persons_or_the_callers() {
    require_fuse!();
    tilo_test_env::within("workspace ownership is presented", async {
        let me = tempdir().unwrap();
        let (uid, gid) = {
            let meta = std::fs::metadata(me.path()).unwrap();
            (meta.uid(), meta.gid())
        };
        let root = uid == 0;

        let (dir, session, endpoint, store) = mount_owned_by(None, root).await;
        let home = dir.path();
        if root {
            if std::path::Path::new("/usr/bin/setpriv").is_file() {
                let stat_as = |id: u32| {
                    let out = std::process::Command::new("/usr/bin/setpriv")
                        .args(["--reuid", &id.to_string(), "--regid", &id.to_string()])
                        .args(["--clear-groups", "stat", "-c", "%u:%g"])
                        .arg(home)
                        .arg(home.join(".config"))
                        .output_locked()
                        .expect("setpriv runs");
                    assert!(
                        out.status.success(),
                        "{}",
                        String::from_utf8_lossy(&out.stderr)
                    );
                    String::from_utf8_lossy(&out.stdout).into_owned()
                };
                for id in [65534, 65533, 0, 65534] {
                    assert_eq!(
                        stat_as(id),
                        format!("{id}:{id}\n{id}:{id}\n"),
                        "caller {id}"
                    );
                }
            }
        } else {
            if let Some(bwrap) = ["/usr/bin/bwrap", "/bin/bwrap"]
                .into_iter()
                .find(|p| std::path::Path::new(p).is_file())
            {
                let out = std::process::Command::new(bwrap)
                    .args([
                        "--unshare-user",
                        "--uid",
                        &uid.to_string(),
                        "--gid",
                        &gid.to_string(),
                    ])
                    .args(["--ro-bind", "/", "/", "--dev", "/dev", "--proc", "/proc"])
                    .arg("--bind")
                    .arg(home)
                    .arg("/var/workspace")
                    .args(["--setenv", "HOME", "/var/workspace", "--chdir", "/"])
                    .args(["sh", "-c", r#"stat -c %U "$HOME" "$HOME/.config"; id -un"#])
                    .output_locked()
                    .expect("bwrap runs");
                let text = String::from_utf8_lossy(&out.stdout);
                assert!(
                    out.status.success(),
                    "{text}{}",
                    String::from_utf8_lossy(&out.stderr)
                );
                let lines: Vec<&str> = text.lines().collect();
                assert_eq!(lines.len(), 3, "{text}");
                assert_eq!(lines[0], lines[2], "~ is the guest user's: {text}");
                assert_eq!(lines[1], lines[2], "~/.config is the guest user's: {text}");
            }
            for path in [home.to_path_buf(), home.join(".config")] {
                let meta = std::fs::metadata(&path).unwrap();
                assert_eq!((meta.uid(), meta.gid()), (uid, gid), "{}", path.display());
            }
        }
        shutdown_fuse_fixture(session, dir.path(), endpoint, store).await;

        let (dir, session, endpoint, store) = mount_owned_by(Some((4242, 4343)), false).await;
        for path in [dir.path().to_path_buf(), dir.path().join(".config")] {
            let meta = std::fs::metadata(&path).unwrap();
            assert_eq!((meta.uid(), meta.gid()), (4242, 4343), "{}", path.display());
        }
        shutdown_fuse_fixture(session, dir.path(), endpoint, store).await;
    })
    .await;
}
