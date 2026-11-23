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

/// A filesystem over an empty namespace, with the runtime it runs on, built as the
/// peer's own mount is: with version history, writer names, pinned hydration and staged writes.
fn mounted(state: &Path) -> (QuickFS, tokio::runtime::Runtime) {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    // The store and the mount start tasks on the runtime this test keeps.
    let _entered = runtime.enter();
    let hydrator = Arc::new(Hydrator::new(Arc::new(ContentStore::memory())));
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
    (fs, runtime)
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

/// A name taken by a directory, a file, a directory again, each removed before the next is
/// made: the mount answers like any filesystem, however often a name is made and taken. A
/// directory made where one was removed is empty.
#[test]
fn a_name_can_be_made_and_taken_again_and_again() {
    require_fuse!();
    let tmp = tempfile::tempdir().unwrap();
    let at = tmp.path().join("mnt");
    std::fs::create_dir(&at).unwrap();
    let (fs, runtime) = mounted(tmp.path());
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
        assert_eq!(
            std::fs::read_dir(at.join("x")).unwrap().count(),
            0,
            "round {round}: a directory made again where one was removed is empty"
        );
        std::fs::remove_dir(at.join("x")).unwrap();
    }

    unmount(session, &at);
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
    let (fs, runtime) = mounted(tmp.path());
    let session = mount(fs, &at);

    std::fs::create_dir_all(at.join("a/b/c")).unwrap();
    std::fs::write(at.join("a/b/c/one.txt"), b"one").unwrap();
    std::fs::write(at.join("a/b/two.txt"), b"two").unwrap();
    std::fs::remove_dir_all(at.join("a")).unwrap();
    assert!(!at.join("a").exists(), "the removed tree is gone");
    std::fs::create_dir_all(at.join("a/b/c")).unwrap();
    assert!(at.join("a/b/c").is_dir(), "and can be made again");

    unmount(session, &at);
    drop(runtime);
}

/// A process table naming this test's own thread as a desktop program (a text editor launched
/// from the desktop, as its scope names it), so every request the kernel hands the mount from
/// this thread comes from that program.
fn editor_procs(dir: &Path) -> crate::writer::WriterResolver {
    let proc = dir.join("proc");
    let pid = std::process::id();
    let tid = rustix::thread::gettid()
        .as_raw_nonzero()
        .get()
        .unsigned_abs();
    std::fs::create_dir_all(proc.join(pid.to_string())).unwrap();
    std::fs::create_dir_all(proc.join(tid.to_string())).unwrap();
    std::fs::write(proc.join("stat"), "cpu 1 2 3\nbtime 1700000000\n").unwrap();
    std::fs::write(
        proc.join(tid.to_string()).join("status"),
        format!("Tgid:\t{pid}\n"),
    )
    .unwrap();
    std::fs::write(
        proc.join(pid.to_string()).join("stat"),
        format!(
            "{pid} (gnome-text-edit) S 1 {pid} {pid} 0 -1 4194304 0 0 0 0 0 0 0 0 20 0 1 0 7 0 0"
        ),
    )
    .unwrap();
    std::fs::write(proc.join(pid.to_string()).join("environ"), b"").unwrap();
    std::fs::write(
        proc.join(pid.to_string()).join("cgroup"),
        "0::/user.slice/user-1000.slice/user@1000.service/app.slice/app-gnome-org.gnome.TextEditor-4242.scope\n",
    )
    .unwrap();
    let applications = dir.join("applications");
    std::fs::create_dir_all(&applications).unwrap();
    std::fs::write(
        applications.join("org.gnome.TextEditor.desktop"),
        "[Desktop Entry]\nName=Text Editor\n",
    )
    .unwrap();
    crate::writer::WriterResolver::new(proc, vec![applications])
}

/// The workspace `w`, owned by someone else, where the person at this device (key `[3; 32]`,
/// as [`mounted`] names them) holds `role`.
fn policy_with(role: tilo_action_policy::Role) -> tilo_action_policy::WorkspacePolicy {
    let mut policy = tilo_action_policy::WorkspacePolicy::new("w", "11".repeat(32));
    policy.members.insert("03".repeat(32), role);
    policy
}

/// An editor saving a file the way desktop editors do (a new file beside it, then renamed over
/// it), from a program the desktop launched: the person's own access decides it, so an editor's
/// save lands and a viewer's is refused as a read-only file system.
#[test]
fn a_desktop_editor_saves_with_its_person_s_access() {
    require_fuse!();
    for (role, saves) in [
        (tilo_action_policy::Role::Editor, true),
        (tilo_action_policy::Role::Viewer, false),
    ] {
        let tmp = tempfile::tempdir().unwrap();
        let at = tmp.path().join("mnt");
        std::fs::create_dir(&at).unwrap();
        let (fs, runtime) = mounted(tmp.path());
        // The file the person opens was already there.
        fs.apply_local_file(Path::new("note.txt"), b"before", 1_000)
            .unwrap();
        let admission = crate::admission::FileAdmission::new(
            &"03".repeat(32),
            Box::new(move || Some(policy_with(role))),
        )
        .unwrap();
        let fs = fs
            .with_writers(Arc::new(editor_procs(tmp.path())))
            .with_admission(Arc::new(admission));
        let session = mount(fs, &at);

        let save = (|| {
            let staged = at.join(".goutputstream-ABC123");
            let mut file = std::fs::File::create(&staged)?;
            std::io::Write::write_all(&mut file, b"after")?;
            file.sync_all()?;
            drop(file);
            std::fs::rename(&staged, at.join("note.txt"))
        })();
        let now = std::fs::read(at.join("note.txt")).unwrap();
        unmount(session, &at);
        drop(runtime);

        if saves {
            save.unwrap();
            assert_eq!(now, b"after", "{role:?}: the save lands");
        } else {
            let refused = save.unwrap_err();
            assert_eq!(
                refused.raw_os_error(),
                Some(EROFS),
                "{role:?}: refused as a read-only file system, not lost: {refused}"
            );
            assert_eq!(now, b"before", "{role:?}: the file is as it was");
        }
    }
}
