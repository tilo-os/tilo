#![allow(refining_impl_trait)]
//! Adapters from the shared broker to the native session's existing services.
use super::desktop_bridge::{native_room, native_task_status, native_workspace, portable_tree};
use super::state::WorkspaceAuthority as _;
use super::state::WorkspaceTreeHandle;
mod committed_document;
use qos_action_broker::services::{
    ContinuationServices, HostServices, LaunchContent, LaunchScope, LaunchableApplication,
    LauncherServices, LegacyWorkspaceProjection, NativeDocumentRead, NativeDocumentReadContext,
    NativeDocumentReads, WorkspaceAuthority, WorkspaceControlServices, WorkspaceGovernanceRequest,
    WorkspaceGovernanceServices,
};
use std::sync::Arc;

/// The same state.json owner used by canonical enrollment and ingress. Network
/// availability never supplies or removes this durable admission boundary.
pub(crate) struct NativeOwnerCommitService {
    tree: WorkspaceTreeHandle,
    owner: Option<super::native_workspace_owner::NativeWorkspaceOwner>,
    network: Option<Arc<super::network::ShellNetwork>>,
}
impl NativeOwnerCommitService {
    pub(crate) fn new(
        tree: WorkspaceTreeHandle,
        network: Option<Arc<super::network::ShellNetwork>>,
    ) -> Self {
        let opened = match network.as_ref() {
            Some(network) => network.native_owner(),
            None => super::native_workspace_owner::NativeWorkspaceOwner::open(tree.clone()),
        };
        let owner = match opened {
            Ok(owner) => Some(owner),
            Err(error) => {
                tracing::warn!(%error,"native durable owner unavailable");
                None
            }
        };
        Self {
            tree,
            owner,
            network,
        }
    }
}
impl qos_peer_state::owner_commit::OwnerCommitService for NativeOwnerCommitService {
    fn requires_room(&self, room: &str) -> bool {
        super::state::RoomId::parse(room)
            .is_some_and(|room| self.tree.read().requires_native_owner(room))
    }
    fn health(&self) -> Result<(), String> {
        self.owner
            .as_ref()
            .ok_or("native durable owner unavailable")?
            .health()
    }
    fn commit(
        &self,
        request: qos_peer_state::owner_commit::OwnerCommitRequest,
    ) -> qos_peer_state::owner_commit::CommitFuture {
        let owner = self.owner.clone();
        let network = self.network.clone();
        Box::pin(async move {
            let owner = owner.ok_or("native durable owner unavailable")?;
            let room = super::state::RoomId::parse(&request.room).ok_or("invalid native room")?;
            if let Some(network) = network {
                network.check_native_custody(&owner, room)?;
            }
            owner.commit_document(request, std::time::SystemTime::now())
        })
    }
}

pub struct NativeWorkspaceAuthority(pub WorkspaceTreeHandle);
impl WorkspaceAuthority for NativeWorkspaceAuthority {
    fn snapshot(&self) -> Result<qos_desktop::WorkspaceTree, String> {
        portable_tree(&self.0.read().desktop)
    }
    fn legacy_journal_root(&self) -> Option<std::path::PathBuf> {
        self.0.read().persist_path.as_ref().map(|path| {
            let mut name = path.clone().into_os_string();
            name.push(".invocations");
            name.into()
        })
    }
}
impl LegacyWorkspaceProjection for NativeWorkspaceAuthority {
    fn record_effect(
        &self,
        room: &str,
        invocation: tilo_actions::InvocationId,
        actor: &str,
        context: tilo_actions::ActionContext,
        status: tilo_actions::EffectReceiptStatus,
    ) -> Result<(), String> {
        let mut tree = self.0.write();
        if super::state::RoomId::parse(room).is_some_and(|room| tree.requires_native_owner(room)) {
            return Err("native enrolled mutation requires its typed durable owner".into());
        }
        let id = tree
            .desktop
            .workspaces
            .iter()
            .find(|w| w.room.to_string() == room)
            .map(|w| w.id)
            .ok_or("effect workspace no longer hosted")?;
        // Preserve native projection semantics; this void flush is not OwnerCommitService.
        let _ = tree.append_workspace_effect_receipt(id, invocation, actor, context, status);
        drop(tree);
        Ok(())
    }
    fn record_task_update(
        &self,
        room: &str,
        task: &str,
        actor: &str,
        status: Option<qos_desktop::WorkspaceTaskStatus>,
        summary: String,
    ) -> Result<(), String> {
        let mut tree = self.0.write();
        if super::state::RoomId::parse(room).is_some_and(|room| tree.requires_native_owner(room)) {
            return Err("native enrolled mutation requires its typed durable owner".into());
        }
        let id = tree
            .desktop
            .workspaces
            .iter()
            .find(|w| w.room.to_string() == room && w.tasks.iter().any(|t| t.id == task))
            .map(|w| w.id)
            .ok_or("task workspace or task no longer hosted")?;
        if let Some(status) = status {
            let _ = tree.set_workspace_task_status(id, task, native_task_status(status), actor);
        }
        let _ = tree.append_workspace_task_audit(id, task, actor, summary);
        drop(tree);
        Ok(())
    }
}

