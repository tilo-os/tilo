//! Services for a runtime profile without the Tilo desktop: the shared
//! action broker and identity service over the profile's own workspace.
//!
//! This is what the runtime serves on stock macOS or a foreign Linux
//! desktop. The desktop session hosts the same brokers with its native
//! adapters (launcher, governance, durable owner, peer network). Here the
//! profile launches its installed apps through the platform launcher
//! ([`crate::apps`]) and records the grants people give apps for other
//! apps' actions in its own workspace; every other host service is absent,
//! and the broker answers as it does for any host that does not provide one.
//! What this host keeps durably is in the profile: grants, effect and task
//! records in its own workspace file, the invocation journal and
//! collaboration history.

use crate::{BLOB_SOCKET, COLLAB_SOCKET, IDENTITY_SOCKET, RuntimeServices, apps, profile, unix};
use std::{
    io,
    path::{Path, PathBuf},
    process::{Child, Command},
    sync::{Arc, Mutex, RwLock},
};
use tilo_action_broker::{
    collab_broker::{AppActionBroker, BrokerHandle, FanoutState},
    collab_core::BrokerChannels,
    collab_history::CollabHistoryStore,
    collab_outbound::PeerPublisher,
    services::{
        AppActionGrants, HostServices, LauncherServices, LegacyWorkspaceProjection,
        WorkspaceAuthority,
    },
};
use tilo_actions::remote::{
    ActionHost, ActionHostError, Caller, CataloguePage, CatalogueQuery, InvocationReceipt,
    InvocationState, InvokeRequest, Pending,
};
use tilo_desktop_model::{
    DesktopTree, IdMinter, RoomId, Strip, StripId, Workspace, WorkspaceId, WorkspaceTaskStatus,
};
use tilo_identity_host::{
    broker::{IdentityBrokerHandle, WorkspaceMembership},
    provisioning::IdentityStorage,
};

/// The desktop session's workspace file. Read, never written, here: its
/// native authority fields belong to that host.
const DESKTOP_WORKSPACE: &str = "workspace.json";
/// This host's own workspace file, for a profile the desktop never served.
const STANDALONE_WORKSPACE: &str = "desktop.json";
/// The broker's durable invocation journal: a retried invocation id never
/// starts twice, across restarts too.
const INVOCATION_JOURNAL: &str = "invocations";
/// Durable collaboration history, where the desktop session keeps it under
/// the profile's `XDG_STATE_HOME`.
const COLLABORATION_HISTORY: &str = "state/tilo/collaboration/history";

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
        tilo_identity_host::provisioning::ensure_foreign_collaboration_identity(
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
        let apps = Arc::new(apps::Apps::from_env(root, &room_id));
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
                launcher: Some(Arc::clone(&apps) as Arc<dyn LauncherServices>),
                app_action_access: Some(apps.access_resolver()),
                app_action_grants: Some(Arc::new(ProfileGrants {
                    workspace: Arc::clone(&workspace),
                    apps,
                })),
                legacy_workspace: Some(Arc::clone(&workspace) as Arc<dyn LegacyWorkspaceProjection>),
                ..HostServices::default()
            },
            Arc::new(Mutex::new(channels(root))),
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

/// The broker's channel state, with the profile's durable collaboration
/// history. Without it scrollback is limited to memory, as in the desktop
/// session, and the host still serves.
fn channels(root: &Path) -> BrokerChannels {
    let mut channels = BrokerChannels::default();
    match CollabHistoryStore::open(root.join(COLLABORATION_HISTORY)) {
        Ok(history) => channels.history = Some(history),
        Err(error) => tracing::warn!(
            %error,
            "durable collaboration history unavailable; scrollback is limited to memory"
        ),
    }
    channels
}

/// The profile's committed workspace tree.
struct ProfileWorkspace {
    root: PathBuf,
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
                root: root.to_path_buf(),
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
            root: root.to_path_buf(),
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

    /// Whether this host governs the tree: it keeps its own file, rather
    /// than reading the desktop session's.
    fn governs(&self) -> bool {
        self.path.is_some()
    }

    /// Apply `change` to this host's own tree and persist it. `None`, with
    /// the tree unchanged, when this host does not govern it, `change`
    /// declines, or the result cannot be saved.
    fn update<R>(&self, change: impl FnOnce(&mut DesktopTree) -> Option<R>) -> Option<R> {
        let path = self.path.as_ref()?;
        let mut tree = self.tree.write().ok()?;
        let before = tree.clone();
        let result = change(&mut tree)?;
        let saved = serde_json::to_vec_pretty(&*tree)
            .map_err(io::Error::other)
            .and_then(|bytes| atomic_file::write_with_mode(path, &bytes, 0o600));
        if let Err(error) = saved {
            *tree = before;
            drop(tree);
            tracing::warn!(%error, "runtime workspace change not saved; undone");
            return None;
        }
        drop(tree);
        Some(result)
    }

