use std::sync::{Arc, Mutex};
use std::time::Duration;

use collab_types::{CapOverride, Ed25519PublicKey, Effect, Role, Scope, Subject, Verb};
use fs::namespace::ROOT_INO;
use fs::{Access, ContentStore, Heuristics, Hydrator, Namespace, QuickFS, WorkspaceAcl};
use fuser::{Config, MountOption};
use iroh::endpoint::{Endpoint, presets};
use tempfile::tempdir;
use tokio::time::timeout;

// These integration tests mount a real FUSE filesystem, which needs `/dev/fuse`
// (+ fusermount3). Minimal CI containers don't have it, so `spawn_mount2` fails
// with NotFound. Skip cleanly there rather than panicking. When FUSE IS present
// (dev machines, the VM) the test runs in full — this gates on capability, it
// does not weaken any assertion.
fn fuse_available() -> bool {
    std::path::Path::new("/dev/fuse").exists()
}
macro_rules! require_fuse {
    () => {
        if !fuse_available() {
            eprintln!("skipping: requires /dev/fuse + fusermount3");
            return;
        }
    };
}

const ALICE: [u8; 32] = [0xA1; 32];
const BOB: [u8; 32] = [0xB0; 32];

// ---------------------------------------------------------------------
// Capability enforcement
//
// The capability model in `collab_types` is the system's one permission
// vocabulary. `WorkspaceAcl::from_capabilities` binds a capability list
// to a mount identity; these tests prove the binding *denies* through
// `QuickFS::check_access` — the exact function `open`, `read` and `write`
// route every access through. Nothing here pokes at ACL internals.
// ---------------------------------------------------------------------

/// A workspace tree with a granted subtree, a sibling subtree, and a
/// path that shares a *string* prefix with the granted one but is not
/// beneath it. Returns the inode of each interesting path.
struct CapFixture {
    fs: QuickFS,
    /// `/w/docs` — the granted subtree itself.
    docs: u64,
    /// `/w/docs/report.md` — beneath the granted subtree.
    docs_child: u64,
    /// `/w/other/private.md` — a sibling subtree, never granted.
    other_child: u64,
    /// `/w/docsSECRET/leak.md` — shares the string prefix `/w/docs`,
    /// but is a different directory.
    look_alike_child: u64,
}

async fn cap_fixture(identity: [u8; 32], caps: &[CapOverride], now: Option<u64>) -> CapFixture {
    let endpoint = Endpoint::bind(presets::N0).await.expect("bind endpoint");
    let store = Arc::new(ContentStore::memory());
    let body = b"contents";
    let hash = store.write_content(body).await.unwrap().unwrap();
    let namespace = Arc::new(Mutex::new(Namespace::new("cap-peer")));

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
        (docs, docs_child, other_child, look_alike_child)
    };

    let hydrator = Arc::new(Hydrator::new(endpoint, store));
    let heuristics = Arc::new(Heuristics::new(hydrator.clone(), namespace.clone()));
    let acl = WorkspaceAcl::from_capabilities(identity, caps, &[], now);
    let fs = QuickFS::new(namespace, hydrator, heuristics).with_acl(acl, identity);

    CapFixture {
        fs,
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
        Verb::Read,
    )
}

// A capability names a subtree. Everything outside that subtree is
// denied — a grant is not a foot in the door.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_capability_on_one_subtree_denies_a_sibling_subtree() {
    let f = cap_fixture(ALICE, &[read_cap(ALICE, "/w/docs")], Some(1_000)).await;

    assert_eq!(
        f.fs.check_access(f.docs_child, Access::Read),
        Ok(()),
        "the granted subtree is readable"
    );
    assert_eq!(
        f.fs.check_access(f.other_child, Access::Read),
        Err(libc::EACCES),
        "a capability on /w/docs must NOT reach /w/other/private.md"
    );
    assert_eq!(
        f.fs.check_access(f.other_child, Access::Write),
        Err(libc::EACCES),
        "nor may it write there"
    );
}

// The prefix is a *path* prefix, not a string prefix. `/w/docsSECRET`
// is a different directory that merely starts with the same characters;
// treating it as covered would leak a whole tree.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_capability_stops_at_the_path_component_boundary() {
    let f = cap_fixture(ALICE, &[read_cap(ALICE, "/w/docs")], Some(1_000)).await;

    assert_eq!(
        f.fs.check_access(f.docs, Access::Read),
        Ok(()),
        "the granted directory itself is readable"
    );
    assert_eq!(
        f.fs.check_access(f.look_alike_child, Access::Read),
        Err(libc::EACCES),
        "/w/docs must NOT cover /w/docsSECRET/leak.md — component boundary"
    );
}

