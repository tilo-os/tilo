//! The profile's workspace files, and how this host shows them where the
//! desktop does not: at `~/Tilo/<workspace>`.
//!
//! The files live in the profile as the filesystem daemon keeps them on
//! Tilo OS: a namespace, the content store the profile's document media
//! share, and a history of every version. On Linux with a usable FUSE the
//! workspace is mounted there ([`mount`]); everywhere else a plain folder
//! mirrors it both ways ([`tilo_fs::sync_folder`]).

#[cfg(target_os = "linux")]
mod mount;
mod serve;

use std::{
    io,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};
use tilo_fs::{
    fs::QuickFS,
    history::FileHistoryStore,
    hydration::Hydrator,
    namespace::Namespace,
    smart::Heuristics,
    sync_folder::{SyncFolder, Watch},
};

/// The profile directory holding the workspace files' state.
const FILES: &str = "files";
const NAMESPACE: &str = "namespace.bin";
const HISTORY: &str = "history.json";
/// Workspace changes reach the folder, and are persisted, within this.
const INTERVAL: Duration = Duration::from_secs(2);

/// Who the workspace attributes this host's versions to, and the device
/// they come from.
pub(super) struct Author {
    pub(super) person: [u8; 32],
    pub(super) device: [u8; 32],
}

/// How the workspace files appear at their folder's path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Projection {
    /// A plain folder mirroring them.
    Folder,
    /// A FUSE mount of the workspace itself.
    #[cfg(target_os = "linux")]
    Mount,
}

impl Projection {
    /// A mount where this user can make one, otherwise the folder.
    pub(super) fn for_host() -> Self {
        #[cfg(target_os = "linux")]
        if mount::available_in(Path::new("/dev/fuse"), std::env::var_os("PATH").as_deref()) {
            return Self::Mount;
        }
        Self::Folder
    }
}

/// The workspace files, shown at their folder's path and served on the
/// profile's [`crate::FILES_SOCKET`] until dropped.
pub(super) struct WorkspaceFiles {
    _view: View,
    _server: runtime_fs::workspace_server::WorkspaceServer,
}

/// Held for its lifetime: dropping it stops the sync or unmounts.
enum View {
    Folder {
        _watch: Watch,
    },
    #[cfg(target_os = "linux")]
    Mount {
        _mount: mount::Mount,
    },
}

impl WorkspaceFiles {
    /// Open the workspace files of `profile` over the content `store` and
    /// show them at `folder` as `projection`. A mount that fails falls
    /// back to the folder.
    pub(super) fn start(
        profile: &Path,
        folder: &Path,
        store: Arc<tilo_fs::ContentStore>,
        endpoint: iroh::Endpoint,
        author: &Author,
        projection: Projection,
    ) -> io::Result<Self> {
        let files = Files::open(&profile.join(FILES), store, endpoint, author)?;
        let server = runtime_fs::workspace_server::serve_workspace(
            &profile.join(crate::FILES_SOCKET),
            Arc::new(serve::Backend(Arc::new(files.quick_fs()?))),
            tilo_rt::app().handle().clone(),
        )?;
        let view = match projection {
            Projection::Folder => View::Folder {
                _watch: files.sync_folder(folder)?.watch(INTERVAL)?,
            },
            #[cfg(target_os = "linux")]
            Projection::Mount => {
                // Changes made in the folder while nothing was mounted over
                // it enter the workspace before the mount hides them.
                files.sync_folder(folder)?.reconcile()?;
                match mount::Mount::start(files.quick_fs()?, folder, Arc::clone(&files.namespace)) {
                    Ok(mount) => View::Mount { _mount: mount },
                    Err(error) => {
                        tracing::warn!(%error, "the workspace cannot be mounted; syncing a folder instead");
                        View::Folder {
                            _watch: files.sync_folder(folder)?.watch(INTERVAL)?,
                        }
                    }
                }
            }
        };
        Ok(Self {
            _view: view,
            _server: server,
        })
    }
}

/// The workspace's durable parts, from which each view builds its `QuickFS`.
struct Files {
    state: PathBuf,
    namespace: Arc<NamespaceFile>,
    history: Arc<FileHistoryStore>,
    hydrator: Arc<Hydrator>,
    actor: fs_protocol::FileActor,
}

