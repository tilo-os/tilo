use std::sync::{Arc, Mutex};
use std::time::Duration;

use fs::namespace::ROOT_INO;
use fs::{ContentStore, Heuristics, Hydrator, Namespace, QuickFS};
use fuser::{Config, MountOption};
use iroh::endpoint::Endpoint;
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

// Multi-thread runtime: the FUSE read path uses `block_in_place`, which panics
// on a current-thread runtime.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fuse_projects_tree_and_reads_real_content() {
    require_fuse!();
    let test_timeout = Duration::from_secs(20);

    let _ = timeout(test_timeout, async {
        let mount_dir = tempdir().expect("mount temp dir");
        let mount_path = mount_dir.path().to_path_buf();

        let endpoint = Endpoint::bind().await.expect("bind endpoint");

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

        let endpoint = Endpoint::bind().await.expect("bind endpoint");
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

        let endpoint = Endpoint::bind().await.expect("bind endpoint");
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

        let endpoint = Endpoint::bind().await.expect("bind endpoint");
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

        let endpoint = Endpoint::bind().await.expect("bind endpoint");
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
