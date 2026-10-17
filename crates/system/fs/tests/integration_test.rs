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
    os::unix::fs::MetadataExt,
};

use collab_types::{Access as CapAccess, CapOverride, Ed25519PublicKey, Effect, Scope, Subject};
use fs::namespace::ROOT_INO;
use fs::{Access, ContentStore, Heuristics, Hydrator, Namespace, QuickFS, WorkspaceAcl};
use fuser::{Config, MountOption};
use iroh::endpoint::{Endpoint, presets};
use tempfile::tempdir;

// These integration tests mount a real FUSE filesystem, which needs
// `/dev/fuse` (+ fusermount3). Minimal CI containers don't have it, so
// `spawn_mount` fails with NotFound — skip cleanly there rather than
// panicking; this gates on capability, it does not weaken any assertion.
// The FUSE lane sets TILO_REQUIRE_FUSE=1, turning a missing device into a
// failure instead of a silently skipped contract.
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

/// Wait until the FUSE session answers at `mount`: the directory then reports
/// the mounted filesystem's device instead of its parent's, which needs the
/// session to have served a `getattr`. Replaces a fixed pause that was either
/// wasted time or, on a loaded machine, too short.
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

/// Finish every background owner before the per-test Tokio runtime exits.
///
/// `BackgroundSession::drop` only requests unmounting, while an unclosed Iroh
/// endpoint aborts its task set from `Drop`.  Awaiting these owners in order
/// prevents either teardown path from leaking into the next serialized test.
async fn shutdown_fuse_fixture(
    session: fuser::BackgroundSession,
    endpoint: Endpoint,
    store: Arc<ContentStore>,
) {
    fs::fuse_session::unmount_and_join(session).expect("unmount and join FUSE session");
    endpoint.close().await;
    store
        .store()
        .shutdown()
        .await
        .expect("shut down blob store");
}

/// Negotiates `FUSE_ABORT_ERROR`, so an aborted connection fails the
/// session's device read with `ECONNABORTED` on every abort rather than only
/// when the read races an unmount's abort.
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

/// The `fusectl` abort switch of the connection mounted at `mount`: its
/// directory is named by the kernel's own device number, `major << 20 |
/// minor` of the mount's device in `/proc/self/mountinfo`.
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

/// Unmounting aborts the kernel connection, and the session's blocked read
/// can lose the race with that abort: it then fails with `ECONNABORTED`, not
/// the usual `ENODEV`, and fuser hands that back as the session's error. The
/// race is in the kernel and cannot be scheduled from here, so the abort
/// error is negotiated and the connection aborted outright: the session
/// thread then always ends on `ECONNABORTED`, which teardown must read as
/// the end of the session.
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
    let ended = fs::fuse_session::unmount_and_join(session);
    // An aborted connection still holds its mount point; detach it so the
    // temporary directory can go.
    let _ = std::process::Command::new("fusermount3")
        .args(["-u", "-q", "-z", "--"])
        .arg(&mount)
        .status();
    tilo_test_env::require_capability!(
        FuseControl,
        abort.is_some(),
        "aborting the connection needs fusectl mounted at /sys/fs/fuse/connections"
    );
    ended.expect("an aborted connection ends the session cleanly");
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

// Capability enforcement: the capability model in `collab_types` is the
// system's one permission vocabulary. `WorkspaceAcl::from_capabilities`
// binds a capability list to a mount identity; these tests prove the
// binding *denies* through `QuickFS::check_access` — the exact function
// `open`, `read` and `write` route every access through.

