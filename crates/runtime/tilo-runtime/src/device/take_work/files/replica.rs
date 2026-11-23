//! A joined workspace's files, held as a replica of the workspace's own Files
//! record and shown as a plain folder.
//!
//! A workspace's file truth is its Files app's record: the tree, one Automerge
//! document ([`tilo_fs::namespace`]), held in this device's own workspace
//! service as every member copy holds it ([`tilo_workspace_service::files`]), and the bytes its
//! files are made of, which travel beside the record on the record's own route
//! ([`tilo_workspace_service::content`]). The workspace's host keeps the
//! record with the workspace's other records and hands it whole to a device
//! that joins, and it carries content requests between the members. A device
//! taking part needs nothing else: no ticket, no peer addresses, no sync protocol
//! of its own. That is why a joined workspace's files take this path rather
//! than the device-to-device sync protocol a person's own mounts use
//! ([`tilo_fs::sync`]): a member of a hosted workspace reaches the others only
//! through the host, and the record path is the one the host already serves.
//!
//! Here this device is the Files app's windowless endpoint for the workspace,
//! as the files daemon is for a desktop: the runtime attests its own process
//! for the app and workspace. The tree lives in the profile beside
//! the profile's own workspace files; a folder mirrors it both ways
//! ([`tilo_fs::sync_folder`]), so a program that knows nothing of Tilo (a
//! command in a workspace's environment that mounts the folder) makes files
//! the workspace's members see, and sees theirs.
//!
//! The bytes a file is made of reach the other members as record content of
//! the workspace's Files app: whatever this device holds of the tree's content
//! it keeps under the workspace's scope, which is what lets it answer a
//! member asking for it, and whatever it lacks is asked of the workspace when a
//! file is read.

use std::{
    collections::{BTreeMap, HashSet},
    io,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};

use iroh_blobs::Hash;
use tilo_fs::{
    hydration::Hydrator,
    identity::PeerCredentials,
    namespace::Namespace,
    sync_folder::Watch,
    tree_sync::{FILES_APP, TreeSync},
};
use tilo_lock::MutexRecover as _;
use tilo_workspace_service::apps::route::CrdtRoute;
use tilo_workspace_service::content::RouteContent;
use tilo_workspace_service::files::WorkspaceSource;
use tilo_workspace_service::workspaces::Workspaces;

use super::{Author, Files, INTERVAL, NamespaceFile};

/// How long a read waits for content the workspace's members are asked for.
/// A folder pass waiting longer holds every other path behind it; the next
/// pass asks again.
const CONTENT_WAIT: Duration = Duration::from_secs(10);

/// How long a dropped replica waits for the writes it had under way: content
/// being offered, and the tree being kept. Both are short; a write still
/// waiting past this is left, its state as far as it got.
const STOPPING: Duration = Duration::from_secs(10);

/// What a replica of one joined workspace's files is made of.
pub(crate) struct ReplicaParts<'a> {
    /// The joined workspace.
    pub(crate) workspace: &'a str,
    /// Where the profile keeps this workspace's tree and folder baseline.
    pub(crate) state: &'a Path,
    /// The folder that mirrors the workspace.
    pub(crate) folder: &'a Path,
    /// The profile's content store, the one its record content is kept in.
    pub(crate) store: Arc<tilo_fs::ContentStore>,
    pub(crate) author: &'a Author,
    /// This device's key and its person's, as the workspace knows them.
    pub(crate) keys: ([u8; 32], [u8; 32]),
    pub(crate) admission: Arc<tilo_fs::admission::FileAdmission>,
    /// The service whose member copy holds the workspace's tree.
    pub(crate) workspaces: Workspaces,
    /// The profile's identity service, which hands over the record's keys.
    pub(crate) identity: PathBuf,
    /// The profile's record-content service.
    pub(crate) content: Arc<RouteContent>,
}

/// One joined workspace's files, synced while this value lives.
pub(crate) struct ReplicaFiles {
    folder: PathBuf,
    _watch: Watch,
    /// Held until this replica drops: its drop stops the tree and waits for its last keep, so no
    /// write lands in the tree's state after the replica has gone.
    tree: Option<TreeSync>,
    keeper: Option<tokio::task::JoinHandle<()>>,
}

impl Drop for ReplicaFiles {
    fn drop(&mut self) {
        // What a replica had under way writes into the tree's state: content being offered for a
        // file, and the tree's own documents being kept. They are stopped and waited for here, so
        // a daemon whose replicas are dropped has finished writing, not merely stopped asking.
        // The folder's thread is joined as its field drops.
        let (keeper, tree) = (self.keeper.take(), self.tree.take());
        if let Some(keeper) = &keeper {
            keeper.abort();
        }
        if tilo_rt::in_async_context() || tilo_rt::is_ui_thread() {
            // A thread that must not wait cannot finish them: the keeper is aborted, and the
            // tree's task is cancelled as its field drops, its last write as far as it got.
            return;
        }
        let _ = tilo_rt::app().block_on(async move {
            if let Some(keeper) = keeper {
                let _ = tokio::time::timeout(STOPPING, keeper).await;
            }
            if let Some(tree) = tree {
                tree.stop(STOPPING).await;
            }
        });
    }
}