// `Verb::Read` does not imply `Verb::Write`. A share that says "read"
// must not become a write when it reaches the filesystem.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_read_capability_is_denied_a_write() {
    let f = cap_fixture(ALICE, &[read_cap(ALICE, "/w/docs")], Some(1_000)).await;

    assert_eq!(
        f.fs.check_access(f.docs_child, Access::Read),
        Ok(()),
        "read is what was granted"
    );
    assert_eq!(
        f.fs.check_access(f.docs_child, Access::Write),
        Err(libc::EACCES),
        "a read-only capability must be denied a write"
    );
}

// An expired capability is gone, not merely stale. Evaluated at a later
// instant than its deadline it authorises nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_expired_capability_authorises_nothing() {
    let expired = read_cap(ALICE, "/w/docs").with_expiry(1_000);

    // Before the deadline the very same capability allows the read, so
    // the denial below is expiry biting and not an unrelated failure.
    let live = cap_fixture(ALICE, std::slice::from_ref(&expired), Some(999)).await;
    assert_eq!(
        live.fs.check_access(live.docs_child, Access::Read),
        Ok(()),
        "the capability allows the read while it is live"
    );

    let lapsed = cap_fixture(ALICE, &[expired], Some(1_001)).await;
    assert_eq!(
        lapsed.fs.check_access(lapsed.docs_child, Access::Read),
        Err(libc::EACCES),
        "a capability whose AtUnix deadline has passed must be denied"
    );
    assert_eq!(
        lapsed.fs.check_access(lapsed.docs, Access::Read),
        Err(libc::EACCES),
        "including on the granted directory itself"
    );
}

// The fail-closed floor: an identity holding nothing gets nothing —
// including the mount root, and including for capabilities that name
// somebody else.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_identity_with_no_capability_is_denied_everything() {
    // Bob's mount, evaluated against a list that grants only Alice.
    let f = cap_fixture(BOB, &[read_cap(ALICE, "/w/docs")], Some(1_000)).await;

    for (ino, what) in [
        (f.docs, "/w/docs"),
        (f.docs_child, "/w/docs/report.md"),
        (f.other_child, "/w/other/private.md"),
        (f.look_alike_child, "/w/docsSECRET/leak.md"),
        (ROOT_INO, "/"),
    ] {
        assert_eq!(
            f.fs.check_access(ino, Access::Read),
            Err(libc::EACCES),
            "an identity with no capability must be denied read on {what}"
        );
        assert_eq!(
            f.fs.check_access(ino, Access::Write),
            Err(libc::EACCES),
            "an identity with no capability must be denied write on {what}"
        );
    }
}

// A role is not a key to the filesystem. `role_base_applies` is false
// for every `ResourceRef::FsPath`, so even an Owner reaches a path only
// through an explicit capability — and a role-scoped grant does not
// cover a mount actor, which holds no role.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_role_wide_grant_does_not_authorise_a_path() {
    let role_cap = CapOverride::new(
        Subject::Role(Role::Owner),
        Scope::FsPath("/w/docs".into()),
        Effect::Grant,
        Verb::Write,
    );
    let f = cap_fixture(ALICE, &[role_cap], Some(1_000)).await;

    assert_eq!(
        f.fs.check_access(f.docs_child, Access::Read),
        Err(libc::EACCES),
        "an Owner-role grant must not authorise a path for a mount that holds no role"
    );
}