/// A workspace tree with a granted subtree, a sibling subtree, and a
/// path that shares a *string* prefix with the granted one but is not
/// beneath it. Returns the inode of each interesting path.
struct CapFixture {
    fs: QuickFS,
    endpoint: Endpoint,
    store: Arc<ContentStore>,
    /// `/w/docs` — the granted subtree itself.
    docs: u64,
    /// `/w/docs/report.md` — beneath the granted subtree.
    docs_child: u64,
    /// `/w/other/private.md` — a sibling subtree, never granted.
    other_child: u64,
    /// `/w/docsSECRET/leak.md` — shares the string prefix `/w/docs`, but is
    /// a different directory.
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

/// [`cap_fixture`] over an ACL handle someone else owns — a
/// [`fs::CapabilityFile`], for tests that swap the policy while the same
/// `QuickFS` stays live.
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
        // Same string prefix as "/w/docs", different directory.
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

/// A read grant for `who` on `path`.
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
    // Phase `a_capability_on_one_subtree_denies_a_sibling_subtree`.
    // A capability names a subtree. Everything outside is denied — a
    // grant is not a foot in the door — and the denial is an absence:
    // `ENOENT`, the same answer a name that was never there gets.
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
    // Phase `a_capability_stops_at_the_path_component_boundary`.
    // The prefix is a *path* prefix, not a string prefix. `/w/docsSECRET`
    // merely starts with the same characters; treating it as covered
    // would leak a whole tree.
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
    // Phase `a_read_capability_is_denied_a_write`.
    // `CapAccess::Read` does not imply `CapAccess::Write`. A share that says "read"
    // must not become a write when it reaches the filesystem.
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
    // Phase `an_expired_capability_authorises_nothing`.
    // An expired capability is gone, not merely stale — evaluated past
    // its deadline it authorises nothing.
    {
        let expired = read_cap(ALICE, "/w/docs").with_expiry(1_000);

        // Before the deadline the same capability allows the read, so the
        // denial below is expiry biting, not an unrelated failure.
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
    // Phase `an_identity_with_no_capability_is_denied_everything`.
    // The fail-closed floor: an identity holding nothing gets nothing —
    // including the mount root, and for capabilities naming somebody else.
    {
        // Bob's mount, evaluated against a list that grants only Alice.
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
        // The mount root is the one exception, deliberately: it must exist
        // or the mount looks broken. Bob gets an ordinary empty read-only
        // directory — no content, no error hinting he's missing anything.
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
    // Phase `a_grant_to_another_room_does_not_authorise_a_path`.
    // A room is not a key to the filesystem unless the mount proves it is in
    // that room: a grant to a room's members doesn't cover a mount actor
    // that presents no such room.
    {
        let room_cap = CapOverride::new(
            Subject::room("another-room"),
            Scope::FsPath("/w/docs".into()),
            Effect::Grant,
            CapAccess::Write,
        );
        let f = cap_fixture(ALICE, &[room_cap], Some(1_000)).await;

        assert_eq!(
            f.fs.check_access(f.docs_child, Access::Read),
            Err(libc::ENOENT),
            "a grant to another room's members must not authorise a path for this mount"
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
    // Phase `fuse_mount_enforces_identity_acl_end_to_end`.
    // Full kernel path: mount two real FUSE sessions over the same
    // namespace but bound to different identities, and prove the read is
    // served for the one holding the cap and denied for the one that
    // doesn't. Requires /dev/fuse.
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

            // ACL: only Bob may read /secret.txt. Alice holds nothing.
            let mut acl = WorkspaceAcl::new();
            acl.grant_read(BOB, "/secret.txt");

            // Alice's mount: read must be denied through the real kernel path.
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
            // …and it is absent from her listing, not present-and-refused.
            let alice_entries: Vec<String> = std::fs::read_dir(alice_dir.path())
                .expect("Alice can list her (empty) view")
                .map(|e| e.unwrap().file_name().into_string().unwrap())
                .collect();
            assert!(
                alice_entries.is_empty(),
                "Alice's view of the workspace must be an empty directory, got {alice_entries:?}"
            );

            // Bob's mount: read must succeed and return the real bytes.
            let bob_dir = tempdir().unwrap();
            let bob_fs = QuickFS::new(namespace.clone(), hydrator, heuristics).with_acl(acl, BOB);
            let mut cfg_b = Config::default();
            cfg_b.mount_options = vec![MountOption::FSName("quickfs_acl_bob".into())];
            let sb = fuser::spawn_mount(bob_fs, bob_dir.path(), &cfg_b).expect("spawn bob session");
            wait_for_mount(bob_dir.path()).await;

            let bob_read = std::fs::read(bob_dir.path().join("secret.txt")).expect("Bob may read");
            assert_eq!(bob_read, body, "Bob (holds cap) reads the real content");
            fs::fuse_session::unmount_and_join(sa).expect("unmount Alice session");
            fs::fuse_session::unmount_and_join(sb).expect("unmount Bob session");
            shutdown_endpoint.close().await;
            shutdown_store
                .store()
                .shutdown()
                .await
                .expect("shut down blob store");
        })
        .await;
    }
    // Phase `identity_acl_gates_the_real_fs_access_path`.
    // The per-identity ACL decision, exercised on the REAL fs access
    // method (`QuickFS::check_access`) — no FUSE mount needed, drives
    // the production path directly, so it runs everywhere.
    {
        // Helper: build a fresh QuickFS over the shared namespace bound to an
        // identity (each needs its own endpoint/hydrator).
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

        // A shared file, a secret subdir, and a file inside it.
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

        // The ACL: Alice may read /shared.txt only. Bob may read+write /secret/**.
        let mut acl = WorkspaceAcl::new();
        acl.grant_read(ALICE, "/shared.txt");
        acl.grant_read_write(BOB, "/secret");

        // Mount view A — bound to Alice's identity.
        let (alice_fs, alice_endpoint, alice_store) =
            mount_as(namespace.clone(), Some((acl.clone(), ALICE))).await;
        // Alice: read on the shared file → allowed; read on the secret dir
        // AND the secret child → denied (EACCES).
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
        // Alice cannot write the file she may only read.
        assert_eq!(
            alice_fs.check_access(shared_ino, Access::Write),
            Err(libc::EROFS),
            "Alice's grant is read-only, so the file is on a read-only mount"
        );

        // Mount view B — same namespace + ACL, bound to Bob's identity.
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

        // A mount with NO ACL attached keeps the compatibility veneer: every
        // access is allowed (the Unix layer alone governs).
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
    // Phase `fuse_mount_enforces_identity_acl_end_to_end` (defined above).
    fuse_mount_enforces_identity_acl_end_to_end().await;
    // Phase `a_grant_made_after_the_mount_reaches_the_live_access_check`.
    {
        use fs::CapabilityFile;

        let dir = tempdir().unwrap();
        let caps_path = dir.path().join("caps.json");
        put_caps(&caps_path, &[read_cap(ALICE, "/w/docs")]);

        let file = CapabilityFile::load(&caps_path, ALICE, Vec::new()).expect("loads");
        let f = cap_fixture_with(ALICE, file.handle()).await;

        // Reproduces the bug's precondition: the sibling subtree is denied.
        assert_eq!(
            f.fs.check_access(f.other_child, Access::Read),
            Err(libc::ENOENT),
            "the sibling subtree must be denied before it is granted, or the reload proves nothing"
        );
        assert_eq!(f.fs.check_access(f.docs_child, Access::Read), Ok(()));

        // The user grants it, an hour into the session. No remount.
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
    // Phase `a_revocation_after_the_mount_reaches_the_live_access_check`.
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

        // The user revokes it — the direction that matters most: a
        // revocation the filesystem ignores is a permission the user
        // believes they took away.
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
    // Phase `fuse_projects_tree_and_reads_real_content`.
    // Multi-thread runtime: the FUSE read path uses `block_in_place`,
    // which panics on a current-thread runtime.
    {
        require_fuse!();
        tilo_test_env::within("the projected FUSE tree reads", async {
            let mount_dir = tempdir().expect("mount temp dir");
            let mount_path = mount_dir.path().to_path_buf();

            let endpoint = Endpoint::bind(presets::Minimal)
                .await
                .expect("bind endpoint");

            // Ingest real content into the local store and seed the namespace with it.
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

            // Projection: the tree is visible.
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

            // Hydration: reading the file returns the real ingested bytes.
            let read_back = std::fs::read(mount_path.join("test_file.txt")).expect("read file");
            assert_eq!(read_back, body, "FUSE read must return the real content");
            shutdown_fuse_fixture(session, shutdown_endpoint, shutdown_store).await;
        })
        .await;
    }
    // Phase `fuse_write_create_overwrite_rename_unlink`.
    // Full read-write round-trip through the kernel: create, write,
    // overwrite, mkdir, rename, unlink.
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

                // create + write
                std::fs::write(path.join("hello.txt"), b"first").expect("write hello");
                assert_eq!(std::fs::read(path.join("hello.txt")).unwrap(), b"first");

                // overwrite (truncate + rewrite) with a longer body
                std::fs::write(path.join("hello.txt"), b"second longer content")
                    .expect("overwrite");
                assert_eq!(
                    std::fs::read(path.join("hello.txt")).unwrap(),
                    b"second longer content"
                );

                // mkdir + nested file
                std::fs::create_dir(path.join("d")).expect("mkdir");
                std::fs::write(path.join("d/n.txt"), b"nested").expect("write nested");
                assert_eq!(std::fs::read(path.join("d/n.txt")).unwrap(), b"nested");

                // rename
                std::fs::rename(path.join("hello.txt"), path.join("renamed.txt")).expect("rename");
                assert!(!path.join("hello.txt").exists(), "old name gone");
                assert_eq!(
                    std::fs::read(path.join("renamed.txt")).unwrap(),
                    b"second longer content"
                );

                // unlink
                std::fs::remove_file(path.join("renamed.txt")).expect("unlink");
                assert!(!path.join("renamed.txt").exists(), "unlinked file gone");
                shutdown_fuse_fixture(session, shutdown_endpoint, shutdown_store).await;
            },
        )
        .await;
    }
    // Phase `fuse_non_utf8_name_survives_rename_and_namespace_restart`.
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
            fs::fuse_session::unmount_and_join(session).unwrap();
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
            fs::fuse_session::unmount_and_join(session).unwrap();
            shutdown_endpoint.close().await;
            store.store().shutdown().await.unwrap();
        })
        .await;
    }
    // Phase `rmdir_rejects_nonempty_and_unlink_rejects_dir`.
    // rmdir must refuse a non-empty directory (no silent rm -rf), and
    // unlink must refuse a directory.
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

            // rmdir on a non-empty directory must fail and preserve contents.
            assert!(
                std::fs::remove_dir(path.join("dir")).is_err(),
                "rmdir of a non-empty dir must fail"
            );
            assert_eq!(
                std::fs::read(path.join("dir/inner.txt")).unwrap(),
                b"keepme",
                "contents must survive a failed rmdir"
            );

            // unlink on a directory must fail.
            assert!(
                std::fs::remove_file(path.join("dir")).is_err(),
                "unlink of a directory must fail"
            );

            // Empty it, then rmdir succeeds.
            std::fs::remove_file(path.join("dir/inner.txt")).expect("unlink inner");
            std::fs::remove_dir(path.join("dir")).expect("rmdir empty dir");
            assert!(!path.join("dir").exists(), "empty dir removed");
            shutdown_fuse_fixture(session, shutdown_endpoint, shutdown_store).await;
        })
        .await;
    }
    // Phase `fuse_document_roundtrip_and_opaque_still_works`.
    // A recognized document format is stored as a CRDT and round-trips
    // through the mount; unknown formats stay opaque bytes.
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

            // A markdown document: recognized, stored as a CRDT, round-trips.
            std::fs::write(path.join("note.md"), b"# Title\n\nHello world\n").expect("write md");
            let back = std::fs::read_to_string(path.join("note.md")).expect("read md");
            assert!(
                back.contains("Title") && back.contains("Hello world"),
                "document round-trip lost content: {back:?}"
            );

            // An unknown format stays opaque bytes (exact round-trip).
            std::fs::write(path.join("data.bin"), b"\x00\x01raw-bytes\xff").expect("write bin");
            assert_eq!(
                std::fs::read(path.join("data.bin")).unwrap(),
                b"\x00\x01raw-bytes\xff"
            );
            shutdown_fuse_fixture(session, shutdown_endpoint, shutdown_store).await;
        })
        .await;
    }
    // Phase `getxattr_reports_projected_vs_local_sync_state`.
    // The sync-state interface end-to-end: a projected file reports
    // `projected` via getxattr, a hydrated file reports `local`,
    // directories are `local` — the path the file manager reads for sync badges.
    {
        require_fuse!();
        tilo_test_env::within("the FUSE sync-state xattrs", async {
            let mount_dir = tempdir().expect("mount temp dir");
            let path = mount_dir.path().to_path_buf();

            let endpoint = Endpoint::bind(presets::Minimal)
                .await
                .expect("bind endpoint");
            let store = Arc::new(ContentStore::memory());

            // A file whose content IS ingested locally → hydrated → `local`.
            let body = b"content that is already here";
            let local_hash = store.write_content(body).await.unwrap().unwrap();

            // A file whose namespace entry carries a content hash the store
            // does NOT have — exactly what a freshly-joined peer projects before hydration.
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
            shutdown_fuse_fixture(session, shutdown_endpoint, shutdown_store).await;
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
    // Phase `fuse_replacement_rename_preserves_the_replaced_open_handle`.
    // POSIX replacement rename has two audiences: new path lookups must resolve
    // to the source inode, while a process that already holds the replaced inode
    // must keep reading that original object until its handle closes.
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
            shutdown_fuse_fixture(session, shutdown_endpoint, shutdown_store).await;
        })
        .await;
    }
    // Phase `fuse_unlink_and_recreate_keeps_the_old_open_inode_private`.
    // An unlinked inode belongs to its existing handles until the last close. A
    // new file may reuse the old pathname, but must never alias the old handle's
    // private buffer or inode identity.
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
            shutdown_fuse_fixture(session, shutdown_endpoint, shutdown_store).await;
        })
        .await;
    }
    // Phase `fuse_two_writable_handles_preserve_disjoint_writes`.
    // Writable handles share one live inode buffer. Closing or flushing either
    // handle must not commit a stale per-handle snapshot over the other writer.
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
            shutdown_fuse_fixture(session, shutdown_endpoint, shutdown_store).await;
        })
        .await;
    }
    // Phase `signed_fuse_replacement_is_transactional_and_preserves_open_handles`.
    // The production signed path must preserve the same kernel contract while
    // atomically committing source history, victim tombstone, repository records,
    // namespace persistence, and the recovery journal.
    {
        require_fuse!();
        tilo_test_env::within("the signed real-FUSE replacement test", async {
            let mount_dir = tempdir().expect("mount temp dir");
            let state_dir = tempdir().expect("state temp dir");
            let endpoint = Endpoint::bind(presets::Minimal)
                .await
                .expect("bind endpoint");
            let store = Arc::new(ContentStore::memory());
            let source_hash = store
                .write_content(b"signed source")
                .await
                .unwrap()
                .unwrap();
            let victim_hash = store
                .write_content(b"signed victim")
                .await
                .unwrap()
                .unwrap();
            let namespace = Arc::new(Mutex::new(Namespace::new("signed-fuse-replace").unwrap()));
            let (source_ino, victim_ino, source_record, victim_record) = {
                let mut namespace = namespace.lock().unwrap();
                let source_ino = namespace
                    .create_file(ROOT_INO, "source.bin", Some(&source_hash), 13, 1)
                    .unwrap();
                let victim_ino = namespace
                    .create_file(ROOT_INO, "destination.bin", Some(&victim_hash), 13, 1)
                    .unwrap();
                (
                    source_ino,
                    victim_ino,
                    namespace.record_id_for(source_ino).unwrap(),
                    namespace.record_id_for(victim_ino).unwrap(),
                )
            };
            let workspace_id = uuid::Uuid::new_v4();
            let signer = ed25519_dalek::SigningKey::from_bytes(&[61; 32]);
            let person = ed25519_dalek::SigningKey::from_bytes(&[62; 32]);
            let actor = fs_protocol::FileActor {
                person: Ed25519PublicKey::from_bytes(person.verifying_key().to_bytes()),
                device: Ed25519PublicKey::from_bytes(signer.verifying_key().to_bytes()),
            };
            let history = Arc::new(
                fs::FileHistoryStore::open(state_dir.path().join("history.json")).unwrap(),
            );
            let repository =
                Arc::new(fs::RecordRepository::open(state_dir.path().join("records")).unwrap());
            let publisher = Arc::new(
                fs::RecordPublisher::new(repository.clone(), workspace_id, actor, signer).unwrap(),
            );
            let journal = Arc::new(Mutex::new(
                fs::RecordTransactionJournal::open(state_dir.path().join("transactions")).unwrap(),
            ));
            let namespace_path = state_dir.path().join("namespace.bin");
            let shutdown_endpoint = endpoint.clone();
            let shutdown_store = store.clone();
            let hydrator = Arc::new(Hydrator::new(endpoint, store));
            let heuristics = Arc::new(Heuristics::new(hydrator.clone()));
            let quick_fs = QuickFS::new(namespace.clone(), hydrator, heuristics)
                .with_history(history.clone(), actor)
                .unwrap()
                .with_record_publisher(publisher.clone())
                .unwrap()
                .with_replacement_transactions(journal.clone(), namespace_path.clone())
                .unwrap();
            let mut config = Config::default();
            config.mount_options = vec![MountOption::FSName("quickfs_signed_replace".to_string())];
            let session = fuser::spawn_mount(quick_fs, mount_dir.path(), &config)
                .expect("spawn signed FUSE session");
            wait_for_mount(mount_dir.path()).await;

            let source = mount_dir.path().join("source.bin");
            let destination = mount_dir.path().join("destination.bin");
            assert_eq!(std::fs::metadata(&source).unwrap().ino(), source_ino);
            assert_eq!(std::fs::metadata(&destination).unwrap().ino(), victim_ino);
            let mut victim_handle = std::fs::File::open(&destination).unwrap();
            std::fs::rename(&source, &destination).expect("signed replacement rename");
            assert_eq!(std::fs::metadata(&destination).unwrap().ino(), source_ino);
            assert_eq!(std::fs::read(&destination).unwrap(), b"signed source");
            let mut old_bytes = Vec::new();
            victim_handle.read_to_end(&mut old_bytes).unwrap();
            assert_eq!(old_bytes, b"signed victim");

            let source_snapshot = publisher.snapshot(source_record).unwrap().unwrap();
            let victim_snapshot = publisher.snapshot(victim_record).unwrap().unwrap();
            assert_eq!(source_snapshot.binding.unwrap().name, "destination.bin");
            assert!(victim_snapshot.binding.unwrap().tombstoned);
            assert!(journal.lock().unwrap().entries().next().is_none());
            assert!(namespace_path.exists());
            assert!(history.snapshot(source_record).unwrap().versions.len() >= 2);
            assert!(history.snapshot(victim_record).unwrap().versions.len() >= 2);
            drop(victim_handle);
            shutdown_fuse_fixture(session, shutdown_endpoint, shutdown_store).await;
        })
        .await;
    }
}