/// Upper bound on content a launch may open read-only.
const MAX_LAUNCH_CONTENT_BYTES: usize = 64 * 1024 * 1024;

/// Reads content an application asked the shell to open, through that
/// application's own document-media scope. Possessing a hash never grants
/// access, so the shell cannot be used to read around an app's capability.
trait LaunchContentReader {
    fn read(&self, room: &str, app: &str, hash: &str) -> Result<Vec<u8>, String>;
}

/// The filesystem daemon's content store, reached as the desktop session.
struct SessionContentStore;

static SESSION_BLOBS: std::sync::Mutex<Option<runtime_fs::blobs::BlobClient>> =
    std::sync::Mutex::new(None);

impl LaunchContentReader for SessionContentStore {
    fn read(&self, room: &str, app: &str, hash: &str) -> Result<Vec<u8>, String> {
        use runtime_fs::blobs::{BlobClient, BlobError, ReadOutcome};

        let mut session = SESSION_BLOBS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let client = if let Some(client) = session.as_ref() {
            client.clone()
        } else {
            let socket = ::launcher::services::blob_socket();
            let client = BlobClient::connect_session(&socket)
                .map_err(|error| format!("launch content store: {error}"))?;
            *session = Some(client.clone());
            client
        };
        let result = client
            .scope(room, app)
            .and_then(|scoped| scoped.read_all(hash));
        if matches!(result, Err(BlobError::Disconnected(_))) {
            // Reconnect on the next launch rather than keep a dead session.
            *session = None;
        }
        drop(session);
        match result {
            Ok(ReadOutcome::Ready { bytes, .. }) => Ok(bytes),
            Ok(ReadOutcome::Pending) => {
                Err("launch content is still arriving on this device".into())
            }
            Ok(ReadOutcome::Unavailable) => {
                Err("launch content is unavailable on this device".into())
            }
            Err(error) => Err(format!("launch content store: {error}")),
        }
    }
}

/// Stage the content `content` names as a read-only file granted to
/// `app_id`. `None` for a plain launch. The bytes are read through the scope
/// of the application that asked — the room partition its private directory
/// uses — and never with the shell's own authority: holding a hash is not
/// access.
fn stage_launch_content(
    app_id: &str,
    content: LaunchContent<'_>,
    workspace: &mut super::state::Workspace,
    reader: &dyn LaunchContentReader,
) -> Result<Option<std::path::PathBuf>, String> {
    let content_hash = content.hash;
    if content_hash.is_empty() {
        return Ok(None);
    }
    if content_hash.len() != 64
        || !content_hash
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err("launch content hash must be a lowercase 64-character blake3 value".into());
    }
    let caller: &LaunchScope = content.caller.ok_or_else(|| {
        "launch content requires an application the launcher authenticated".to_owned()
    })?;
    let bytes = reader.read(
        &::launcher::appdir::room_slug(Some(&caller.workspace_id)),
        &caller.app_id,
        content_hash,
    )?;
    if bytes.is_empty()
        || bytes.len() > MAX_LAUNCH_CONTENT_BYTES
        || !tilo_content::verify(content_hash, &bytes)
    {
        return Err("launch content failed integrity validation".into());
    }
    let dir = std::env::var_os("XDG_RUNTIME_DIR")
        .map_or_else(std::env::temp_dir, std::path::PathBuf::from)
        .join("tilo-opened");
    std::fs::create_dir_all(&dir).map_err(|error| format!("create launch staging: {error}"))?;
    #[cfg(unix)]
    std::fs::set_permissions(
        &dir,
        <std::fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o700),
    )
    .map_err(|error| format!("protect launch staging: {error}"))?;
    let extension = match tilo_content::sniff(&bytes).mime.as_str() {
        "image/png" => "png",
        "image/jpeg" => "jpg",
        "image/webp" => "webp",
        _ if bytes.get(4..8).is_some_and(|header| header == b"ftyp") => "mp4",
        _ if bytes.starts_with(&[0x1A, 0x45, 0xDF, 0xA3]) => "webm",
        _ if bytes.starts_with(b"RIFF")
            && bytes.get(8..12).is_some_and(|header| header == b"AVI ") =>
        {
            "avi"
        }
        _ => "bin",
    };
    let path = dir.join(format!("{content_hash}.{extension}"));
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
    {
        Ok(mut file) => {
            use std::io::Write;
            file.write_all(&bytes)
                .and_then(|()| file.sync_all())
                .map_err(|error| format!("stage launch content: {error}"))?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let metadata = std::fs::symlink_metadata(&path)
                .map_err(|error| format!("inspect existing launch content: {error}"))?;
            if !metadata.file_type().is_file() {
                return Err("existing launch content is not a regular file".into());
            }
            let existing = std::fs::read(&path)
                .map_err(|error| format!("read existing launch content: {error}"))?;
            if existing.len() > MAX_LAUNCH_CONTENT_BYTES
                || !tilo_content::verify(content_hash, &existing)
            {
                return Err("existing launch content failed integrity validation".into());
            }
        }
        Err(error) => return Err(format!("stage launch content: {error}")),
    }
    workspace.grant_app(
        app_id,
        &super::state::Capability::Filesystem {
            path: dir,
            mode: super::state::FsMode::Read,
            scope: super::state::GRANT_SESSION,
        },
    );
    Ok(Some(path))
}

