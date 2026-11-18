#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "a test fixture: a failed step is the assertion"
)]

//! A mounted folder answers like a filesystem: a name removed can be made again at once, and a
//! tree removed reports nothing left behind.

use super::*;
use crate::store::ContentStore;
use iroh::endpoint::{Endpoint, presets};
use std::path::Path;

/// These tests mount a real FUSE filesystem. A machine that cannot (a test sandbox without
/// `/dev/fuse` or the helper that mounts it) leaves them out; the real-fuse check requires
/// them, and fails rather than skips when the capability is missing there.
macro_rules! require_fuse {
    () => {
        tilo_test_env::require_capability!(
            Fuse,
            crate::fuse_session::mountable(
                Path::new("/dev/fuse"),
                std::env::var_os("PATH").as_deref()
            ),
            "requires /dev/fuse and fusermount3"
        );
    };
}

/// A filesystem over an empty namespace, with the runtime its endpoint runs on, built as the
/// peer's own mount is: with version history, writer names, pinned hydration and staged writes.
fn mounted(state: &Path) -> (QuickFS, Endpoint, tokio::runtime::Runtime) {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let endpoint = runtime.block_on(Endpoint::bind(presets::Minimal)).unwrap();
    // The store and the mount start tasks on the runtime this test keeps.
    let _entered = runtime.enter();
    let hydrator = Arc::new(Hydrator::new(
        endpoint.clone(),
        Arc::new(ContentStore::memory()),
    ));
    let namespace = Arc::new(Mutex::new(Namespace::new("mount-tests").unwrap()));
    let heuristics = Arc::new(Heuristics::new(Arc::clone(&hydrator)));
    let fs = QuickFS::new(namespace, hydrator, heuristics)
        .with_actor(fs_protocol::FileActor {
            person: fs_protocol::Ed25519PublicKey::from_bytes([3; 32]),
            device: fs_protocol::Ed25519PublicKey::from_bytes([4; 32]),
        })
        .with_writers(Arc::new(crate::writer::WriterResolver::default()))
        .with_pinned_hydration()
        .with_unsaved_store(crate::unsaved::UnsavedStore::open(state.join("unsaved")).unwrap())
        .with_write_stage_dir(state.join("stage"));
    (fs, endpoint, runtime)
}

/// Mount `fs` at `at`, keeping its session alive while the returned guard lives.
fn mount(fs: QuickFS, at: &Path) -> fuser::BackgroundSession {
    let mut config = fuser::Config::default();
    config.mount_options = vec![fuser::MountOption::FSName("quickfs-test".to_owned())];
    config.n_threads = Some(2);
    config.clone_fd = true;
    config.acl = fuser::SessionACL::Owner;
    fuser::spawn_mount(fs, at, &config).unwrap()
}

fn unmount(session: fuser::BackgroundSession, at: &Path) {
    let _ = crate::fuse_session::unmount_and_join(session, at);
}

/// `rm -rf` of a directory and `mkdir` of the same name straight after it: a filesystem lets a
/// name be made again the moment it is gone.
#[test]
fn a_name_removed_can_be_made_again_at_once() {
    require_fuse!();
    let tmp = tempfile::tempdir().unwrap();
    let at = tmp.path().join("mnt");
    std::fs::create_dir(&at).unwrap();
    let (fs, endpoint, runtime) = mounted(tmp.path());
    let session = mount(fs, &at);

    std::fs::create_dir(at.join("docs")).unwrap();
    std::fs::write(at.join("docs/note.txt"), b"nine").unwrap();
    std::fs::remove_dir_all(at.join("docs")).unwrap();
    std::fs::create_dir(at.join("docs")).unwrap();
    assert_eq!(
        std::fs::read_dir(at.join("docs")).unwrap().count(),
        0,
        "a directory made again where one was removed is empty"
    );

    unmount(session, &at);
    drop(endpoint);
    drop(runtime);
}

/// A name taken by a directory, a file, a directory again, each removed before the next is
/// made: the mount answers like any filesystem, however often a name is made and taken.
#[test]
fn a_name_can_be_made_and_taken_again_and_again() {
    require_fuse!();
    let tmp = tempfile::tempdir().unwrap();
    let at = tmp.path().join("mnt");
    std::fs::create_dir(&at).unwrap();
    let (fs, endpoint, runtime) = mounted(tmp.path());
    let session = mount(fs, &at);

    for round in 0..20 {
        std::fs::create_dir(at.join("x")).unwrap();
        std::fs::write(at.join("x/a.txt"), b"a").unwrap();
        std::fs::create_dir(at.join("x/y")).unwrap();
        std::fs::remove_dir_all(at.join("x")).unwrap();
        assert!(
            !at.join("x").exists(),
            "round {round}: the directory is gone"
        );
        std::fs::write(at.join("x"), b"file").unwrap();
        std::fs::remove_file(at.join("x")).unwrap();
        std::fs::create_dir(at.join("x")).unwrap();
        std::fs::remove_dir(at.join("x")).unwrap();
    }

    unmount(session, &at);
    drop(endpoint);
    drop(runtime);
}

/// `rm -rf` of a tree of directories and files: every name in it goes, and the whole removal
/// reports nothing left.
#[test]
fn a_tree_removed_takes_every_name_with_it() {
    require_fuse!();
    let tmp = tempfile::tempdir().unwrap();
    let at = tmp.path().join("mnt");
    std::fs::create_dir(&at).unwrap();
    let (fs, endpoint, runtime) = mounted(tmp.path());
    let session = mount(fs, &at);

    std::fs::create_dir_all(at.join("a/b/c")).unwrap();
    std::fs::write(at.join("a/b/c/one.txt"), b"one").unwrap();
    std::fs::write(at.join("a/b/two.txt"), b"two").unwrap();
    std::fs::remove_dir_all(at.join("a")).unwrap();
    assert!(!at.join("a").exists(), "the removed tree is gone");
    std::fs::create_dir_all(at.join("a/b/c")).unwrap();
    assert!(at.join("a/b/c").is_dir(), "and can be made again");

    unmount(session, &at);
    drop(endpoint);
    drop(runtime);
}