/// Read the `user.tilo.sync` extended attribute of `path` through the
/// kernel (the exact `getxattr` a file manager issues), or `None`.
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

// A grant made after the mount is up. The capability list was read
// once, at mount time, and copied into the filesystem by value.
// `/var/workspace` stays mounted for the whole session, so "a later
// grant needs a remount" meant the compositor could show a
// capability the filesystem wasn't honouring, in either direction.
// These drive `QuickFS::check_access` on one `QuickFS` never rebuilt.
//
// A denied path answers `ENOENT`, not `EACCES` — indistinguishable
// from absent. What these tests turn on is that the answer *changes*
// when the list is re-read.

/// Write `caps` to `path` as the file authority the mount reads.
fn put_caps(path: &std::path::Path, caps: &[CapOverride]) {
    std::fs::write(
        path,
        serde_json::to_vec(&collab_types::FsAuthority::from(caps.to_vec())).unwrap(),
    )
    .unwrap();
    // Make the ordering of two writes unambiguous regardless of the
    // filesystem's mtime resolution.
    std::thread::sleep(Duration::from_millis(10));
}

// Denial is an absence: the property under test is not "the read
// fails" (proven above) but that a sandboxed app **cannot tell the
// difference** between an ungranted path and a nonexistent one.
// Everything below drives a real FUSE mount through the kernel, so
// what's asserted is what an ordinary program (`ls`, `open`, `stat`)
// actually observes.