pub struct NativeLauncher;
impl LauncherServices for NativeLauncher {
    fn applications(&self) -> Vec<LaunchableApplication> {
        super::launcher::gui_apps()
            .into_iter()
            .map(|app| LaunchableApplication {
                package_id: app.package_id,
                display_name: app.display_name,
            })
            .collect()
    }
    fn launch(&self, app: &str, workspace: &qos_desktop::Workspace) -> Result<(), String> {
        super::launcher::launch_in_workspace(app, &native_workspace(workspace)?)
            .map(|_| ())
            .map_err(|e| e.to_string())
    }
    fn launch_content(
        &self,
        app: &str,
        workspace: &qos_desktop::Workspace,
        content: LaunchContent<'_>,
    ) -> Result<(), String> {
        let mut workspace = native_workspace(workspace)?;
        let document = stage_launch_content(app, content, &mut workspace, &SessionContentStore)?;
        super::launcher::launch_document_in_workspace(app, &workspace, document.as_deref())
            .map(|_| ())
            .map_err(|e| e.to_string())
    }
    fn attest_process(&self, pid: u32, app: &str, room: &str) -> bool {
        super::launcher::attest_trusted_service_launch_for_pid(pid, app, room).is_some()
    }
}

/// Workspace governance (permission profiles) applied under the shell's
/// canonical capability authority. There is no application behind these
/// actions: the shell validates, mutates, records the effect and persists.
pub struct NativeWorkspaceGovernance(pub WorkspaceTreeHandle);
impl WorkspaceGovernanceServices for NativeWorkspaceGovernance {
    fn app_id(&self) -> &str {
        super::state::WORKSPACE_GOVERNANCE_APP_ID
    }
    fn handles(&self, action: &str) -> bool {
        matches!(
            action,
            super::state::PROFILE_SAVE_ACTION_ID | super::state::PROFILE_ASSIGN_ACTION_ID
        )
    }
    fn descriptors(
        &self,
        workspace: &qos_desktop::Workspace,
    ) -> Vec<tilo_actions::ActionDescriptor> {
        // Describe from the shell's own authority, not the broker's snapshot.
        let tree = self.0.read();
        let room = native_room(workspace.room);
        tree.desktop
            .workspaces
            .iter()
            .find(|workspace| workspace.room == room)
            .map(|workspace| {
                tilo_actions::door::survey(&[
                    super::workspace_settings::permission_profile_resource_with_access(
                        workspace, true,
                    ),
                ])
            })
            .unwrap_or_default()
    }
    fn invoke(&self, request: &WorkspaceGovernanceRequest<'_>) -> Result<(), String> {
        let actor = request.actor;
        let mut tree = self.0.write();
        let workspace_index = tree
            .desktop
            .workspaces
            .iter()
            .position(|workspace| workspace.room.to_string() == request.room)
            .ok_or_else(|| "the application workspace is no longer hosted".to_string())?;
        let workspace = tree
            .desktop
            .workspaces
            .get(workspace_index)
            .ok_or_else(|| "the application workspace is no longer hosted".to_string())?;
        let caller = if actor.is_agent() {
            workspace.actor_for_agent(
                actor
                    .id()
                    .strip_prefix("agent:")
                    .unwrap_or_else(|| actor.id()),
            )
        } else {
            workspace.actor_for_person(&actor.id().to_string())
        };
        let surface =
            super::workspace_settings::permission_profile_resource_with_access(workspace, true);
        let resource = find_resource(&surface, request.resource)
            .ok_or_else(|| "unknown workspace governance resource".to_string())?;
        let spec = resource
            .actions
            .iter()
            .find(|candidate| candidate.id == request.action)
            .ok_or_else(|| "unknown workspace governance action".to_string())?;
        if request
            .confirmation
            .get("confirm")
            .and_then(serde_json::Value::as_bool)
            != Some(true)
        {
            return Err("explicit confirmation is required".into());
        }
        if workspace.decide(
            &caller,
            &super::state::ResourceRef::Action {
                app: super::state::WORKSPACE_GOVERNANCE_APP_ID.into(),
                action: request.action.into(),
            },
            super::state::Verb::Admin,
        ) != super::state::CapDecision::Allow
        {
            return Err("the caller is not allowed to manage permission profiles".into());
        }
        let operation = decode_permission_profile_operation(spec, request.argument)?;
        let workspace_id = workspace.id;
        apply_permission_profile_operation(
            &mut tree,
            workspace_index,
            workspace_id,
            &caller,
            operation,
        )?;
        let context = tilo_actions::ActionContext::for_resource(
            &spec.label,
            resource.id.as_str(),
            &resource.kind,
            &resource.label,
        )
        .with_app(
            super::state::WORKSPACE_GOVERNANCE_APP_ID,
            "Workspace permissions",
        );
        let _ = tree.append_workspace_effect_receipt(
            workspace_id,
            request.invocation,
            actor.id(),
            context,
            tilo_actions::EffectReceiptStatus::Succeeded,
        );
        tree.desktop.external_revision = tree.desktop.external_revision.wrapping_add(1);
        tree.flush_to_disk();
        drop(tree);
        Ok(())
    }
}

