//! The files of a workspace whose work this device takes, held as a replica of the workspace's
//! own Files record ([`replica`]) and mirrored at a folder where its shells run.

mod replica;

pub(crate) use replica::{ReplicaHost, Replicas};

/// The workspaces' files, once they are held: a member taking a workspace's work is set up
/// before the broker their records travel on runs. Held weakly: the shells that ask for a
/// workspace's folder keep no replica alive past the daemon that holds them.
pub(crate) type ReplicaSlot = Arc<std::sync::OnceLock<std::sync::Weak<Replicas>>>;

use std::{
    io,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};
use tilo_fs::{
    fs::QuickFS, hydration::Hydrator, namespace::Namespace, smart::Heuristics,
    sync_folder::SyncFolder,
};

/// The profile directory holding the workspace files' state.
const FILES: &str = "files";
/// Workspace changes reach the folder, and are persisted, within this.
const INTERVAL: Duration = Duration::from_secs(2);

/// Who the workspace attributes this host's versions to, and the device
/// they come from.
#[derive(Clone, Copy)]
pub(crate) struct Author {
    pub(crate) person: [u8; 32],
    pub(crate) device: [u8; 32],
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
        author: &Author,
        source: Option<(
            Arc<dyn tilo_fs::hydration::ContentSource>,
            tilo_fs::hydration::FileKeys,
        )>,
    ) -> Self {
        let hydrator = {
            let _entered = tilo_rt::app().handle().enter();
            let hydrator = Hydrator::new(store);
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
        // The tree's sync keeps every change it journals: a folder's commit has nothing to add.
        let commit = Box::new(|| Ok(()));
        let fs = self.quick_fs();
        let fs = match admission {
            Some(admission) => fs.with_admission(admission),
            None => fs,
        };
        SyncFolder::open(Arc::new(fs), folder, &self.state, commit)
    }
}

/// A workspace tree's namespace, which its sync journals and keeps itself.
struct NamespaceFile {
    namespace: Arc<Mutex<Namespace>>,
    journal: tilo_fs::fs::ChangeJournal,
}

impl NamespaceFile {
    fn of_tree(tree: &tilo_fs::files_sync::StoreSync) -> Self {
        Self {
            namespace: Arc::clone(&tree.namespace),
            journal: tree.journal(),
        }
    }
}

/// The folder of the workspace named `workspace`, below `home`. A name
/// that is not a plain folder name is replaced, never interpreted.
pub(crate) fn folder_in(home: &Path, workspace: &str) -> PathBuf {
    let plain =
        !workspace.is_empty() && !workspace.starts_with('.') && !workspace.contains(['/', '\0']);
    home.join("Tilo")
        .join(if plain { workspace } else { "Workspace" })
}