/// A mounted, ACL-filtered workspace, deliberately lopsided: one granted
/// subtree, two hidden top-level directories, and a hidden sibling
/// sharing a string prefix with the granted one.
///
/// ```text
/// /w/docs/report.md        granted (read)
/// /w/docs/sub/deep.md      granted (read)
/// /w/other/private.md      never granted
/// /w/docsSECRET/leak.md    never granted, string-prefix look-alike
/// /vault/keys.txt          never granted
/// /archive/old.txt         never granted
/// ```
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

    /// The CRDT heads of the shared namespace — a fingerprint of the whole
    /// document. Equal heads before and after prove the operation changed
    /// nothing, not merely that it reported an error.
    fn heads(&self) -> Vec<automerge::ChangeHash> {
        self.namespace.lock().unwrap().heads()
    }

    async fn shutdown(self) {
        shutdown_fuse_fixture(self.session, self.endpoint, self.store).await;
    }
}

/// Populate the tree above and mount it as `identity` under `acl`.
/// `acl: None` mounts unfiltered, for side-by-side comparisons.
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

/// An ACL granting `ALICE` read on `/w/docs` and nothing else.
fn docs_read_only() -> (WorkspaceAcl, [u8; 32]) {
    (
        WorkspaceAcl::from_capabilities(ALICE, &[read_cap(ALICE, "/w/docs")], &[], None),
        ALICE,
    )
}