impl ReplicaFiles {
    /// Hold `parts.workspace`'s files: the tree kept in `parts.state`,
    /// converged with the workspace's Files records in the member copy, and
    /// mirrored at `parts.folder`.
    ///
    /// # Errors
    /// The state or the folder cannot be opened.
    pub(crate) fn start(parts: ReplicaParts<'_>) -> io::Result<Self> {
        let route = CrdtRoute::app(FILES_APP, parts.workspace);
        let source = Arc::new(WorkspaceSource::new(
            Arc::clone(&parts.content),
            parts.workspace,
            CONTENT_WAIT,
        ));
        let workspace = collab_types::WorkspaceId::parse(parts.workspace).ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "invalid Files workspace")
        })?;
        tilo_files::native::private::directory(parts.state)?;
        let tree = crate::device::workspaces::block_on_owned({
            let (copies, kept_at) = (parts.workspaces, parts.state.join(super::NAMESPACE));
            let (store, content) = (Arc::clone(&parts.store), Arc::clone(&parts.content));
            async move {
                tilo_workspace_service::files::start(
                    &copies,
                    workspace,
                    &kept_at,
                    parts.keys,
                    Some(store),
                    Some(content),
                )
                .await
            }
        })?;
        let keys = tilo_fs::hydration::FileKeys::new(workspace, parts.identity);
        let files = Files::open(
            parts.state,
            NamespaceFile::of_tree(&tree),
            parts.store,
            parts.author,
            Some((source, keys)),
        );
        std::fs::create_dir_all(parts.folder)?;
        let watch = files
            .sync_folder(parts.folder, Some(parts.admission))?
            .watch(INTERVAL)?;
        let keeper = tilo_rt::app().spawn(keep_content(
            Arc::clone(&tree.namespace),
            Arc::clone(&files.hydrator),
            parts.content,
            route,
        ));
        Ok(Self {
            folder: parts.folder.to_path_buf(),
            _watch: watch,
            tree: Some(tree),
            keeper: Some(keeper),
        })
    }

    /// The folder that mirrors the workspace.
    pub(crate) fn folder(&self) -> &Path {
        &self.folder
    }

    /// Where the folder's sync stands.
    pub(in crate::device) fn status(&self) -> tilo_fs::tree_sync::Status {
        self.tree
            .as_ref()
            .map_or_else(tilo_fs::tree_sync::Status::default, TreeSync::status)
    }
}

/// This device as a joined workspace knows it.
pub(crate) type Credentials = Box<dyn Fn(&str) -> io::Result<PeerCredentials> + Send + Sync>;

/// What every replica of this profile shares.
pub(crate) struct ReplicaHost {
    /// The profile, whose `files/workspaces/<workspace>` keeps each tree.
    pub(crate) profile: PathBuf,
    /// The home whose `Tilo` folder shows each workspace.
    pub(crate) home: PathBuf,
    /// The folder of the profile's own workspace, which no joined workspace
    /// takes.
    pub(crate) own_folder: Option<PathBuf>,
    pub(crate) store: Arc<tilo_fs::ContentStore>,
    pub(crate) author: Author,
    pub(crate) credentials: Credentials,
    pub(crate) identity: PathBuf,
    pub(crate) content: Arc<RouteContent>,
    /// Attests this process as each workspace's Files endpoint.
    pub(crate) apps: Arc<crate::apps::Apps>,
}

/// The files of every workspace this profile joined, by workspace.
pub(crate) struct Replicas {
    host: ReplicaHost,
    held: Mutex<BTreeMap<String, ReplicaFiles>>,
    workspaces: std::sync::Weak<crate::device::workspaces::ServiceWorkspaces>,
}

impl Replicas {
    pub(crate) fn new(
        host: ReplicaHost,
        workspaces: std::sync::Weak<crate::device::workspaces::ServiceWorkspaces>,
    ) -> Self {
        Self {
            host,
            workspaces,
            held: Mutex::default(),
        }
    }