/// Find `id` in `resource`'s subtree.
fn find_resource<'a>(
    resource: &'a tilo_actions::Resource,
    id: &str,
) -> Option<&'a tilo_actions::Resource> {
    if resource.id.as_str() == id {
        return Some(resource);
    }
    resource
        .children
        .iter()
        .find_map(|child| find_resource(child, id))
}

/// Decode a workspace governance invocation's typed `argument` into the
/// permission-profile operation `spec` encodes.
fn decode_permission_profile_operation(
    spec: &tilo_actions::ActionSpec,
    argument: Option<&str>,
) -> Result<super::workspace_settings::PermissionProfileOperation, String> {
    let json = argument
        .and_then(|value| value.strip_prefix(tilo_actions::ARGUMENTS_JSON_PREFIX))
        .ok_or_else(|| "workspace governance action requires typed arguments".to_string())?;
    let arguments = serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(json)
        .map_err(|error| format!("invalid workspace governance arguments: {error}"))?;
    let encoded = spec
        .build_json(&arguments)
        .map_err(|error| format!("invalid workspace governance arguments: {error}"))?;
    if encoded.type_id == tilo_actions::resource::INVALID_ARGUMENTS_TYPE {
        return Err(format!(
            "invalid workspace governance arguments: {}",
            String::from_utf8_lossy(&encoded.payload)
        ));
    }
    if encoded.type_id != super::workspace_settings::PERMISSION_PROFILE_ACTION_TYPE {
        return Err("workspace governance action encoded an unexpected operation".into());
    }
    serde_json::from_slice(&encoded.payload)
        .map_err(|error| format!("invalid workspace governance operation: {error}"))
}