// The per-identity ACL enforcement decision, exercised on the REAL fs
// access method (`QuickFS::check_access`, the exact function every
// open/read/write routes through). No FUSE mount needed — this drives
// the production path directly (namespace path resolution + ACL
// evaluation + the mount's bound identity), so it runs everywhere, not
// only where /dev/fuse exists.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn identity_acl_gates_the_real_fs_access_path() {
    let store = Arc::new(ContentStore::memory());
    let body = b"top secret";
    let hash = store.write_content(body).await.unwrap().unwrap();

    // A shared file, a secret subdir, and a file inside it.
    let namespace = Arc::new(Mutex::new(Namespace::new("acl-peer")));
    let (shared_ino, secret_dir_ino, secret_child_ino) = {
        let mut ns = namespace.lock().unwrap();
        let shared = ns
            .create_file(ROOT_INO, "shared.txt", Some(&hash), body.len() as u64, 1)
            .unwrap();
        let secret_dir = ns.create_dir(ROOT_INO, "secret", 1).unwrap();
        let secret_child = ns
            .create_file(secret_dir, "leak.txt", Some(&hash), body.len() as u64, 1)
            .unwrap();
        (shared, secret_dir, secret_child)
    };

    // The ACL: Alice may read /shared.txt only. Bob may read+write
    // /secret/**.
    let mut acl = WorkspaceAcl::new();
    acl.grant_read(ALICE, "/shared.txt");
    acl.grant_read_write(BOB, "/secret");

    // Helper: build a fresh QuickFS over the shared namespace bound to an
    // identity (each needs its own endpoint/hydrator).
    async fn mount_as(
        namespace: Arc<Mutex<Namespace>>,
        acl: Option<(WorkspaceAcl, [u8; 32])>,
    ) -> QuickFS {
        let endpoint = Endpoint::bind(presets::N0).await.expect("bind endpoint");
        let store = Arc::new(ContentStore::memory());
        let hydrator = Arc::new(Hydrator::new(endpoint, store));
        let heuristics = Arc::new(Heuristics::new(hydrator.clone(), namespace.clone()));
        let fs = QuickFS::new(namespace, hydrator, heuristics);
        match acl {
            Some((a, id)) => fs.with_acl(a, id),
            None => fs,
        }
    }

    // Mount view A — bound to Alice's identity.
    let alice_fs = mount_as(namespace.clone(), Some((acl.clone(), ALICE))).await;
    // Alice: read on the shared file → allowed; read on the secret dir
    // AND the secret child → denied (EACCES).
    assert_eq!(
        alice_fs.check_access(shared_ino, Access::Read),
        Ok(()),
        "Alice holds read on /shared.txt"
    );
    assert_eq!(
        alice_fs.check_access(secret_dir_ino, Access::Read),
        Err(libc::EACCES),
        "Alice has NO read cap on /secret → denied"
    );
    assert_eq!(
        alice_fs.check_access(secret_child_ino, Access::Read),
        Err(libc::EACCES),
        "Alice denied read on /secret/leak.txt too"
    );
    // Alice cannot write the file she may only read.
    assert_eq!(
        alice_fs.check_access(shared_ino, Access::Write),
        Err(libc::EACCES),
        "Alice's grant is read-only"
    );

    // Mount view B — same namespace + ACL, bound to Bob's identity.
    let bob_fs = mount_as(namespace.clone(), Some((acl, BOB))).await;
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
        Err(libc::EACCES),
        "Bob has NO cap on /shared.txt → denied"
    );

    // A mount with NO ACL attached keeps the compatibility veneer: every
    // access is allowed (the Unix layer alone governs).
    let open_fs = mount_as(namespace, None).await;
    assert_eq!(
        open_fs.check_access(shared_ino, Access::Write),
        Ok(()),
        "a mount without an ACL allows access (compat veneer)"
    );
}

// Full kernel path: mount two real FUSE sessions over the same namespace
// but bound to different identities, and prove the read is served for the
// identity that holds the cap and denied (EACCES) for the one that does
// not. Requires /dev/fuse (skips cleanly otherwise, like the tests below).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fuse_mount_enforces_identity_acl_end_to_end() {
    require_fuse!();
    let _ = timeout(Duration::from_secs(20), async {
        let endpoint = Endpoint::bind(presets::N0).await.expect("bind endpoint");
        let store = Arc::new(ContentStore::memory());
        let body = b"members only";
        let hash = store.write_content(body).await.unwrap().unwrap();

        let namespace = Arc::new(Mutex::new(Namespace::new("acl-mount-peer")));
        {
            let mut ns = namespace.lock().unwrap();
            ns.create_file(ROOT_INO, "secret.txt", Some(&hash), body.len() as u64, 1)
                .unwrap();
        }
        let hydrator = Arc::new(Hydrator::new(endpoint, store));
        let heuristics = Arc::new(Heuristics::new(hydrator.clone(), namespace.clone()));

        // ACL: only Bob may read /secret.txt. Alice holds nothing.
        let mut acl = WorkspaceAcl::new();
        acl.grant_read(BOB, "/secret.txt");

        // Alice's mount: read must be denied through the real kernel path.
        let alice_dir = tempdir().unwrap();
        let alice_fs = QuickFS::new(namespace.clone(), hydrator.clone(), heuristics.clone())
            .with_acl(acl.clone(), ALICE);
        let mut cfg_a = Config::default();
        cfg_a.mount_options = vec![MountOption::FSName("quickfs_acl_alice".into())];
        let _sa = fuser::spawn_mount2(alice_fs, alice_dir.path(), &cfg_a)
            .expect("spawn alice session");
        tokio::time::sleep(Duration::from_millis(1000)).await;

        let alice_read = std::fs::read(alice_dir.path().join("secret.txt"));
        assert!(
            matches!(&alice_read, Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied),
            "Alice (no cap) must be denied EACCES through the kernel, got {alice_read:?}"
        );

        // Bob's mount: read must succeed and return the real bytes.
        let bob_dir = tempdir().unwrap();
        let bob_fs = QuickFS::new(namespace.clone(), hydrator, heuristics).with_acl(acl, BOB);
        let mut cfg_b = Config::default();
        cfg_b.mount_options = vec![MountOption::FSName("quickfs_acl_bob".into())];
        let _sb =
            fuser::spawn_mount2(bob_fs, bob_dir.path(), &cfg_b).expect("spawn bob session");
        tokio::time::sleep(Duration::from_millis(1000)).await;

        let bob_read = std::fs::read(bob_dir.path().join("secret.txt")).expect("Bob may read");
        assert_eq!(bob_read, body, "Bob (holds cap) reads the real content");
    })
    .await;
}