    /// Hold the files of `workspace`, called `name`, unless they are held
    /// already: the folder that mirrors them.
    ///
    /// # Errors
    /// This device's credentials for the workspace, its state or its folder
    /// cannot be had.
    #[allow(
        clippy::significant_drop_tightening,
        reason = "the held replicas stay locked while one starts, so two holds of a workspace never start two"
    )]
    pub(crate) fn hold(&self, workspace: &str, name: &str) -> io::Result<PathBuf> {
        let mut held = self.held.lock_recover();
        if let Some(replica) = held.get(workspace) {
            return Ok(replica.folder().to_path_buf());
        }
        let folder = {
            let taken = |folder: &Path| {
                self.host.own_folder.as_deref() == Some(folder)
                    || held.values().any(|replica| replica.folder() == folder)
            };
            let plain = super::folder_in(&self.host.home, name);
            if taken(&plain) {
                let short: String = workspace.chars().take(8).collect();
                super::folder_in(&self.host.home, &format!("{name} {short}"))
            } else {
                plain
            }
        };
        let credentials = (self.host.credentials)(workspace)?;
        let id = collab_types::WorkspaceId::parse(workspace)
            .ok_or_else(|| io::Error::other("invalid Files workspace"))?;
        let service = self
            .workspaces
            .upgrade()
            .ok_or_else(|| io::Error::other("the workspace service closed"))?;
        let workspaces = self.workspaces.clone();
        let admission = tilo_fs::admission::FileAdmission::new(
            &hex::encode(credentials.user_public_key()),
            Box::new(move || {
                workspaces
                    .upgrade()?
                    .workspace(id)
                    .map(|workspace| workspace.policy)
            }),
        )
        .ok_or_else(|| io::Error::other("invalid Files identity"))?;
        let state = self
            .host
            .profile
            .join(super::FILES)
            .join("workspaces")
            .join(workspace);
        self.host.apps.serve_endpoint(FILES_APP, workspace);
        let started = ReplicaFiles::start(ReplicaParts {
            workspace,
            state: &state,
            folder: &folder,
            store: Arc::clone(&self.host.store),
            author: &self.host.author,
            keys: (
                credentials.device_public_key(),
                credentials.user_public_key(),
            ),
            admission: Arc::new(admission),
            workspaces: service.copies().clone(),
            identity: self.host.identity.clone(),
            content: Arc::clone(&self.host.content),
        });
        match started {
            Ok(replica) => {
                held.insert(workspace.to_owned(), replica);
                Ok(folder)
            }
            Err(error) => {
                self.host.apps.stop_serving(FILES_APP, workspace);
                Err(error)
            }
        }
    }

    /// The folder mirroring `workspace`'s files, while they are held.
    pub(crate) fn folder(&self, workspace: &str) -> Option<PathBuf> {
        self.held
            .lock_recover()
            .get(workspace)
            .map(|replica| replica.folder().to_path_buf())
    }

    /// Where the sync of `workspace`'s folder stands, while its files are held.
    pub(in crate::device) fn status(&self, workspace: &str) -> Option<tilo_fs::tree_sync::Status> {
        self.held
            .lock_recover()
            .get(workspace)
            .map(ReplicaFiles::status)
    }
}

impl Drop for Replicas {
    fn drop(&mut self) {
        for workspace in self.held.lock_recover().keys() {
            self.host.apps.stop_serving(FILES_APP, workspace);
        }
    }
}

/// Keep the content of every file the tree holds that this device has whole
/// under the workspace's scope, whenever the tree changes: what lets this
/// device answer a member that asks the workspace for it. A file not kept yet
/// (its bytes not here yet, or the store not taking them) is tried again
/// after a wait that doubles up to [`KEEP_AGAIN_AT_MOST`], tree change or not:
/// a tree that stops changing would otherwise leave it unanswered for good.
async fn keep_content(
    namespace: Arc<Mutex<Namespace>>,
    hydrator: Arc<Hydrator>,
    content: Arc<RouteContent>,
    route: CrdtRoute,
) {
    let mut changes = namespace.lock_recover().subscribe_changes();
    let mut kept: HashSet<Hash> = HashSet::new();
    let mut again = INTERVAL;
    loop {
        let mut lacking = false;
        let held: Vec<Hash> = {
            let namespace = namespace.lock_recover();
            namespace
                .live_node_ids()
                .iter()
                .filter_map(|id| namespace.node_hash(id))
                .collect()
        };
        for file in held {
            if kept.contains(&file) {
                continue;
            }
            if keep_one(&hydrator, &content, &route, file).await {
                kept.insert(file);
            } else {
                lacking = true;
            }
        }
        let waited = if lacking {
            tokio::time::timeout(again, changes.changed()).await.ok()
        } else {
            Some(changes.changed().await)
        };
        match waited {
            Some(Err(_)) => return,
            Some(Ok(())) => again = INTERVAL,
            None => again = again.saturating_mul(2).min(KEEP_AGAIN_AT_MOST),
        }
    }
}

/// The longest a file whose content was not kept waits to be tried again.
const KEEP_AGAIN_AT_MOST: Duration = Duration::from_secs(60);

/// Keep every blob of `file`'s content under `route`'s scope; whether all of
/// it is local, and kept.
async fn keep_one(
    hydrator: &Hydrator,
    content: &Arc<RouteContent>,
    route: &CrdtRoute,
    file: Hash,
) -> bool {
    let Some(blobs) = hydrator.content_blobs(file).await else {
        return false;
    };
    for blob in blobs {
        let Some(bytes) = hydrator.local_content_blob(file, blob).await else {
            return false;
        };
        let (content, route) = (Arc::clone(content), route.clone());
        let kept = tokio::task::spawn_blocking(move || {
            content.offer(&route, blob.to_hex().as_str(), &bytes)
        })
        .await;
        match kept {
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                tracing::debug!(%error, %blob, "a file's content is not kept for the workspace");
                return false;
            }
            Err(error) => {
                tracing::debug!(%error, "keeping a file's content stopped");
                return false;
            }
        }
    }
    true
}

#[cfg(test)]
#[path = "replica_tests.rs"]
mod tests;