/// Sorted names in a directory, or the errno if it could not be listed.
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

/// The raw errno of a `stat`, or `None` if it succeeded.
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
    // Phase `a_denied_subtree_is_absent_from_the_listing`.
    // A path the identity was not granted does not appear in the listing
    // — not an entry that errors on access, it's not in the directory.
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

        // The same tree, unfiltered, does contain all of it — so the
        // omissions above are the filter, not an empty fixture.
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
    // Phase `a_hidden_path_answers_exactly_as_an_absent_one`.
    // The headline property: a hidden path and a path that was never
    // created must produce the *same* errno from every operation an app
    // can reach them with — otherwise the filesystem is an oracle for
    // what the user is hiding.
    {
        require_fuse!();
        let f = view_fixture(Some(docs_read_only()), "quickfs_view_oracle").await;
        let p = f.path();

        // Left column: exists in the tree, outside the view. Right column:
        // never existed. Same parent directory in each pair.
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

        // None of that probing may have touched the namespace.
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
    // Phase `directory_link_counts_match_the_filtered_listing`.
    // A directory's link count is `2 + subdirectories`, taken from the
    // filtered listing rather than the real tree — else it would
    // announce how many subdirectories are hidden.
    {
        use std::os::unix::fs::MetadataExt;

        require_fuse!();

        let f = view_fixture(Some(docs_read_only()), "quickfs_view_nlink").await;
        let open = view_fixture(None, "quickfs_view_nlink_open").await;

        // Filtered: root has one visible subdirectory (`w`); `w` has one
        // (`docs`); `docs` has one (`sub`).
        for (rel, expect) in [("", 3u64), ("w", 3), ("w/docs", 3), ("w/docs/sub", 2)] {
            let md = std::fs::metadata(f.path().join(rel)).expect("stat in the filtered view");
            assert_eq!(
                md.nlink(),
                expect,
                "nlink of /{rel} must count only the subdirectories the listing shows"
            );
            assert_eq!(md.len(), 0, "a directory's size never encodes its contents");
        }

        // Unfiltered, the same directories report more — what makes the
        // numbers above meaningful rather than a constant.
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
    // Phase `a_read_grant_presents_a_read_only_filesystem`.
    // A read grant is a read-only mount, not a mount that refuses. Every
    // mutation answers EROFS, and the document is byte-for-byte
    // unchanged afterwards.
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
    // Phase `a_write_without_read_entry_refuses_rather_than_clobbering`.
    // The one residual: an entry that's writable but not readable. The
    // capability model can't express it (every verb implies Read), so
    // this is reachable only through a direct ACL entry. Pinned here so
    // if it ever becomes reachable it's a deliberate change: the name is
    // refused, never created over, never confirmed.
    {
        require_fuse!();
        let mut acl = WorkspaceAcl::new();
        // Write on /w, but read only on /w/docs: /w/other and /w/docsSECRET
        // are writable and invisible at once, which no capability can produce.
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
        // Creating over the hidden name is refused — the hidden entry survives.
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
    // Phase `a_read_participant_reads_a_shared_document_and_cannot_author_it`.
    // A read participant reads the shared document and cannot author
    // it. The CRDT is byte-identical afterwards — not "the change was
    // rejected" but "no change was ever made".
    {
        require_fuse!();
        let acl = WorkspaceAcl::from_capabilities(ALICE, &[read_cap(ALICE, "/shared")], &[], None);
        let fixture = document_fixture(Some((acl, ALICE)), "quickfs_doc_ro").await;
        let doc = fixture.dir.path().join("shared/notes.md");

        // Participation: the document is there and it reads.
        let before = std::fs::read(&doc).expect("a read participant reads the shared document");
        assert!(
            String::from_utf8_lossy(&before).contains("Shared paragraph"),
            "the read must return the real document, not an empty stand-in: {:?}",
            String::from_utf8_lossy(&before)
        );
        let heads_before = fixture.namespace.lock().unwrap().heads();
        let nodes_before = fixture.namespace.lock().unwrap().live_node_ids();

        // Authorship: every route by which bytes could become a change.
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

        // Nothing was authored. Not "the change was refused" — the change
        // graph did not move.
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

        // The negative case is not vacuous: same document, same writes,
        // with a write grant instead of a read grant.
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

// Read participation without authorship: a shared document is a
// CRDT, and a change is *authored* by somebody. Read participation
// means the app sees the document and cannot originate a change —
// not "rejected", which would still need something downstream to reject it.
//
// In the filesystem this is structural: the only route from an app's
// bytes into the CRDT is a write buffer, only ever created by
// `write`, `create` or a truncating `setattr`, and all three are
// refused for a path the identity may not write.

/// A mounted workspace holding one CRDT-backed document under a granted
/// subtree, plus the ACL the mount enforces.
struct DocumentFixture {
    session: fuser::BackgroundSession,
    endpoint: Endpoint,
    store: Arc<ContentStore>,
    dir: tempfile::TempDir,
    namespace: Arc<Mutex<Namespace>>,
}

impl DocumentFixture {
    async fn shutdown(self) {
        shutdown_fuse_fixture(self.session, self.endpoint, self.store).await;
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