// Multi-thread runtime: the FUSE read path uses `block_in_place`, which panics
// on a current-thread runtime.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fuse_projects_tree_and_reads_real_content() {
    require_fuse!();
    let test_timeout = Duration::from_secs(20);

    let _ = timeout(test_timeout, async {
        let mount_dir = tempdir().expect("mount temp dir");
        let mount_path = mount_dir.path().to_path_buf();

        let endpoint = Endpoint::bind(presets::N0).await.expect("bind endpoint");

        // Ingest real content into the local store and seed the namespace with it.
        let store = Arc::new(ContentStore::memory());
        let body = b"hello from the collaborative filesystem";
        let hash = store
            .write_content(body)
            .await
            .expect("write_content")
            .expect("non-empty");

        let namespace = Arc::new(Mutex::new(Namespace::new("test-peer")));
        {
            let mut ns = namespace.lock().unwrap();
            ns.create_file(ROOT_INO, "test_file.txt", Some(&hash), body.len() as u64, 1)
                .unwrap();
            let subdir = ns.create_dir(ROOT_INO, "subdir", 1).unwrap();
            ns.create_file(subdir, "nested.txt", Some(&hash), body.len() as u64, 1)
                .unwrap();
        }

        let hydrator = Arc::new(Hydrator::new(endpoint, store));
        let heuristics = Arc::new(Heuristics::new(hydrator.clone(), namespace.clone()));
        let fs = QuickFS::new(namespace, hydrator, heuristics);

        let options = vec![
            MountOption::RO,
            MountOption::FSName("quickfs_test".to_string()),
        ];
        let mut config = Config::default();
        config.mount_options = options;

        let _session =
            fuser::spawn_mount2(fs, &mount_path, &config).expect("spawn FUSE session");
        tokio::time::sleep(Duration::from_millis(1000)).await;

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
    })
    .await;
}

// Full read-write round-trip through the kernel: create, write, overwrite,
// mkdir, rename, unlink.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fuse_write_create_overwrite_rename_unlink() {
    require_fuse!();
    let _ = timeout(Duration::from_secs(20), async {
        let mount_dir = tempdir().expect("mount temp dir");
        let path = mount_dir.path().to_path_buf();

        let endpoint = Endpoint::bind(presets::N0).await.expect("bind endpoint");
        let store = Arc::new(ContentStore::memory());
        let namespace = Arc::new(Mutex::new(Namespace::new("rw-peer")));
        let hydrator = Arc::new(Hydrator::new(endpoint, store));
        let heuristics = Arc::new(Heuristics::new(hydrator.clone(), namespace.clone()));
        let fs = QuickFS::new(namespace, hydrator, heuristics);

        let mut config = Config::default();
        config.mount_options = vec![MountOption::FSName("quickfs_rw".to_string())];
        let _session =
            fuser::spawn_mount2(fs, &path, &config).expect("spawn FUSE session");
        tokio::time::sleep(Duration::from_millis(1000)).await;

        // create + write
        std::fs::write(path.join("hello.txt"), b"first").expect("write hello");
        assert_eq!(std::fs::read(path.join("hello.txt")).unwrap(), b"first");

        // overwrite (truncate + rewrite) with a longer body
        std::fs::write(path.join("hello.txt"), b"second longer content").expect("overwrite");
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
    })
    .await;
}

/// Read the `user.qos.sync` extended attribute of `path` through the kernel
/// (the exact `getxattr` a file manager issues), or `None` if it isn't set.
fn read_sync_xattr(path: &std::path::Path) -> Option<String> {
    use std::os::unix::ffi::OsStrExt;
    let cpath = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
    let name = std::ffi::CString::new("user.qos.sync").unwrap();
    let mut buf = [0u8; 64];
    let n = unsafe {
        libc::getxattr(
            cpath.as_ptr(),
            name.as_ptr(),
            buf.as_mut_ptr() as *mut libc::c_void,
            buf.len(),
        )
    };
    if n < 0 {
        return None;
    }
    Some(String::from_utf8_lossy(&buf[..n as usize]).into_owned())
}

