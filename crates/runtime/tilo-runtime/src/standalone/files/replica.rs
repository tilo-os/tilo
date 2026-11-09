//! A joined workspace's files, held as a replica of the workspace's own Files
//! record and shown as a plain folder.
//!
//! A workspace's file truth is its Files app's record: the tree, one Automerge
//! document ([`tilo_fs::namespace`]), published on the workspace's session by
//! every device that holds it ([`tilo_fs::tree_record`]), and the bytes its
//! files are made of, which travel beside the record on the record's own route
//! ([`tilo_action_broker::record_content`]). The workspace's host keeps the
//! record with the workspace's other records and hands it whole to a device
//! that joins, and it carries content requests between the members. A device
//! taking part needs nothing else: no ticket, no peer addresses, no sync plane
//! of its own. That is why a joined workspace's files take this path rather
//! than the device-to-device sync plane a person's own mounts use
//! ([`tilo_fs::sync`]): a member of a hosted workspace reaches the others only
//! through the host, and the record path is the one the host already serves.
//!
//! Here this device is the Files app's windowless endpoint for the workspace,
//! as the files daemon is for a desktop: the runtime attests its own process
//! for the app and workspace, and the publisher joins the record's route on
//! the profile's collaboration socket. The tree lives in the profile beside
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

use bytes::Bytes;
use iroh_blobs::Hash;
use tilo_action_broker::{
    record_content::{ContentState, RouteContent},
    route::CrdtRoute,
};
use tilo_fs::{
    hydration::{ContentSource, Hydrator},
    identity::PeerCredentials,
    namespace::Namespace,
    sync_folder::Watch,
    tree_record::FILES_APP,
};
use tilo_lock::MutexRecover as _;

use super::{Author, Files, INTERVAL};

/// How long a read waits for content the workspace's members are asked for.
/// A folder pass waiting longer holds every other path behind it; the next
/// pass asks again.
const CONTENT_WAIT: Duration = Duration::from_secs(10);

/// What a replica of one joined workspace's files is made of.
pub(in crate::standalone) struct ReplicaParts<'a> {
    /// The joined workspace.
    pub(in crate::standalone) workspace: &'a str,
    /// Where the profile keeps this workspace's tree and folder baseline.
    pub(in crate::standalone) state: &'a Path,
    /// The folder that mirrors the workspace.
    pub(in crate::standalone) folder: &'a Path,
    /// The profile's content store, the one its record content is kept in.
    pub(in crate::standalone) store: Arc<tilo_fs::ContentStore>,
    pub(in crate::standalone) endpoint: iroh::Endpoint,
    pub(in crate::standalone) author: &'a Author,
    /// This device as the workspace knows it.
    pub(in crate::standalone) credentials: PeerCredentials,
    /// The profile's collaboration socket, where the record's route is.
    pub(in crate::standalone) collab: PathBuf,
    /// The profile's identity service, which hands over the record's keys.
    pub(in crate::standalone) identity: PathBuf,
    /// The profile's record-content service.
    pub(in crate::standalone) content: Arc<RouteContent>,
}

/// One joined workspace's files, synced while this value lives.
pub(in crate::standalone) struct ReplicaFiles {
    folder: PathBuf,
    _watch: Watch,
    tasks: Vec<tokio::task::JoinHandle<()>>,
}

impl Drop for ReplicaFiles {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}

impl ReplicaFiles {
    /// Hold `parts.workspace`'s files: the tree kept in `parts.state`,
    /// published and received on the workspace's Files record, and mirrored
    /// at `parts.folder`.
    ///
    /// # Errors
    /// The state or the folder cannot be opened.
    pub(in crate::standalone) fn start(parts: ReplicaParts<'_>) -> io::Result<Self> {
        let route = CrdtRoute::app(FILES_APP, parts.workspace);
        let source = Arc::new(RouteSource {
            content: Arc::clone(&parts.content),
            route: route.clone(),
        });
        let files = Files::open(
            parts.state,
            parts.store,
            parts.endpoint,
            parts.author,
            Some(source),
        )?;
        std::fs::create_dir_all(parts.folder)?;
        let watch = files.sync_folder(parts.folder)?.watch(INTERVAL)?;
        let namespace = Arc::clone(&files.namespace.namespace);
        let runtime = tilo_rt::app().handle().clone();
        let _entered = runtime.enter();
        let publisher = tilo_fs::tree_record::spawn(
            Arc::clone(&namespace),
            parts.workspace.to_owned(),
            parts.credentials,
            parts.collab,
            parts.identity,
        );
        let keeper = tokio::spawn(keep_content(
            namespace,
            Arc::clone(&files.hydrator),
            parts.content,
            route,
        ));
        Ok(Self {
            folder: parts.folder.to_path_buf(),
            _watch: watch,
            tasks: vec![publisher, keeper],
        })
    }