/// Apply a permission-profile operation as `caller` to one workspace and
/// mirror it into the room document.
fn apply_permission_profile_operation(
    tree: &mut super::state::WorkspaceTree,
    workspace_index: usize,
    workspace_id: super::state::WorkspaceId,
    caller: &super::state::Actor,
    operation: super::workspace_settings::PermissionProfileOperation,
) -> Result<(), String> {
    match operation {
        super::workspace_settings::PermissionProfileOperation::Save {
            name,
            description,
            rules,
        } => {
            let profile = collab_types::PermissionProfile {
                name,
                description,
                rules,
            };
            tree.desktop
                .workspaces
                .get_mut(workspace_index)
                .ok_or("the application workspace is no longer hosted")?
                .save_permission_profile_as_actor(caller, profile.clone())
                .map_err(|error| error.to_string())?;
            tree.doc_upsert_permission_profile(workspace_id, &profile);
        }
        super::workspace_settings::PermissionProfileOperation::Assign {
            profile,
            subject_kind,
            subject,
            expires_unix,
        } => {
            let subject = match subject_kind.trim().to_ascii_lowercase().as_str() {
                "person" => collab_types::Subject::person(subject.trim()),
                "app" => collab_types::Subject::app(subject.trim()),
                "agent" => collab_types::Subject::agent(subject.trim()),
                _ => return Err("subject_kind must be person, app, or agent".into()),
            };
            let expiry = (expires_unix > 0)
                .then_some(expires_unix)
                .map_or(collab_types::Expiry::Never, collab_types::Expiry::AtUnix);
            let workspace = tree
                .desktop
                .workspaces
                .get_mut(workspace_index)
                .ok_or("the application workspace is no longer hosted")?;
            let ids = workspace
                .assign_permission_profile_as_actor(caller, &profile, &subject, expiry)
                .map_err(|error| error.to_string())?;
            let capabilities = workspace
                .capabilities
                .iter()
                .filter(|capability| ids.contains(&capability.id))
                .cloned()
                .collect::<Vec<_>>();
            for capability in capabilities {
                tree.doc_insert_capability(workspace_id, &capability);
            }
        }
    }
    Ok(())
}

pub struct NativeContinuations;
impl ContinuationServices for NativeContinuations {
    fn report(
        &self,
        room: qos_desktop::RoomId,
        app: String,
        instance: u64,
        schema: u16,
        metadata: Vec<u8>,
    ) -> Result<(), String> {
        super::personal_continuity_registry::global_registry()
            .lock()
            .map_err(|_| "continuation registry unavailable".to_string())?
            .report(
                native_room(room),
                app,
                super::personal_continuity::AppInstanceId(instance),
                schema,
                metadata,
            )
            .map(|_| ())
            .map_err(|e| e.to_string())
    }
    fn clear(&self, room: qos_desktop::RoomId, app: &str, instance: u64) -> Result<(), String> {
        super::personal_continuity_registry::global_registry()
            .lock()
            .map_err(|_| "continuation registry unavailable".to_string())?
            .clear(
                native_room(room),
                app,
                super::personal_continuity::AppInstanceId(instance),
            );
        Ok(())
    }
}

/// The native identity selector and fleet remain native service dependencies.
pub struct NativeCertificates;
impl qos_action_broker::peer_actions::CertificateAuthority for NativeCertificates {
    fn authorize(
        &self,
        room: uuid::Uuid,
        cert: &tilo_user_identity::DeviceCert,
    ) -> Result<(), String> {
        let profile = crate::identity_registry::selected_identity_profile(&room.to_string())
            .map_err(|e| e.to_string())?
            .ok_or("workspace identity unavailable")?;
        let fleet = crate::device_fleet::DeviceFleetStore::from_env()
            .and_then(|store| store.load(&profile.id))
            .map_err(|e| e.to_string())?;
        if cert.user_public_key != profile.user_public_key
            || fleet.is_revoked(&cert.device_public_key)
        {
            return Err("device revoked or identity mismatched".into());
        }
        Ok(())
    }
    fn authorize_join(
        &self,
        room: uuid::Uuid,
        cert: &tilo_user_identity::DeviceCert,
    ) -> Result<(), String> {
        self.authorize(room, cert)?;
        let profile = crate::identity_registry::selected_identity_profile(&room.to_string())
            .map_err(|e| e.to_string())?
            .ok_or("workspace identity unavailable")?;
        let fleet = crate::device_fleet::DeviceFleetStore::from_env()
            .and_then(|store| store.load(&profile.id))
            .map_err(|e| e.to_string())?;
        if !fleet
            .certificates
            .iter()
            .any(|known| known.device_public_key == cert.device_public_key)
        {
            return Err("certified collaboration device is not enrolled".into());
        }
        Ok(())
    }
}
pub fn native_services(
    membership: Option<WorkspaceTreeHandle>,
    network: Option<Arc<super::network::ShellNetwork>>,
) -> HostServices {
    let committed_documents = membership.as_ref().map(|tree| {
        Arc::new(NativeOwnerCommitService::new(tree.clone(), network.clone()))
            as Arc<dyn NativeDocumentReads>
    });
    let governance = membership.as_ref().map(|tree| {
        Arc::new(NativeWorkspaceGovernance(tree.clone())) as Arc<dyn WorkspaceGovernanceServices>
    });
    HostServices {
        committed_documents,
        desktop_commands: None,
        native_certificates: Some(Arc::new(NativeCertificates)),
        legacy_workspace: membership.map(|tree| {
            Arc::new(NativeWorkspaceAuthority(tree)) as Arc<dyn LegacyWorkspaceProjection>
        }),
        launcher: Some(Arc::new(NativeLauncher)),
        workspace_governance: governance,
        continuations: Some(Arc::new(NativeContinuations)),
        workspace_control: network.map(|network| {
            Arc::new(NativeWorkspaceControl(network)) as Arc<dyn WorkspaceControlServices>
        }),
        ..Default::default()
    }
}

