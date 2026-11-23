//! A workspace's tree, shown at its folder (`~/<workspace>`): mounted with FUSE where
//! this user can mount it, as Tilo OS mounts `/var/workspace`, and a plain
//! folder synced both ways everywhere else ([`tilo_fs::sync_folder`]).
//!
//! Both are the same filesystem ([`tilo_fs::QuickFS`]) over the tree replica:
//! a file copied in becomes a node of the tree and its bytes this device's
//! content, and a file read that this device does not hold yet is fetched from
//! the workspace as it is read.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use tilo_fs::fs::QuickFS;
use tilo_fs::hydration::{FileKeys, Hydrator};
use tilo_fs::smart::Heuristics;
use tilo_fs::tree_sync::TreeSync;

/// How often a synced folder carries changes across, where nothing mounts it.
const FOLDER_PASS: Duration = Duration::from_secs(1);

/// How the tree is shown.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Shown {
    /// A FUSE mount.
    #[cfg(target_os = "linux")]
    Mounted,
    /// A folder synced both ways.
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
            }
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

/// The filesystem over the replica's namespace, as Tilo OS's filesystem
/// daemon serves `/var/workspace`: versions with who wrote them, files on
/// demand with keep-offline and free-up-space, writes staged on disk, and
/// bytes a crash left unsaved put back. Must be called on a tokio runtime.
pub(crate) fn filesystem(parts: &Parts<'_>) -> std::io::Result<QuickFS> {
    let hydrator = Arc::new(
        Hydrator::new(Arc::clone(&parts.store))
            .with_source(Arc::clone(&parts.content))
            .with_file_keys(parts.keys.clone()),
    );
    let heuristics = Arc::new(Heuristics::new(Arc::clone(&hydrator)));
    let availability = tilo_fs::AvailabilityRegistry::open(parts.state.join("availability.json"))
        .map_err(std::io::Error::other)?;
    let evictions = tilo_fs::eviction::EvictionQueue::open(parts.state.join("evictions.json"))
        .map_err(std::io::Error::other)?;
    let fs = QuickFS::new(Arc::clone(&parts.replica.namespace), hydrator, heuristics)
        .with_availability(Arc::new(std::sync::Mutex::new(availability)))
        .with_eviction_queue(Arc::new(tokio::sync::Mutex::new(evictions)))
        .with_actor(parts.actor)
        .with_writers(Arc::clone(&parts.writers))
        .with_admission(Arc::clone(&parts.admission))
        .with_pinned_hydration()
        .with_change_journal(parts.replica.journal());
    fs.resume_pending_evictions();
    let fs = match tilo_fs::unsaved::UnsavedStore::open(parts.state.join("unsaved-writes")) {
        Ok(unsaved) => {
            let fs = fs.with_unsaved_store(unsaved);
            fs.recover_unsaved_writes();
            fs
        }
        Err(error) => {
            tracing::warn!(%error, "unsaved writes are not staged on disk");
            fs
        }
    };
    let fs = fs.with_write_stage_dir(parts.state.join("write-stage"));
    // A closed file is kept the moment `close` answers, whatever ends this process next, and
    // reaches the disk with the journal naming it on a short window: a checkout of thousands of
    // files waits for the disk once a window, not once a file.
    fs.sync_on_window(Arc::clone(&parts.store), Some(parts.replica.sync_journal()));
    Ok(fs)
}

/// Show the filesystem `make` makes at `at`: mounted when `mount` and this
/// user can mount FUSE, else as a folder synced both ways, its sync state
/// kept in `state`. What may be written is the filesystem's admission's to
/// answer, write by write.
pub(crate) fn show(
    make: impl Fn() -> std::io::Result<QuickFS>,
    at: &Path,
    state: &Path,
    mount: bool,
) -> std::io::Result<Showing> {
    // A mount a peer that ended left behind answers ENOTCONN, which no
    // folder can be made at.
    #[cfg(target_os = "linux")]
    tilo_fs::fuse_session::clear_stale_mount(at)?;
    std::fs::create_dir_all(at)?;
    #[cfg(target_os = "linux")]
    if mount
        && tilo_fs::fuse_session::mountable(
            Path::new("/dev/fuse"),
            std::env::var_os("PATH").as_deref(),
        )
    {
        let fs = make()?;
        let activity = fs.activity();
        match mounted(fs, at) {
            Ok(session) => {
                return Ok(Showing {
                    at: at.to_path_buf(),
                    shown: Shown::Mounted,
                    activity,
                    view: View::Mount(Some(session)),
                });
            }
            Err(error) => {
                tracing::warn!(%error, at = %at.display(), "the workspace is not mounted; syncing a folder instead");
            }
        }
    }
    #[cfg(not(target_os = "linux"))]
    let _ = mount;
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

#[cfg(target_os = "linux")]
fn mounted(fs: QuickFS, at: &Path) -> std::io::Result<fuser::BackgroundSession> {
    tilo_fs::fuse_session::clear_stale_mount(at)?;
    let mut config = fuser::Config::default();
    config.mount_options = vec![fuser::MountOption::FSName("tilo".into())];
    // Owner-only needs no system FUSE policy; the kernel refuses every other user.
    config.acl = fuser::SessionACL::Owner;
    fuser::spawn_mount(fs, at, &config)
}
