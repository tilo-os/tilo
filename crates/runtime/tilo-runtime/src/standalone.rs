//! Services for a runtime profile without the Tilo desktop: the shared
//! action broker and identity service over the profile's own workspace.
//!
//! This is what the runtime serves on stock macOS or a foreign Linux
//! desktop. The desktop session hosts the same brokers with its native
//! adapters (launcher, governance, durable owner, peer network); here each
//! of those host services is absent, and the broker answers as it does for
//! any host that does not provide one.

use crate::{BLOB_SOCKET, COLLAB_SOCKET, IDENTITY_SOCKET, RuntimeServices, profile, unix};
use std::{
    io,
    path::{Path, PathBuf},
    process::{Child, Command},
    sync::{Arc, Mutex, RwLock},
};
use tilo_action_broker::{
    collab_broker::{AppActionBroker, BrokerHandle, FanoutState},
    collab_core::BrokerChannels,
    collab_outbound::PeerPublisher,
    services::{HostServices, WorkspaceAuthority},
};
use tilo_actions::remote::{
    ActionHost, ActionHostError, Caller, CataloguePage, CatalogueQuery, InvocationReceipt,
    InvocationState, InvokeRequest, Pending,
};
use tilo_desktop_model::{DesktopTree, IdMinter, RoomId, Strip, StripId, Workspace, WorkspaceId};
use tilo_identity_host::{
    broker::{IdentityBrokerHandle, WorkspaceMembership},
    provisioning::IdentityStorage,
};

/// The desktop session's workspace file. Read, never written, here: its
/// native authority fields belong to that host.
const DESKTOP_WORKSPACE: &str = "workspace.json";
/// This host's own workspace file, for a profile the desktop never served.
const STANDALONE_WORKSPACE: &str = "desktop.json";

/// A standalone service session over one profile. Drop applications first.
pub struct Standalone {
    broker: BrokerHandle,
    _identity: IdentityBrokerHandle,
    room: String,
    actor: String,
    identity_socket: PathBuf,
    blob_socket: PathBuf,
    _media: profile::MediaHost,
    _lock: advisory_lock::FileLock,
}

impl Standalone {
    /// Read document media an app in this session's room stored.
    /// # Errors
    /// Returns connection errors; `Ok(None)` when the content is not readable.
    pub fn read_media(&self, app_id: &str, hash: &str) -> io::Result<Option<Vec<u8>>> {
        profile::read_media(&self.blob_socket, &self.room, app_id, hash)
    }

    /// The live action broker, for in-process inspection.
    pub fn app_action_broker(&self) -> Option<AppActionBroker> {
        self.broker.app_action_broker()
    }
}

impl RuntimeServices for Standalone {
    type ActionHost = RuntimeActionHost;

    fn start_reporting(root: &Path, stage: &mut dyn FnMut(&str)) -> io::Result<Self> {
        stage("locking the profile");
        let lock = profile::lock(root)?;
        stage("provisioning the collaboration identity");
        let identity_dir = runtime_identity::identity_dir_from_env()?;
        tilo_identity_host::provisioning::ensure_collaboration_identity(
            &identity_dir,
            &tilo_identity_host::provisioning::collaboration_identity_paths(),
            IdentityStorage::Platform,
        )?;
        stage("loading the workspace");
        let workspace = ProfileWorkspace::open(root)?;
        let room_id = workspace.room();
        let actor = tilo_identity_host::registry::selected_identity_profile(&room_id)?
            .ok_or_else(|| io::Error::other("runtime workspace has no selected identity"))?
            .principal_id();
        stage("saving the workspace");
        workspace.adopt(&actor)?;
        let workspace = Arc::new(workspace);
        let identity_socket = root.join(IDENTITY_SOCKET);
        stage("starting the identity service");
        let identity = tilo_identity_host::broker::spawn(
            identity_socket.clone(),
            Arc::clone(&workspace) as Arc<dyn WorkspaceMembership>,
            Arc::new(()),
        )?;
        stage("starting the collaboration broker");
        let broker = tilo_action_broker::collab_broker::spawn_with_services(
            root.join(COLLAB_SOCKET),
            Some(Arc::clone(&workspace) as Arc<dyn WorkspaceAuthority>),
            PeerPublisher::disabled(),
            None,
            HostServices {
                native_certificates: Some(Arc::new(Certificates)),
                launchable_catalogue: Some(root.join("state/launchable-actions.json")),
                ..HostServices::default()
            },
            Arc::new(Mutex::new(BrokerChannels::default())),
            Arc::new(FanoutState::new()),
        )?;
        let blob_socket = root.join(BLOB_SOCKET);
        stage("opening the content store");
        let media = profile::MediaHost::start(&root.join("content"), &blob_socket)?;
        Ok(Self {
            broker,
            _identity: identity,
            room: room_id,
            actor,
            identity_socket,
            blob_socket,
            _media: media,
            _lock: lock,
        })
    }

    fn room(&self) -> &str {
        &self.room
    }

    fn actor(&self) -> &str {
        &self.actor
    }

    fn action_host(&self) -> io::Result<RuntimeActionHost> {
        self.broker
            .app_action_broker()
            .map(RuntimeActionHost)
            .ok_or_else(|| io::Error::other("runtime action broker is unavailable"))
    }

    fn launch(&self, app_id: &str, command: Command) -> io::Result<Child> {
        unix::launch(
            command,
            app_id,
            &self.room,
            &unix::ServiceSockets {
                collab: self.broker.capnp_socket(),
                identity: &self.identity_socket,
                blobs: &self.blob_socket,
            },
        )
    }
}

/// The broker's action door as one more local caller, standing for no app
/// connection.
#[derive(Clone)]
pub struct RuntimeActionHost(AppActionBroker);