// The sync-state interface end-to-end: a projected file (metadata only) reports
// `projected` via getxattr, a hydrated file reports `local`, and directories are
// `local`. This is the exact path the file manager reads to draw sync badges.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn getxattr_reports_projected_vs_local_sync_state() {
    require_fuse!();
    let _ = timeout(Duration::from_secs(20), async {
        let mount_dir = tempdir().expect("mount temp dir");
        let path = mount_dir.path().to_path_buf();

        let endpoint = Endpoint::bind(presets::N0).await.expect("bind endpoint");
        let store = Arc::new(ContentStore::memory());

        // A file whose content IS ingested locally → hydrated → `local`.
        let body = b"content that is already here";
        let local_hash = store.write_content(body).await.unwrap().unwrap();

        // A file whose namespace entry carries a content hash the store does NOT
        // have — exactly what a freshly-joined peer projects before hydration.
        let projected_hash =
            iroh_blobs::Hash::from([3u8; 32]);

        let namespace = Arc::new(Mutex::new(Namespace::new("xattr-peer")));
        {
            let mut ns = namespace.lock().unwrap();
            ns.create_file(ROOT_INO, "here.txt", Some(&local_hash), body.len() as u64, 1)
                .unwrap();
            ns.create_file(ROOT_INO, "remote.txt", Some(&projected_hash), 4096, 1)
                .unwrap();
            ns.create_dir(ROOT_INO, "folder", 1).unwrap();
        }

        let hydrator = Arc::new(Hydrator::new(endpoint, store));
        let heuristics = Arc::new(Heuristics::new(hydrator.clone(), namespace.clone()));
        let fs = QuickFS::new(namespace, hydrator, heuristics);

        let mut config = Config::default();
        config.mount_options = vec![MountOption::FSName("quickfs_xattr".to_string())];
        let _session = fuser::spawn_mount2(fs, &path, &config).expect("spawn FUSE session");
        tokio::time::sleep(Duration::from_millis(1000)).await;

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
    })
    .await;
}

// A recognized document format is stored as a CRDT and round-trips through the
// mount, while unknown formats stay opaque bytes.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fuse_document_roundtrip_and_opaque_still_works() {
    require_fuse!();
    let _ = timeout(Duration::from_secs(20), async {
        let mount_dir = tempdir().expect("mount temp dir");
        let path = mount_dir.path().to_path_buf();

        let endpoint = Endpoint::bind(presets::N0).await.expect("bind endpoint");
        let store = Arc::new(ContentStore::memory());
        let namespace = Arc::new(Mutex::new(Namespace::new("doc-peer")));
        let hydrator = Arc::new(Hydrator::new(endpoint, store));
        let heuristics = Arc::new(Heuristics::new(hydrator.clone(), namespace.clone()));
        let fs = QuickFS::new(namespace, hydrator, heuristics);

        let mut config = Config::default();
        config.mount_options = vec![MountOption::FSName("quickfs_doc".to_string())];
        let _session = fuser::spawn_mount2(fs, &path, &config).expect("spawn FUSE session");
        tokio::time::sleep(Duration::from_millis(1000)).await;

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
    })
    .await;
}

// rmdir must refuse a non-empty directory (no silent rm -rf), and unlink must
// refuse a directory.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rmdir_rejects_nonempty_and_unlink_rejects_dir() {
    require_fuse!();
    let _ = timeout(Duration::from_secs(20), async {
        let mount_dir = tempdir().expect("mount temp dir");
        let path = mount_dir.path().to_path_buf();

        let endpoint = Endpoint::bind(presets::N0).await.expect("bind endpoint");
        let store = Arc::new(ContentStore::memory());
        let namespace = Arc::new(Mutex::new(Namespace::new("rm-peer")));
        let hydrator = Arc::new(Hydrator::new(endpoint, store));
        let heuristics = Arc::new(Heuristics::new(hydrator.clone(), namespace.clone()));
        let fs = QuickFS::new(namespace, hydrator, heuristics);

        let mut config = Config::default();
        config.mount_options = vec![MountOption::FSName("quickfs_rm".to_string())];
        let _session = fuser::spawn_mount2(fs, &path, &config).expect("spawn FUSE session");
        tokio::time::sleep(Duration::from_millis(1000)).await;

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
    })
    .await;
}
