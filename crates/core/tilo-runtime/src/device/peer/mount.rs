//! A workspace's tree, shown at its folder (`~/<workspace>`) through FUSE, Linux's file
//! provider. The filesystem ([`tilo_fs::QuickFS`]) is one over the tree replica.

use std::path::{Path, PathBuf};
use std::sync::Arc;
#[cfg(not(target_os = "linux"))]
use std::time::Duration;

use tilo_fs::fs::QuickFS;
use tilo_fs::hydration::{FileKeys, Hydrator};
use tilo_fs::tree_sync::TreeSync;

#[cfg(not(target_os = "linux"))]
const FOLDER_PASS: Duration = Duration::from_secs(1);

/// How the tree is shown.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Shown {
    #[cfg(target_os = "linux")]
    Mounted,
    #[cfg(not(target_os = "linux"))]
    Folder,
}

/// One workspace's tree, shown at its folder while this lives.
pub(crate) struct Showing {
    pub(crate) at: PathBuf,
    pub(crate) shown: Shown,
    /// The requests the folder's filesystem is answering: a holder ending waits on them.
    pub(crate) activity: Arc<tilo_fs::fs::activity::Activity>,
    view: View,
}

enum View {
    #[cfg(target_os = "linux")]
    Mount(Option<fuser::BackgroundSession>),
    #[cfg(not(target_os = "linux"))]
    Folder(
        #[allow(dead_code, reason = "held: dropping it stops the sync")]
        tilo_fs::sync_folder::Watch,
    ),
}

impl Drop for Showing {
    fn drop(&mut self) {
        match &mut self.view {
            #[cfg(target_os = "linux")]
            View::Mount(session) => {
                if let Some(session) = session.take()
                    && let Err(error) = tilo_fs::fuse_session::unmount_and_join(session, &self.at)
                {
                    tracing::warn!(
                        %error,
                        at = %self.at.display(),
                        "the workspace did not unmount cleanly"
                    );
                }
                // A file written into the folder now would land underneath the next mount: the
                // folder refuses it until the workspace is shown there again.
                if let Err(error) = set_mode(&self.at, WHILE_NOT_SHOWN) {
                    tracing::warn!(
                        %error,
                        at = %self.at.display(),
                        "the folder takes writes while the workspace is not shown there"
                    );
                }
            }
            #[cfg(not(target_os = "linux"))]
            View::Folder(_) => {}
        }
    }
}

/// What one workspace's filesystem is made of here.
pub(crate) struct Parts<'a> {
    pub(crate) keys: FileKeys,
    pub(crate) replica: &'a TreeSync,
    pub(crate) store: Arc<tilo_fs::ContentStore>,
    /// Where the bytes this device lacks are asked for.
    pub(crate) content: Arc<dyn tilo_fs::hydration::ContentSource>,
    pub(crate) actor: fs_protocol::FileActor,
    pub(crate) writers: Arc<tilo_fs::writer::WriterResolver>,
    /// What the workspace takes of the writes made through it.
    pub(crate) admission: Arc<tilo_fs::admission::FileAdmission>,
    /// Where its availability, staged writes and unsaved bytes are kept.
    pub(crate) state: &'a Path,
}

/// The filesystem over the replica's namespace, with history, files on demand, staged writes
/// and crash recovery. Must be called on a tokio runtime.
pub(crate) fn filesystem(parts: &Parts<'_>) -> std::io::Result<QuickFS> {
    let hydrator = Arc::new(
        Hydrator::new(Arc::clone(&parts.store))
            .with_source(Arc::clone(&parts.content))
            .with_file_keys(parts.keys.clone()),
    );
    let availability = tilo_fs::AvailabilityRegistry::open(parts.state.join("availability.json"))
        .map_err(std::io::Error::other)?;
    let evictions = tilo_fs::eviction::EvictionQueue::open(parts.state.join("evictions.json"))
        .map_err(std::io::Error::other)?;
    let fs = QuickFS::over(Arc::clone(&parts.replica.namespace), hydrator)
        .with_availability(Arc::new(std::sync::Mutex::new(availability)))
        .with_eviction_queue(Arc::new(tokio::sync::Mutex::new(evictions)))
        .with_actor(parts.actor)
        .with_writers(Arc::clone(&parts.writers))
        .with_admission(Arc::clone(&parts.admission))
        .with_pinned_hydration()
        .with_change_journal(parts.replica.journal());
    // Both recoveries block on this device's store to finish. A task of the app's
    // current-thread runtime cannot block that runtime's one thread, so they run on a thread of
    // theirs while the caller waits, as a FUSE worker or the daemon's own start would run them.
    let state = parts.state;
    let fs = off_the_worker(move || {
        fs.resume_pending_evictions();
        match tilo_fs::unsaved::UnsavedStore::open(state.join("unsaved-writes")) {
            Ok(unsaved) => {
                let fs = fs.with_unsaved_store(unsaved);
                fs.recover_unsaved_writes();
                fs
            }
            Err(error) => {
                tracing::warn!(%error, "unsaved writes are not staged on disk");
                fs
            }
        }
    });
    let fs = fs.with_write_stage_dir(parts.state.join("write-stage"));
    // A closed file is kept the moment `close` answers, whatever ends this process next, and
    // reaches the disk with the journal naming it on a short window: a checkout of thousands of
    // files waits for the disk once a window, not once a file.
    fs.sync_on_window(Arc::clone(&parts.store), Some(parts.replica.sync_journal()));
    Ok(fs)
}