impl Files {
    fn open(
        state: &Path,
        store: Arc<tilo_fs::ContentStore>,
        endpoint: iroh::Endpoint,
        author: &Author,
    ) -> io::Result<Self> {
        crate::unix::private_directory(state)?;
        let namespace = NamespaceFile::open(state.join(NAMESPACE), hex::encode(author.device))?;
        let history = FileHistoryStore::open(state.join(HISTORY)).map_err(io::Error::other)?;
        let hydrator = {
            let _entered = tilo_rt::app().handle().enter();
            Arc::new(Hydrator::new(endpoint, store))
        };
        Ok(Self {
            state: state.to_path_buf(),
            namespace: Arc::new(namespace),
            history: Arc::new(history),
            hydrator,
            actor: fs_protocol::FileActor {
                person: collab_types::Ed25519PublicKey(author.person),
                device: collab_types::Ed25519PublicKey(author.device),
            },
        })
    }

    fn quick_fs(&self) -> io::Result<QuickFS> {
        let heuristics = Arc::new(Heuristics::new(Arc::clone(&self.hydrator)));
        QuickFS::new(
            Arc::clone(&self.namespace.namespace),
            Arc::clone(&self.hydrator),
            heuristics,
        )
        .with_history(Arc::clone(&self.history), self.actor)
        .map_err(io::Error::other)
    }

    fn sync_folder(&self, folder: &Path) -> io::Result<SyncFolder> {
        let namespace = Arc::clone(&self.namespace);
        let commit = Box::new(move || namespace.save_if_changed());
        SyncFolder::open(Arc::new(self.quick_fs()?), folder, &self.state, commit)
    }
}

/// The workspace namespace and the profile file that keeps it.
struct NamespaceFile {
    namespace: Arc<Mutex<Namespace>>,
    path: PathBuf,
    /// What the file holds, so an unchanged namespace is not rewritten.
    saved: Mutex<Vec<automerge::ChangeHash>>,
}

impl NamespaceFile {
    fn open(path: PathBuf, author: String) -> io::Result<Self> {
        let mut namespace = match std::fs::read(&path) {
            Ok(bytes) => Namespace::from_bytes(author, &bytes)
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                Namespace::new(author).map_err(io::Error::other)?
            }
            Err(error) => return Err(error),
        };
        let saved = if path.exists() {
            namespace.heads()
        } else {
            Vec::new()
        };
        Ok(Self {
            namespace: Arc::new(Mutex::new(namespace)),
            path,
            saved: Mutex::new(saved),
        })
    }

    #[allow(
        clippy::significant_drop_tightening,
        reason = "the saved heads stay locked across the write, so two savers never write the file out of order"
    )]
    fn save_if_changed(&self) -> io::Result<()> {
        let poisoned = || io::Error::other("workspace namespace lock poisoned");
        let mut saved = self.saved.lock().map_err(|_| poisoned())?;
        let (heads, bytes) = {
            let mut namespace = self.namespace.lock().map_err(|_| poisoned())?;
            let heads = namespace.heads();
            if heads == *saved {
                return Ok(());
            }
            (heads, namespace.save())
        };
        tilo_files::native::atomic::write_with_mode(&self.path, &bytes, 0o600)?;
        *saved = heads;
        Ok(())
    }
}

/// The folder of the workspace named `workspace`, below `home`. A name
/// that is not a plain folder name is replaced, never interpreted.
pub(super) fn folder_in(home: &Path, workspace: &str) -> PathBuf {
    let plain =
        !workspace.is_empty() && !workspace.starts_with('.') && !workspace.contains(['/', '\0']);
    home.join("Tilo")
        .join(if plain { workspace } else { "Workspace" })
}

/// The folder of the workspace named `workspace` in this user's home.
pub(super) fn folder(workspace: &str) -> io::Result<PathBuf> {
    let home = std::env::var_os("HOME")
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "HOME is missing"))?;
    Ok(folder_in(Path::new(&home), workspace))
}

#[cfg(test)]
#[path = "files_tests.rs"]
mod tests;