    /// The folder that mirrors the workspace.
    pub(in crate::standalone) fn folder(&self) -> &Path {
        &self.folder
    }
}

/// This device as a joined workspace knows it.
pub(in crate::standalone) type Credentials =
    Box<dyn Fn(&str) -> io::Result<PeerCredentials> + Send + Sync>;

/// What every replica of this profile shares.
pub(in crate::standalone) struct ReplicaHost {
    /// The profile, whose `files/workspaces/<workspace>` keeps each tree.
    pub(in crate::standalone) profile: PathBuf,
    /// The home whose `Tilo` folder shows each workspace.
    pub(in crate::standalone) home: PathBuf,
    /// The folder of the profile's own workspace, which no joined workspace
    /// takes.
    pub(in crate::standalone) own_folder: Option<PathBuf>,
    pub(in crate::standalone) store: Arc<tilo_fs::ContentStore>,
    pub(in crate::standalone) endpoint: iroh::Endpoint,
    pub(in crate::standalone) author: Author,
    pub(in crate::standalone) credentials: Credentials,
    pub(in crate::standalone) collab: PathBuf,
    pub(in crate::standalone) identity: PathBuf,
    pub(in crate::standalone) content: Arc<RouteContent>,
    /// Attests this process as each workspace's Files endpoint.
    pub(in crate::standalone) apps: Arc<crate::apps::Apps>,
}

/// The files of every workspace this profile joined, by workspace.
pub(in crate::standalone) struct Replicas {
    host: ReplicaHost,
    held: Mutex<BTreeMap<String, ReplicaFiles>>,
}

impl Replicas {
    pub(in crate::standalone) fn new(host: ReplicaHost) -> Self {
        Self {
            host,
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
    pub(in crate::standalone) fn hold(&self, workspace: &str, name: &str) -> io::Result<PathBuf> {
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
            endpoint: self.host.endpoint.clone(),
            author: &self.host.author,
            credentials,
            collab: self.host.collab.clone(),
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
    pub(in crate::standalone) fn folder(&self, workspace: &str) -> Option<PathBuf> {
        self.held
            .lock_recover()
            .get(workspace)
            .map(|replica| replica.folder().to_path_buf())
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
/// device answer a member that asks the workspace for it.
async fn keep_content(
    namespace: Arc<Mutex<Namespace>>,
    hydrator: Arc<Hydrator>,
    content: Arc<RouteContent>,
    route: CrdtRoute,
) {
    let mut changes = namespace.lock_recover().subscribe_changes();
    let mut kept: HashSet<Hash> = HashSet::new();
    loop {
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
            }
        }
        if changes.changed().await.is_err() {
            return;
        }
    }
}

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

/// The workspace's Files record route, as the source of the bytes this device
/// lacks: kept here under the workspace's scope already, or asked of the
/// workspace's members through its host.
struct RouteSource {
    content: Arc<RouteContent>,
    route: CrdtRoute,
}

impl ContentSource for RouteSource {
    fn fetch(&self, hash: Hash) -> futures_util::future::BoxFuture<'static, Option<Bytes>> {
        let (content, route) = (Arc::clone(&self.content), self.route.clone());
        Box::pin(async move {
            let hash = hash.to_hex();
            let mut news = content.subscribe(&route);
            {
                let (content, route, hash) = (Arc::clone(&content), route.clone(), hash.clone());
                tokio::task::spawn_blocking(move || content.want(&route, &hash))
                    .await
                    .ok()?;
            }
            tokio::time::timeout(CONTENT_WAIT, async {
                while let Some(news) = news.recv().await {
                    if news.hash != hash {
                        continue;
                    }
                    match news.state {
                        ContentState::Resolved => return Some(Bytes::from(news.bytes)),
                        ContentState::Unavailable => return None,
                        _ => {}
                    }
                }
                None
            })
            .await
            .ok()
            .flatten()
        })
    }
}

#[cfg(test)]
#[path = "replica_tests.rs"]
mod tests;