    /// Apply `change` to the workspace of `room` and persist it, refused
    /// when this host does not govern the tree.
    fn record(
        &self,
        room: &str,
        change: impl FnOnce(&mut DesktopTree, WorkspaceId) -> Option<()>,
    ) -> Result<(), String> {
        if !self.governs() {
            return Err("the desktop session's workspace is read only in the runtime".into());
        }
        self.update(|tree| {
            let workspace = tree
                .workspaces
                .iter()
                .find(|workspace| workspace.room.to_string() == room)?
                .id;
            change(tree, workspace)
        })
        .ok_or_else(|| format!("room {room} has no such record target, or it was not saved"))
    }
}

/// Grants people give applications for other apps' actions, recorded in
/// this host's own workspace by the person who gave them and noted in its
/// history. A profile adopted from the desktop session is that host's to
/// govern: nothing is granted or written here.
struct ProfileGrants {
    workspace: Arc<ProfileWorkspace>,
    apps: Arc<apps::Apps>,
}

impl AppActionGrants for ProfileGrants {
    fn may_grant(&self, room: &str, person: &str) -> bool {
        self.workspace.governs()
            && self.workspace.tree.read().is_ok_and(|tree| {
                tree.workspaces
                    .iter()
                    .find(|workspace| workspace.room.to_string() == room)
                    .is_some_and(|workspace| {
                        workspace.may_administer_at(&person.to_owned(), unix_now())
                    })
            })
    }

    fn record(
        &self,
        request: &tilo_action_broker::app_actions::AppActionConsentRequest,
        capability: collab_types::caps::CapOverride,
        answer: tilo_action_broker::app_actions::AppActionConsent,
    ) -> Option<String> {
        if !self.workspace.governs() {
            return None;
        }
        let context = tilo_desktop_model::grants::app_action_grant_context(
            &request.room,
            &self.apps.display_name(&request.app_id),
            &self.apps.display_name(&request.target_app),
            &request.action_label,
            answer.lifetime(),
        );
        let now = unix_now();
        self.workspace.update(|tree| {
            let workspace = tree
                .workspaces
                .iter()
                .find(|workspace| workspace.room.to_string() == request.room)?
                .id;
            let stored = tree.grant_in_workspace_at(workspace, &request.person, capability, now)?;
            tree.append_workspace_effect_receipt_at(
                workspace,
                tilo_actions::InvocationId::new(),
                &request.person,
                context,
                tilo_actions::EffectReceiptStatus::Succeeded,
                i64::try_from(now).unwrap_or(i64::MAX),
            );
            Some(stored.id)
        })
    }

    fn revoke(&self, room: &str, capability: &str) {
        self.workspace.update(|tree| {
            tree.workspaces
                .iter_mut()
                .find(|workspace| workspace.room.to_string() == room)?
                .revoke_capability(capability)
                .then_some(())
        });
    }
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}

/// Effect and task records the broker keeps in this host's own workspace.
/// A profile adopted from the desktop session refuses them: its file is
/// that host's to write.
impl LegacyWorkspaceProjection for ProfileWorkspace {
    fn record_effect(
        &self,
        room: &str,
        invocation: tilo_actions::InvocationId,
        actor: &str,
        context: tilo_actions::ActionContext,
        status: tilo_actions::EffectReceiptStatus,
    ) -> Result<(), String> {
        let completed_at = i64::try_from(unix_now()).unwrap_or(i64::MAX);
        self.record(room, |tree, workspace| {
            // A retried invocation keeps its first receipt.
            tree.append_workspace_effect_receipt_at(
                workspace,
                invocation,
                actor,
                context,
                status,
                completed_at,
            );
            Some(())
        })
    }

    fn record_task_update(
        &self,
        room: &str,
        task: &str,
        actor: &str,
        status: Option<WorkspaceTaskStatus>,
        summary: String,
    ) -> Result<(), String> {
        let now = i64::try_from(unix_now()).unwrap_or(i64::MAX);
        self.record(room, |tree, workspace| {
            let workspace = tree
                .workspaces
                .iter_mut()
                .find(|candidate| candidate.id == workspace)?;
            if let Some(status) = status {
                workspace.set_task_status_at(task, status, actor, now);
            }
            workspace.append_task_audit_at(task, actor, summary, now)?;
            Some(())
        })
    }
}

impl WorkspaceAuthority for ProfileWorkspace {
    fn snapshot(&self) -> Result<DesktopTree, String> {
        self.tree
            .read()
            .map(|tree| tree.clone())
            .map_err(|_| "runtime workspace lock poisoned".to_owned())
    }

    /// This host's own journal, also for an adopted profile: the desktop
    /// session's files stay untouched.
    fn legacy_journal_root(&self) -> Option<PathBuf> {
        Some(self.root.join(INVOCATION_JOURNAL))
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