/// A broker over `membership` with the native services an action host
/// reaches (launcher, governance, legacy projection), for tests.
#[cfg(test)]
pub(crate) fn test_broker_with_membership(
    membership: WorkspaceTreeHandle,
) -> qos_action_broker::collab_broker::AppActionBroker {
    let authority = Arc::new(NativeWorkspaceAuthority(membership.clone()));
    let services = HostServices {
        legacy_workspace: Some(authority.clone()),
        launcher: Some(Arc::new(NativeLauncher)),
        workspace_governance: Some(Arc::new(NativeWorkspaceGovernance(membership))),
        ..Default::default()
    };
    qos_action_broker::collab_broker_capnp::CapnpBrokerState::test_broker_with_authority(
        authority, services,
    )
}

use capnp::capability::Promise;
use runtime_collab::raw::workspace_control_handle;
use std::rc::Rc;
use tokio::sync::Mutex as AsyncMutex;
pub struct NativeWorkspaceControl(pub Arc<super::network::ShellNetwork>);
impl WorkspaceControlServices for NativeWorkspaceControl {
    fn open(
        &self,
        room: qos_desktop::RoomId,
        target: [u8; 32],
        revalidate: Arc<dyn Fn() -> Result<(), capnp::Error> + Send + Sync>,
    ) -> Result<workspace_control_handle::Client, capnp::Error> {
        revalidate()?;
        let target = iroh::EndpointId::from_bytes(&target)
            .map_err(|_| capnp::Error::failed("invalid target device identity".into()))?;
        let session = self
            .0
            .open_workspace_control(super::network::room_topic_id(native_room(room)), target)
            .map_err(capnp::Error::failed)?;
        let initial_route = match session.route() {
            crate::network::WorkspaceControlRoute::Direct { .. } => {
                runtime_collab::WorkspaceControlRoute::Direct
            }
            crate::network::WorkspaceControlRoute::Relayed { via } => {
                runtime_collab::WorkspaceControlRoute::Relayed {
                    via: *via.as_bytes(),
                }
            }
        };
        Ok(capnp_rpc::new_client(CapnpWorkspaceControlHandle {
            revalidate,
            network: self.0.clone(),
            session: Rc::new(AsyncMutex::new(session)),
            inbound: Rc::new(AsyncMutex::new(self.0.subscribe_workspace_control())),
            initial: Rc::new(AsyncMutex::new(Some(
                runtime_collab::WorkspaceControlEvent::WaitingForConsent {
                    route: initial_route,
                },
            ))),
        }))
    }
}
struct CapnpWorkspaceControlHandle {
    revalidate: Arc<dyn Fn() -> Result<(), capnp::Error> + Send + Sync>,
    network: Arc<crate::shell::network::ShellNetwork>,
    session: Rc<AsyncMutex<crate::network::WorkspaceControlSession>>,
    inbound: Rc<
        AsyncMutex<
            tokio::sync::mpsc::UnboundedReceiver<
                crate::shell::network::WorkspaceControlNetworkEvent,
            >,
        >,
    >,
    initial: Rc<AsyncMutex<Option<runtime_collab::WorkspaceControlEvent>>>,
}

fn workspace_control_event(
    update: crate::network::WorkspaceControlUpdate,
) -> runtime_collab::WorkspaceControlEvent {
    match update {
        crate::network::WorkspaceControlUpdate::ConsentGranted {
            surface,
            width,
            height,
        } => runtime_collab::WorkspaceControlEvent::StreamStarted {
            surface,
            width,
            height,
        },
        crate::network::WorkspaceControlUpdate::Frame {
            surface,
            encoded_h264,
            timestamp,
        } => runtime_collab::WorkspaceControlEvent::Frame {
            surface,
            encoded_h264,
            timestamp,
        },
        crate::network::WorkspaceControlUpdate::Closed(reason) => {
            let reason = match reason {
                crate::network::WorkspaceControlCloseReason::Denied(message) => {
                    runtime_collab::WorkspaceControlCloseReason::Denied(message)
                }
                crate::network::WorkspaceControlCloseReason::Revoked => {
                    runtime_collab::WorkspaceControlCloseReason::Revoked
                }
                crate::network::WorkspaceControlCloseReason::TargetDisconnected => {
                    runtime_collab::WorkspaceControlCloseReason::TargetDisconnected
                }
                crate::network::WorkspaceControlCloseReason::RelayDisconnected => {
                    runtime_collab::WorkspaceControlCloseReason::RelayDisconnected
                }
                crate::network::WorkspaceControlCloseReason::LocalDisconnect => {
                    runtime_collab::WorkspaceControlCloseReason::LocalDisconnect
                }
            };
            runtime_collab::WorkspaceControlEvent::Closed(reason)
        }
    }
}