impl ActionHost for RuntimeActionHost {
    fn query(
        &self,
        caller: &Caller,
        query: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionHostError>> {
        self.0.state().action_query(caller, &query)
    }

    fn invoke(
        &self,
        caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionHostError>> {
        self.0.state().action_invoke(caller, None, None, &request)
    }

    fn observe(&self, caller: &Caller, id: &tilo_actions::InvocationId) -> Option<InvocationState> {
        self.0.state().action_observe(caller, id)
    }

    fn cancel(
        &self,
        caller: &Caller,
        id: &tilo_actions::InvocationId,
    ) -> Result<(), ActionHostError> {
        self.0.state().action_cancel(caller, id)
    }
}

/// The identity store's device-certificate checks, as the broker asks them.
struct Certificates;

impl tilo_action_broker::peer_actions::CertificateAuthority for Certificates {
    fn authorize(
        &self,
        room: uuid::Uuid,
        cert: &tilo_user_identity::DeviceCert,
    ) -> Result<(), String> {
        tilo_identity_host::certificates::authorize(&room.to_string(), cert)
    }
    fn authorize_join(
        &self,
        room: uuid::Uuid,
        cert: &tilo_user_identity::DeviceCert,
    ) -> Result<(), String> {
        tilo_identity_host::certificates::authorize_join(&room.to_string(), cert)
    }
}

/// The profile's committed workspace tree.
struct ProfileWorkspace {
    tree: RwLock<DesktopTree>,
    /// Where this host keeps the tree; `None` when the desktop session's
    /// file is authoritative and read only.
    path: Option<PathBuf>,
}

impl ProfileWorkspace {
    /// The desktop session's tree when that host served this profile,
    /// otherwise this host's own, created on first start.
    fn open(root: &Path) -> io::Result<Self> {
        let desktop = root.join(DESKTOP_WORKSPACE);
        if desktop.exists() {
            // The desktop session versions this file by its native layout,
            // which only adds fields this host ignores; it is read only here.
            return Ok(Self {
                tree: RwLock::new(parse_tree(&std::fs::read(&desktop)?)?),
                path: None,
            });
        }
        let path = root.join(STANDALONE_WORKSPACE);
        let tree = match read_tree(&path) {
            Ok(tree) => tree,
            Err(error) if error.kind() == io::ErrorKind::NotFound => bootstrap(),
            Err(error) => return Err(error),
        };
        if tree.workspaces.is_empty() {
            return Err(io::Error::other("runtime profile has no workspace"));
        }
        Ok(Self {
            tree: RwLock::new(tree),
            path: Some(path),
        })
    }

    fn room(&self) -> String {
        self.tree
            .read()
            .map(|tree| {
                tree.workspaces
                    .first()
                    .map(|workspace| workspace.room.to_string())
            })
            .ok()
            .flatten()
            .unwrap_or_default()
    }

    /// Give an unowned workspace to `actor` and persist this host's tree.
    fn adopt(&self, actor: &str) -> io::Result<()> {
        let mut tree = self
            .tree
            .write()
            .map_err(|_| io::Error::other("runtime workspace lock poisoned"))?;
        let workspace = tree
            .workspaces
            .first_mut()
            .ok_or_else(|| io::Error::other("runtime profile has no workspace"))?;
        if workspace.owner.is_none() {
            if self.path.is_none() {
                return Err(io::Error::other(
                    "the desktop session's runtime workspace has no owner",
                ));
            }
            workspace.set_owner(actor.to_owned());
        }
        let Some(path) = &self.path else {
            return Ok(());
        };
        let bytes = serde_json::to_vec_pretty(&*tree).map_err(io::Error::other)?;
        drop(tree);
        atomic_file::write_with_mode(path, &bytes, 0o600)
    }
}

impl WorkspaceAuthority for ProfileWorkspace {
    fn snapshot(&self) -> Result<DesktopTree, String> {
        self.tree
            .read()
            .map(|tree| tree.clone())
            .map_err(|_| "runtime workspace lock poisoned".to_owned())
    }
}

impl WorkspaceMembership for ProfileWorkspace {
    fn with_workspace(&self, room: &RoomId, read: &mut dyn FnMut(&Workspace)) {
        if let Ok(tree) = self.tree.read()
            && let Some(workspace) = tree
                .workspaces
                .iter()
                .find(|workspace| &workspace.room == room)
        {
            read(workspace);
        }
    }
}

fn parse_tree(bytes: &[u8]) -> io::Result<DesktopTree> {
    serde_json::from_slice(bytes).map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

fn read_tree(path: &Path) -> io::Result<DesktopTree> {
    let tree = parse_tree(&std::fs::read(path)?)?;
    if tree.schema_version > tilo_desktop_model::STATE_SCHEMA_VERSION {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "workspace layout {} is newer than the {} this build understands",
                tree.schema_version,
                tilo_desktop_model::STATE_SCHEMA_VERSION
            ),
        ));
    }
    Ok(tree)
}

/// One empty home workspace with one strip, as a new desktop starts.
fn bootstrap() -> DesktopTree {
    let mut ids = IdMinter::default();
    let workspace = WorkspaceId(ids.mint());
    let strip = StripId(ids.mint());
    let home = Workspace {
        strips: vec![Strip {
            id: strip,
            name: "Main".into(),
            window_groups: Vec::new(),
        }],
        ..Workspace::new(workspace, "Home")
    };
    DesktopTree {
        workspaces: vec![home],
        active: workspace,
        active_strip: std::iter::once((workspace, strip)).collect(),
        ..DesktopTree::default()
    }
}

#[cfg(test)]
#[path = "standalone_tests.rs"]
mod tests;
