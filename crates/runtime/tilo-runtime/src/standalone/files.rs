//! The profile's workspace files, and the folder that shows them on this
//! host, where nothing mounts the workspace: `~/Tilo/<workspace>`.
//!
//! The files live in the profile as the filesystem daemon keeps them on
//! Tilo OS: a namespace, the content store the profile's document media
//! share, and a history of every version. The folder mirrors them both ways
//! ([`tilo_fs::sync_folder`]).

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
/// Workspace changes reach the folder within this.
const INTERVAL: Duration = Duration::from_secs(2);

/// Who the workspace attributes this host's versions to, and the device
/// they come from.
pub(super) struct Author {
    pub(super) person: [u8; 32],
    pub(super) device: [u8; 32],
}

/// The workspace files, kept in step with their folder until dropped.
pub(super) struct WorkspaceFiles {
    _watch: Watch,
}

impl WorkspaceFiles {
    /// Open the workspace files of `profile` over the content `store` and
    /// mirror them into `folder`.
    pub(super) fn start(
        profile: &Path,
        folder: &Path,
        store: Arc<tilo_fs::ContentStore>,
        endpoint: iroh::Endpoint,
        author: &Author,
    ) -> io::Result<Self> {
        let state = profile.join(FILES);
        crate::unix::private_directory(&state)?;
        let namespace_path = state.join(NAMESPACE);
        let namespace = Arc::new(Mutex::new(open_namespace(
            &namespace_path,
            hex::encode(author.device),
        )?));
        let history = FileHistoryStore::open(state.join(HISTORY)).map_err(io::Error::other)?;
        let actor = collab_types::FileActor {
            person: collab_types::Ed25519PublicKey(author.person),
            device: collab_types::Ed25519PublicKey(author.device),
        };
        let hydrator = {
            let _entered = tilo_rt::app().handle().enter();
            Arc::new(Hydrator::new(endpoint, store))
        };
        let heuristics = Arc::new(Heuristics::new(Arc::clone(&hydrator)));
        let fs = QuickFS::new(Arc::clone(&namespace), hydrator, heuristics)
            .with_history(Arc::new(history), actor)
            .map_err(io::Error::other)?;
        let commit = Box::new(move || save_namespace(&namespace, &namespace_path));
        let watch = SyncFolder::open(Arc::new(fs), folder, &state, commit)?.watch(INTERVAL)?;
        Ok(Self { _watch: watch })
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

fn open_namespace(path: &Path, author: String) -> io::Result<Namespace> {
    match std::fs::read(path) {
        Ok(bytes) => Namespace::from_bytes(author, &bytes)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            Namespace::new(author).map_err(io::Error::other)
        }
        Err(error) => Err(error),
    }
}

fn save_namespace(namespace: &Mutex<Namespace>, path: &Path) -> io::Result<()> {
    let bytes = namespace
        .lock()
        .map_err(|_| io::Error::other("workspace namespace lock poisoned"))?
        .save();
    atomic_file::write_with_mode(path, &bytes, 0o600)
}

#[cfg(test)]
#[path = "files_tests.rs"]
mod tests;