/// Run `step`, which blocks its thread, where blocking is allowed: a thread inside a runtime is
/// that runtime's, not the step's, so the step runs on a thread of its own and the caller waits.
/// A founding call starts the person's workspace, and mounts its files, from a task of the app's
/// current-thread runtime, or from a plain thread that drives that runtime's `block_on` (host
/// search reaching the person's workspace first), whether or not that `block_on` enters a tokio
/// context: its store's blocking bridge can take none of them.
fn off_the_worker<T: Send>(step: impl FnOnce() -> T + Send) -> T {
    if tokio::runtime::Handle::try_current().is_err() && !tilo_rt::in_async_context() {
        return step();
    }
    std::thread::scope(|scope| {
        let done = scope.spawn(step);
        match done.join() {
            Ok(output) => output,
            Err(panic) => std::panic::resume_unwind(panic),
        }
    })
}

/// Show the filesystem `make` makes at `at` through FUSE.
#[cfg(target_os = "linux")]
pub(crate) fn show(
    make: impl Fn() -> std::io::Result<QuickFS>,
    at: &Path,
    state: &Path,
) -> std::io::Result<Showing> {
    // A mount a peer that ended left behind answers ENOTCONN, which no mount can be made at.
    tilo_fs::fuse_session::clear_stale_mount(at)?;
    std::fs::create_dir_all(at)?;
    // Mounting needs the folder underneath writable; while nothing is mounted it refuses writes
    // ([`Showing`]'s drop).
    set_mode(at, WHILE_SHOWN)?;
    if !tilo_fs::fuse_session::mountable(
        Path::new("/dev/fuse"),
        std::env::var_os("PATH").as_deref(),
    ) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "a workspace's folder is a FUSE mount, and this computer has no FUSE: \
             /dev/fuse and fusermount3 are missing",
        ));
    }
    let fs = make()?;
    // What was written into the folder while nothing was mounted there is the person's: it goes
    // into the workspace before the mount covers it, never hidden underneath.
    let taken = off_the_worker(|| tilo_fs::underneath::take_in(&fs, at, &state.join("taking-in")))?;
    if taken > 0 {
        tracing::info!(taken, at = %at.display(), "the files written into the folder while it was not shown are the workspace's now");
    }
    let activity = fs.activity();
    let session = mounted(fs, at)?;
    Ok(Showing {
        at: at.to_path_buf(),
        shown: Shown::Mounted,
        activity,
        view: View::Mount(Some(session)),
    })
}

/// Show the filesystem `make` makes at `at` as a synced folder: macOS's until its file provider
/// writes, Windows's until its cloud files provider does, and Android's, where an app mounts
/// nothing.
#[cfg(not(target_os = "linux"))]
pub(crate) fn show(
    make: impl Fn() -> std::io::Result<QuickFS>,
    at: &Path,
    state: &Path,
) -> std::io::Result<Showing> {
    std::fs::create_dir_all(at)?;
    let commit = Box::new(|| Ok(()));
    let fs = make()?;
    let activity = fs.activity();
    let watch = tilo_fs::sync_folder::SyncFolder::open(Arc::new(fs), at, state, commit)?
        .watch(FOLDER_PASS)?;
    Ok(Showing {
        at: at.to_path_buf(),
        shown: Shown::Folder,
        activity,
        view: View::Folder(watch),
    })
}

/// The folder underneath a mount while the workspace is shown there: fusermount mounts only over
/// a folder its user may write.
#[cfg(target_os = "linux")]
const WHILE_SHOWN: u32 = 0o755;
/// The folder underneath while the workspace is not shown there: a write is refused rather than
/// landing where the next mount would hide it.
#[cfg(target_os = "linux")]
const WHILE_NOT_SHOWN: u32 = 0o555;

