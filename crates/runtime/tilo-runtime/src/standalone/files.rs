//! The profile's workspace files, and how this host shows them where the
//! desktop does not: at `~/Tilo/<workspace>`.
//!
//! The files live in the profile as the filesystem daemon keeps them on
//! Tilo OS: a namespace holding the tree and every version of every record,
//! and the content store the profile's document media share. On Linux with a
//! usable FUSE the workspace is mounted there ([`mount`]); everywhere else a
//! plain folder mirrors it both ways ([`tilo_fs::sync_folder`]).
//!
//! A workspace this profile joined has its files too, held as a replica of
//! the workspace's own Files record ([`replica`]).

#[cfg(target_os = "linux")]
mod mount;
mod replica;
mod serve;

pub(super) use replica::{ReplicaHost, Replicas};

/// The joined workspaces' files, once they are held: a member taking a
/// workspace's work is set up before the broker their records travel on runs.
pub(super) type ReplicaSlot = Arc<std::sync::OnceLock<Arc<Replicas>>>;

use std::{
    io,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};
use tilo_fs::{
    fs::QuickFS,
    hydration::Hydrator,
    kept_documents::KeptDocuments,
    namespace::Namespace,
    smart::Heuristics,
    sync_folder::{SyncFolder, Watch},
};

/// The profile directory holding the workspace files' state.
const FILES: &str = "files";
const NAMESPACE: &str = "namespace";
/// Workspace changes reach the folder, and are persisted, within this.
const INTERVAL: Duration = Duration::from_secs(2);

/// Who the workspace attributes this host's versions to, and the device
/// they come from.
#[derive(Clone, Copy)]
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
        let state = profile.join(FILES);
        crate::unix::private_directory(&state)?;
        let namespace = NamespaceFile::open(&state.join(NAMESPACE), hex::encode(author.device))?;
        let files = Files::open(&state, namespace, store, endpoint, author, None);
        let server = runtime_fs::workspace_server::serve_workspace(
            &profile.join(crate::FILES_SOCKET),
            Arc::new(serve::Backend(Arc::new(files.quick_fs()))),
            tilo_rt::app().handle().clone(),
        )?;
        let view = match projection {
            Projection::Folder => View::Folder {
                _watch: files.sync_folder(folder, None)?.watch(INTERVAL)?,
            },
            #[cfg(target_os = "linux")]
            Projection::Mount => {
                // Changes made in the folder while nothing was mounted over
                // it enter the workspace before the mount hides them.
                let mut sync = files.sync_folder(folder, None)?;
                while sync.reconcile()?.more() {}
                match mount::Mount::start(files.quick_fs(), folder, Arc::clone(&files.namespace)) {
                    Ok(mount) => View::Mount { _mount: mount },
                    Err(error) => {
                        tracing::warn!(%error, "the workspace cannot be mounted; syncing a folder instead");
                        View::Folder {
                            _watch: files.sync_folder(folder, None)?.watch(INTERVAL)?,
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
    hydrator: Arc<Hydrator>,
    actor: fs_protocol::FileActor,
}

impl Files {
    /// The files of `namespace`, their folder state kept in `state` and their bytes in `store`;
    /// what this device lacks is asked of `source` when one is named.
    fn open(
        state: &Path,
        namespace: NamespaceFile,
        store: Arc<tilo_fs::ContentStore>,
        endpoint: iroh::Endpoint,
        author: &Author,
        source: Option<(
            Arc<dyn tilo_fs::hydration::ContentSource>,
            tilo_fs::hydration::FileKeys,
        )>,
    ) -> Self {
        let hydrator = {
            let _entered = tilo_rt::app().handle().enter();
            let hydrator = Hydrator::new(endpoint, store);
            Arc::new(match source {
                Some((source, keys)) => hydrator.with_source(source).with_file_keys(keys),
                None => hydrator,
            })
        };
        Self {
            state: state.to_path_buf(),
            namespace: Arc::new(namespace),
            hydrator,
            actor: fs_protocol::FileActor {
                person: collab_types::Ed25519PublicKey(author.person),
                device: collab_types::Ed25519PublicKey(author.device),
            },
        }
    }

    fn quick_fs(&self) -> QuickFS {
        let heuristics = Arc::new(Heuristics::new(Arc::clone(&self.hydrator)));
        QuickFS::new(
            Arc::clone(&self.namespace.namespace),
            Arc::clone(&self.hydrator),
            heuristics,
        )
        .with_actor(self.actor)
        // Every change is journaled on disk before the request that made it is answered.
        .with_change_journal(Arc::clone(&self.namespace.journal))
    }

    fn sync_folder(
        &self,
        folder: &Path,
        admission: Option<Arc<tilo_fs::admission::FileAdmission>>,
    ) -> io::Result<SyncFolder> {
        let namespace = Arc::clone(&self.namespace);
        let commit = Box::new(move || namespace.save_if_changed());
        let fs = self.quick_fs();
        let fs = match admission {
            Some(admission) => fs.with_admission(admission),
            None => fs,
        };
        SyncFolder::open(Arc::new(fs), folder, &self.state, commit)
    }
}

/// The workspace namespace, kept document by document in the profile's state directory: each
/// document with the journal of the changes it has made since its last save.
struct NamespaceFile {
    namespace: Arc<Mutex<Namespace>>,
    journal: tilo_fs::fs::ChangeJournal,
    save: Box<dyn Fn() -> io::Result<()> + Send + Sync>,
}

impl NamespaceFile {
    fn open(dir: &Path, author: String) -> io::Result<Self> {
        let (kept, documents) = KeptDocuments::open(dir)?;
        let (mut namespace, read) = if documents.is_empty() {
            (
                Namespace::new(author).map_err(io::Error::other)?,
                std::collections::BTreeMap::new(),
            )
        } else {
            Namespace::from_documents_read(author, documents)
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?
        };
        kept.opened(&mut namespace, read)?;
        let (namespace, kept) = (Arc::new(Mutex::new(namespace)), Arc::new(kept));
        let journaled = Arc::clone(&kept);
        let saved = Arc::clone(&namespace);
        Ok(Self {
            namespace,
            journal: Arc::new(move |namespace| journaled.journal(namespace)),
            save: Box::new(move || kept.keep(&saved)),
        })
    }

    /// A workspace tree's namespace, which its sync journals and keeps itself.
    fn of_tree(tree: &tilo_fs::tree_sync::TreeSync) -> Self {
        Self {
            namespace: Arc::clone(&tree.namespace),
            journal: tree.journal(),
            save: Box::new(|| Ok(())),
        }
    }

    /// Save each document that changed since its last save; the journals already hold every
    /// change, so this only compacts them.
    fn save_if_changed(&self) -> io::Result<()> {
        (self.save)()
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
    let home = tilo_paths::home()
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "HOME is missing"))?;
    Ok(folder_in(&home, workspace))
}

#[cfg(test)]
#[path = "files_tests.rs"]
mod tests;