fn closed_workspace_control_event(
    session: &crate::network::WorkspaceControlSession,
) -> Option<runtime_collab::WorkspaceControlEvent> {
    let crate::network::WorkspaceControlStatus::Closed(reason) = session.status() else {
        return None;
    };
    Some(workspace_control_event(
        crate::network::WorkspaceControlUpdate::Closed(reason.clone()),
    ))
}

impl workspace_control_handle::Server for CapnpWorkspaceControlHandle {
    fn read(
        self: Rc<Self>,
        _params: workspace_control_handle::ReadParams,
        mut results: workspace_control_handle::ReadResults,
    ) -> Promise<(), capnp::Error> {
        let this = self;
        Promise::from_future(async move {
            let initial_event = {
                let mut initial = this.initial.lock().await;
                initial.take()
            };
            if let Some(event) = initial_event {
                (this.revalidate)()?;
                let bytes = postcard::to_allocvec(&event)
                    .map_err(|error| capnp::Error::failed(error.to_string()))?;
                results.get().set_event(&bytes);
                return Ok(());
            }
            loop {
                let closed = {
                    let session = this.session.lock().await;
                    closed_workspace_control_event(&session)
                };
                if let Some(event) = closed {
                    let bytes = postcard::to_allocvec(&event)
                        .map_err(|error| capnp::Error::failed(error.to_string()))?;
                    results.get().set_event(&bytes);
                    return Ok(());
                }
                if let Err(error) = (this.revalidate)() {
                    let event = {
                        let mut session = this.session.lock().await;
                        workspace_control_event(session.deny(error.to_string()))
                    };
                    let bytes = postcard::to_allocvec(&event)
                        .map_err(|error| capnp::Error::failed(error.to_string()))?;
                    results.get().set_event(&bytes);
                    return Ok(());
                }
                let network_event =
                    match tokio::time::timeout(std::time::Duration::from_millis(250), async {
                        this.inbound.lock().await.recv().await
                    })
                    .await
                    {
                        Ok(Some(event)) => event,
                        Ok(None) => {
                            return Err(capnp::Error::disconnected(
                                "workspace-control observer closed".into(),
                            ));
                        }
                        Err(_) => continue,
                    };
                let mut session = this.session.lock().await;
                (this.revalidate)()?;
                let update = match network_event {
                    crate::shell::network::WorkspaceControlNetworkEvent::Message(event)
                        if event.workspace_topic == session.workspace() =>
                    {
                        session.observe_from(event.transport_peer, &event.message)
                    }
                    crate::shell::network::WorkspaceControlNetworkEvent::PeerLeft {
                        workspace_topic,
                        peer,
                    } if workspace_topic == session.workspace() => session.peer_departed(peer),
                    crate::shell::network::WorkspaceControlNetworkEvent::PeerRevoked {
                        workspace_topic,
                        peer,
                    } if workspace_topic == session.workspace() && peer == session.target() => {
                        Some(session.deny("target device certificate was revoked"))
                    }
                    _ => None,
                };
                if let Some(update) = update {
                    let bytes = postcard::to_allocvec(&workspace_control_event(update))
                        .map_err(|error| capnp::Error::failed(error.to_string()))?;
                    results.get().set_event(&bytes);
                    return Ok(());
                }
            }
        })
    }