#[cfg(target_os = "linux")]
fn set_mode(at: &Path, mode: u32) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    // Never through a mount still there: a stat of one whose daemon ended waits for nothing.
    if tilo_fs::fuse_session::is_mounted(at) {
        return Ok(());
    }
    std::fs::set_permissions(at, std::fs::Permissions::from_mode(mode))
}

#[cfg(target_os = "linux")]
fn mounted(fs: QuickFS, at: &Path) -> std::io::Result<fuser::BackgroundSession> {
    let mut config = fuser::Config::default();
    config.mount_options = vec![fuser::MountOption::FSName("tilo".into())];
    // Owner-only needs no system FUSE policy; the kernel refuses every other user.
    config.acl = fuser::SessionACL::Owner;
    fuser::spawn_mount(fs, at, &config)
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "a test fixture failing is its test failing"
)]
mod tests {
    use super::*;

    /// A founding call starts the person's workspace, and mounts its files, from a task of the
    /// process's current-thread runtime. The mount's recovery waits on the device's store, which
    /// that runtime cannot lend its one thread to: it runs on a thread of its own and the task is
    /// answered.
    #[test]
    fn a_mounts_recovery_waits_off_the_runtime_that_cannot() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(
            tilo_rt::app()
                .block_on(tilo_fs::ContentStore::open(dir.path().join("content")))
                .unwrap()
                .unwrap(),
        );
        let state = dir.path().join("evictions.json");
        let (told, heard) = std::sync::mpsc::channel();
        tilo_rt::app().spawn(async move {
            // The filesystem is made on the task, as `filesystem` makes it, and only the
            // recovery that waits is moved off.
            let hydrator = Arc::new(Hydrator::new(store));
            let namespace = Arc::new(std::sync::Mutex::new(
                tilo_fs::namespace::Namespace::new("test").unwrap(),
            ));
            let queue = tilo_fs::eviction::EvictionQueue::open(state).unwrap();
            let fs = QuickFS::over(namespace, hydrator)
                .with_eviction_queue(Arc::new(tokio::sync::Mutex::new(queue)));
            off_the_worker(move || fs.resume_pending_evictions());
            let _ = told.send(());
        });
        heard
            .recv_timeout(std::time::Duration::from_secs(30))
            .expect("the task that mounted the files was answered");

        // Host search reaches the person's workspace first from a thread of its own, which waits
        // for the founding on the app runtime's `block_on`: inside that runtime, but no task of it.
        let state = dir.path().join("evictions-again.json");
        let store = Arc::new(
            tilo_rt::app()
                .block_on(tilo_fs::ContentStore::open(
                    dir.path().join("content-again"),
                ))
                .unwrap()
                .unwrap(),
        );
        let (told, heard) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            tilo_rt::app().handle().block_on(async move {
                let hydrator = Arc::new(Hydrator::new(store));
                let namespace = Arc::new(std::sync::Mutex::new(
                    tilo_fs::namespace::Namespace::new("test").unwrap(),
                ));
                let queue = tilo_fs::eviction::EvictionQueue::open(state).unwrap();
                let fs = QuickFS::over(namespace, hydrator)
                    .with_eviction_queue(Arc::new(tokio::sync::Mutex::new(queue)));
                off_the_worker(move || fs.resume_pending_evictions());
            });
            let _ = told.send(());
        });
        heard
            .recv_timeout(std::time::Duration::from_secs(30))
            .expect("the thread that waited on the app runtime for the mount was answered");

        // The same founding reached through the app runtime's own `block_on`, which drives the
        // future on the calling thread: no tokio context is entered there, but the thread drives
        // the runtime all the same.
        let state = dir.path().join("evictions-driven.json");
        let store = Arc::new(
            tilo_rt::app()
                .block_on(tilo_fs::ContentStore::open(
                    dir.path().join("content-driven"),
                ))
                .unwrap()
                .unwrap(),
        );
        let (told, heard) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            tilo_rt::app()
                .block_on(async move {
                    let hydrator = Arc::new(Hydrator::new(store));
                    let namespace = Arc::new(std::sync::Mutex::new(
                        tilo_fs::namespace::Namespace::new("test").unwrap(),
                    ));
                    let queue = tilo_fs::eviction::EvictionQueue::open(state).unwrap();
                    let fs = QuickFS::over(namespace, hydrator)
                        .with_eviction_queue(Arc::new(tokio::sync::Mutex::new(queue)));
                    off_the_worker(move || fs.resume_pending_evictions());
                })
                .unwrap();
            let _ = told.send(());
        });
        heard
            .recv_timeout(std::time::Duration::from_secs(30))
            .expect("the thread driving the app runtime's own wait for the mount was answered");
    }
}