    fn send_input(
        self: Rc<Self>,
        params: workspace_control_handle::SendInputParams,
        _results: workspace_control_handle::SendInputResults,
    ) -> Promise<(), capnp::Error> {
        if let Err(error) = (self.revalidate)() {
            return Promise::err(error);
        }
        let bytes = match params.get().and_then(
            runtime_collab::collab_capnp::workspace_control_handle::send_input_params::Reader::get_event,
        ) {
            Ok(bytes) => bytes,
            Err(error) => return Promise::err(error),
        };
        let input: runtime_collab::WorkspaceControlInput = match postcard::from_bytes(bytes) {
            Ok(input) => input,
            Err(error) => return Promise::err(capnp::Error::failed(error.to_string())),
        };
        let this = self;
        Promise::from_future(async move {
            {
                let session = this.session.lock().await;
                (this.revalidate)()?;
                let input = match input {
                    runtime_collab::WorkspaceControlInput::PointerMotion { x, y } => {
                        crate::network::InputEvent::PointerMotion { x, y }
                    }
                    runtime_collab::WorkspaceControlInput::PointerButton { button, pressed } => {
                        crate::network::InputEvent::PointerButton { button, pressed }
                    }
                    runtime_collab::WorkspaceControlInput::Key {
                        keycode,
                        pressed,
                        modifiers,
                    } => crate::network::InputEvent::Key {
                        keysym: keycode,
                        pressed,
                        modifiers,
                    },
                    runtime_collab::WorkspaceControlInput::KeyboardFocus { focused } => {
                        let surface = match session.status() {
                            crate::network::WorkspaceControlStatus::Active { surface, .. } => {
                                *surface
                            }
                            _ => {
                                return Err(capnp::Error::failed(
                                    "target has not granted control".into(),
                                ));
                            }
                        };
                        crate::network::InputEvent::KeyboardFocus { surface, focused }
                    }
                };
                let published = this
                    .network
                    .publish_workspace_control_input(&session, &input)
                    .map_err(capnp::Error::failed);
                drop(session);
                published
            }
        })
    }

    fn close(
        self: Rc<Self>,
        _params: workspace_control_handle::CloseParams,
        _results: workspace_control_handle::CloseResults,
    ) -> Promise<(), capnp::Error> {
        let session = self.session.clone();
        Promise::from_future(async move {
            session.lock().await.disconnect();
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Content keyed by `(room, hash)`, standing in for the content store's
    /// room-scoped reads.
    struct ScopedContent {
        room: String,
        hash: String,
        bytes: Vec<u8>,
    }

    impl LaunchContentReader for ScopedContent {
        fn read(&self, room: &str, _app: &str, hash: &str) -> Result<Vec<u8>, String> {
            if room == self.room && hash == self.hash {
                Ok(self.bytes.clone())
            } else {
                Err("launch content is unavailable on this device".into())
            }
        }
    }

    /// The shell stages content through the scope of the application that
    /// asked, never its own authority: a caller in another room holding the
    /// hash, or a launch no application asked for, stages nothing.
    #[cfg(unix)]
    #[test]
    #[serial_test::serial(process_env)]
    fn launch_content_is_read_through_the_calling_applications_scope() {
        let runtime = tempfile::tempdir().expect("runtime directory");
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("XDG_RUNTIME_DIR", runtime.path());
        let bytes = b"a photograph".to_vec();
        let hash = tilo_content::hash_bytes(&bytes);
        let content = ScopedContent {
            room: ::launcher::appdir::room_slug(Some("room-1")),
            hash: hash.clone(),
            bytes,
        };
        let scope = |workspace_id: &str| LaunchScope {
            app_id: "so.tilo.photos".into(),
            workspace_id: workspace_id.into(),
        };
        let mut workspace = crate::shell::state::Workspace::new(
            crate::shell::state::WorkspaceId(1),
            "launch content test",
        );
        let owner = scope("room-1");
        let staged = stage_launch_content(
            "so.tilo.photos",
            LaunchContent {
                hash: &hash,
                caller: Some(&owner),
            },
            &mut workspace,
            &content,
        )
        .expect("the owning app's content stages")
        .expect("a staged path");
        assert_eq!(
            std::fs::read(&staged).expect("staged bytes"),
            b"a photograph"
        );
        assert!(!workspace.app_capabilities("so.tilo.photos").is_empty());

        let elsewhere = scope("room-2");
        for caller in [Some(&elsewhere), None] {
            assert!(
                stage_launch_content(
                    "so.tilo.photos",
                    LaunchContent {
                        hash: &hash,
                        caller,
                    },
                    &mut workspace,
                    &content,
                )
                .is_err(),
                "{caller:?} holds the hash but not the content"
            );
        }
    }

    #[test]
    fn closed_workspace_control_session_is_terminal_for_rpc_readers() {
        let target = iroh::SecretKey::generate().public();
        let mut session = crate::network::WorkspaceControlSession::new(
            7,
            11,
            target,
            crate::network::WorkspaceControlRoute::Direct { target },
        );
        assert!(closed_workspace_control_event(&session).is_none());
        session.disconnect();
        assert_eq!(
            closed_workspace_control_event(&session),
            Some(runtime_collab::WorkspaceControlEvent::Closed(
                runtime_collab::WorkspaceControlCloseReason::LocalDisconnect,
            ))
        );
    }
}

#[cfg(test)]
#[path = "broker_services/native_owner_tests.rs"]
mod native_owner_tests;
