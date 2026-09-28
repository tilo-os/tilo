#![allow(refining_impl_trait)]
//! Adapters from the shared broker to the native session's existing services.
use super::state::WorkspaceAuthority as _;
use super::state::WorkspaceTreeHandle;
mod committed_document;
mod nearby_transfer;
pub use nearby_transfer::NativeNearbyTransfer;
use std::sync::Arc;
use tilo_action_broker::services::{
    ContinuationServices, HostServices, LaunchContent, LaunchScope, LaunchableApplication,
    LauncherServices, LegacyWorkspaceProjection, NativeDocumentRead, NativeDocumentReadContext,
    NativeDocumentReads, ShortcutLibraryRequest, ShortcutLibraryServices, WorkspaceAuthority,
    WorkspaceControlServices, WorkspaceGovernanceRequest, WorkspaceGovernanceServices,
};

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
            None => super::native_workspace_owner::open(tree.clone()),
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
impl tilo_peer_state::owner_commit::OwnerCommitService for NativeOwnerCommitService {
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
        request: tilo_peer_state::owner_commit::OwnerCommitRequest,
    ) -> tilo_peer_state::owner_commit::CommitFuture {
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
    fn snapshot(&self) -> Result<tilo_desktop_model::DesktopTree, String> {
        Ok(self.0.read().desktop.clone())
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
        status: Option<tilo_desktop_model::WorkspaceTaskStatus>,
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
            let _ = tree.set_workspace_task_status(id, task, status, actor);
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
    let dir = xdg_paths::xdg::runtime_dir()
        .unwrap_or_else(std::env::temp_dir)
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

/// Launches installed apps. With the session's workspace tree it can record
/// a workspace author granting an app authorship before launching it.
pub struct NativeLauncher(pub Option<WorkspaceTreeHandle>);

impl NativeLauncher {
    /// Record `person` granting `app` authorship of `workspace`'s space and
    /// add the grant to the launch's copy, unless the app already holds it.
    /// The tree checks `person`'s own authority; a refusal launches a viewer.
    fn grant_authorship(
        &self,
        person: &str,
        app: &str,
        workspace: &mut tilo_desktop_model::Workspace,
    ) {
        use super::state::WorkspaceExt as _;
        use collab_types::caps::{CapOverride, ResourceRef, Scope, Subject, Verb};
        let Some(tree) = &self.0 else { return };
        let actor = workspace.actor_for_app(app);
        if workspace.allows(&actor, &ResourceRef::Workspace, Verb::Write) {
            return;
        }
        let grant = CapOverride::grant(Subject::app(app), Scope::Workspace, Verb::Write);
        let mut tree = tree.write();
        let Some(id) = tree
            .desktop
            .workspaces
            .iter()
            .find(|candidate| candidate.room == workspace.room)
            .map(|candidate| candidate.id)
        else {
            return;
        };
        let Some(recorded) = tree.grant_in_workspace(id, &person.to_string(), grant) else {
            return;
        };
        let stored = tree
            .desktop
            .workspaces
            .iter()
            .find(|candidate| candidate.id == id)
            .and_then(|candidate| candidate.capabilities.iter().find(|cap| cap.id == recorded))
            .cloned();
        tree.desktop.external_revision = tree.desktop.external_revision.wrapping_add(1);
        tree.flush_to_disk();
        drop(tree);
        if let Some(stored) = stored {
            workspace.capabilities.push(stored);
        }
    }
}

impl LauncherServices for NativeLauncher {
    fn launch_instance_for_descendant(
        &self,
        pid: u32,
    ) -> Option<tilo_action_broker::services::LaunchInstance> {
        super::launcher::launch_instance_for_descendant(pid)
    }
    fn launch_instance_for_surface(
        &self,
        surface_id: u64,
    ) -> Option<tilo_action_broker::services::LaunchInstance> {
        super::launcher::launch_instance_for_surface(surface_id)
    }
    fn applications(&self) -> Vec<LaunchableApplication> {
        super::launcher::gui_apps()
            .into_iter()
            .map(|app| LaunchableApplication {
                package_id: app.package_id,
                display_name: app.display_name,
            })
            .collect()
    }
    fn launch(&self, app: &str, workspace: &tilo_desktop_model::Workspace) -> Result<(), String> {
        super::launcher::launch_in_workspace(app, workspace)
            .map(|_| ())
            .map_err(|e| e.to_string())
    }
    fn launch_content(
        &self,
        app: &str,
        workspace: &tilo_desktop_model::Workspace,
        content: LaunchContent<'_>,
    ) -> Result<(), String> {
        let mut workspace = workspace.clone();
        if let Some(person) = content.author {
            self.grant_authorship(person, app, &mut workspace);
        }
        let document = stage_launch_content(app, content, &mut workspace, &SessionContentStore)?;
        super::launcher::launch_document_in_workspace(app, &workspace, document.as_deref())
            .map(|_| ())
            .map_err(|e| e.to_string())
    }
    fn attest_process(&self, pid: u32, app: &str, room: &str) -> bool {
        super::launcher::attest_trusted_service_launch_for_pid(pid, app, room).is_some()
    }
}

/// The workspace's shortcuts, stored in the shell's canonical workspace
/// state and run on the session runtime.
pub struct NativeShortcutLibrary(pub WorkspaceTreeHandle);
impl ShortcutLibraryServices for NativeShortcutLibrary {
    fn keymap(&self) -> settings_store::hardware::ShortcutConfig {
        settings_store::hardware::ShortcutConfig::load()
    }
    fn store(&self, request: &ShortcutLibraryRequest<'_>) -> Result<(), String> {
        let mut tree = self.0.write();
        let workspace_id = tree
            .desktop
            .workspaces
            .iter()
            .find(|workspace| workspace.room.to_string() == request.room)
            .map(|workspace| workspace.id)
            .ok_or_else(|| "the workspace is no longer hosted here".to_string())?;
        let stored = match request.change {
            tilo_action_broker::automation::ShortcutChange::Save(shortcut) => {
                tree.save_workspace_shortcut(workspace_id, shortcut)
            }
            tilo_action_broker::automation::ShortcutChange::Delete(id) => {
                tree.remove_workspace_shortcut(workspace_id, id)
            }
        };
        if !stored {
            return Err("the workspace no longer holds that shortcut".into());
        }
        let _ = tree.append_workspace_effect_receipt(
            workspace_id,
            request.invocation,
            request.actor.id(),
            request.context.clone(),
            tilo_actions::EffectReceiptStatus::Succeeded,
        );
        drop(tree);
        Ok(())
    }
    fn runtime(&self) -> Result<tokio::runtime::Handle, String> {
        crate::session_runtime::runtime().map(|runtime| runtime.handle().clone())
    }
    fn fixed_shortcut_label(
        &self,
        fixed: &settings_store::keyboard_shortcuts::FixedShortcut,
    ) -> String {
        super::i18n::t(fixed.label_key)
    }
}

/// Room record sharing and person requests, queued on the session's tree
/// for the frame that folds them in.
pub struct NativeDesktopRequests(pub WorkspaceTreeHandle);
impl tilo_action_broker::services::DesktopRequestServices for NativeDesktopRequests {
    fn may_withdraw_room_record(&self, room: tilo_desktop_model::RoomId, record: &str) -> bool {
        self.0.read().may_withdraw_room_record(room, record)
    }
    fn request_room_reference(&self, room: tilo_desktop_model::RoomId, id: String, kind: String) {
        self.0.write().request_room_reference(room, id, kind);
    }
    fn request_room_unreference(&self, room: tilo_desktop_model::RoomId, id: String) {
        self.0.write().request_room_unreference(room, id);
    }
    fn request_person(&self, request: tilo_desktop_model::tree::PersonRequest) {
        self.0.write().request_person(request);
    }
}

/// The session's replay, once the overlay host has installed it. Until
/// then a replay request is answered as unavailable.
#[derive(Default)]
pub struct NativeSessionReplay(std::sync::OnceLock<Arc<super::replay::ReplayController>>);
impl NativeSessionReplay {
    /// Install the session's replay. Later calls are ignored.
    pub fn install(&self, replay: Arc<super::replay::ReplayController>) {
        let _ = self.0.set(replay);
    }
}
impl tilo_action_broker::services::SessionReplayServices for NativeSessionReplay {
    fn open_record(
        &self,
        record: &ui::record::RecordId,
        room: Option<tilo_desktop_model::RoomId>,
        viewer: &str,
    ) -> Result<Option<tilo_desktop_model::RoomId>, String> {
        let replay = self
            .0
            .get()
            .ok_or_else(|| "this session cannot replay recordings".to_string())?;
        replay
            .open_record_in(
                record,
                room,
                &viewer.to_string(),
                super::replay::Presentation::Shown,
            )
            .map_err(|error| error.to_string())
    }
}

/// The device's keyboard shortcuts on the action door, described in the
/// desktop's language.
pub struct NativeKeyboardShortcuts;
impl tilo_action_broker::services::KeyboardShortcutServices for NativeKeyboardShortcuts {
    fn app_id(&self) -> &str {
        super::keyboard_shortcuts::APP_ID
    }
    fn is_resource(&self, resource: &str) -> bool {
        super::keyboard_shortcuts::is_resource(resource)
    }
    fn catalogue_rows(
        &self,
        provider: &str,
        keymap: &settings_store::hardware::ShortcutConfig,
    ) -> Vec<tilo_actions::remote::CatalogueRow> {
        super::keyboard_shortcuts::catalogue_rows(provider, keymap)
    }
    fn descriptor(
        &self,
        keymap: &settings_store::hardware::ShortcutConfig,
        address: &tilo_actions::ActionAddress,
    ) -> Option<tilo_actions::ActionDescriptor> {
        super::keyboard_shortcuts::descriptor_at(keymap, address)
    }
    fn apply(
        &self,
        keymap: &mut settings_store::hardware::ShortcutConfig,
        address: &tilo_actions::ActionAddress,
        argument: Option<&str>,
    ) -> Result<(tilo_actions::ActionOutput, bool), Box<tilo_actions::ActionFailure>> {
        let operation = super::keyboard_shortcuts::decode(keymap, address, argument)?;
        let output = super::keyboard_shortcuts::apply(keymap, &operation)?;
        Ok((output, operation.changes()))
    }
}

/// Workspace governance (permission profiles and public-room governance)
/// applied under the shell's canonical capability authority. There is no
/// application behind these actions: the shell validates, mutates, records
/// the effect and persists. Public-room governance is performed by the
/// shell's network, exactly as the palette's is.
pub struct NativeWorkspaceGovernance {
    pub tree: WorkspaceTreeHandle,
    pub network: Option<Arc<super::network::ShellNetwork>>,
}

impl NativeWorkspaceGovernance {
    #[must_use]
    pub fn new(
        tree: WorkspaceTreeHandle,
        network: Option<Arc<super::network::ShellNetwork>>,
    ) -> Self {
        Self { tree, network }
    }

    /// Run one public-room governance action (`shell::public_room_governance`)
    /// for `request.actor`: the action must be on the room's surface,
    /// confirmed, and the caller must hold Admin on it; the shell's network
    /// then performs it exactly as the palette's does. The answer (a new
    /// room's id, a minted invitation) is the invocation's output.
    fn invoke_public_room(
        &self,
        request: &WorkspaceGovernanceRequest<'_>,
    ) -> Result<tilo_actions::ActionOutput, String> {
        let (workspace_id, room, spec, resource) = {
            let workspace = self
                .tree
                .read()
                .desktop
                .workspaces
                .iter()
                .find(|workspace| workspace.room.to_string() == request.room)
                .cloned()
                .ok_or_else(|| "the application workspace is no longer hosted".to_string())?;
            let workspace = &workspace;
            let surface = super::public_room_governance::resource(workspace);
            let spec = surface
                .actions
                .iter()
                .find(|candidate| candidate.id == request.action)
                .cloned()
                .ok_or_else(|| "this room offers no such public-room action".to_string())?;
            if request
                .confirmation
                .get("confirm")
                .and_then(serde_json::Value::as_bool)
                != Some(true)
            {
                return Err("explicit confirmation is required".into());
            }
            let caller = governance_caller(workspace, request.actor);
            if workspace.decide(
                &caller,
                &super::state::ResourceRef::Action {
                    app: super::state::WORKSPACE_GOVERNANCE_APP_ID.into(),
                    action: request.action.into(),
                },
                super::state::Verb::Admin,
            ) != super::state::CapDecision::Allow
            {
                return Err("the caller does not administer this room".into());
            }
            (workspace.id, workspace.room, spec, surface)
        };
        let json = request
            .argument
            .and_then(|value| value.strip_prefix(tilo_actions::ARGUMENTS_JSON_PREFIX))
            .unwrap_or("{}");
        let mut arguments =
            serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(json)
                .map_err(|error| format!("invalid public-room arguments: {error}"))?;
        // The broker's confirmation is the action's `confirm`.
        arguments.insert("confirm".into(), serde_json::Value::Bool(true));
        let encoded = spec
            .build_json(&arguments)
            .map_err(|error| format!("invalid public-room arguments: {error}"))?;
        if encoded.type_id != super::public_room_governance::ACTION_TYPE {
            return Err(format!(
                "invalid public-room arguments: {}",
                String::from_utf8_lossy(&encoded.payload)
            ));
        }
        let operation: super::public_room_governance::Operation =
            serde_json::from_slice(&encoded.payload)
                .map_err(|error| format!("invalid public-room operation: {error}"))?;
        let (community_request, _) = operation.request(&room.to_string());
        let answer = self
            .network
            .as_ref()
            .ok_or_else(|| "this host has no network to govern rooms through".to_string())?
            .serve_community_request(community_request)?;
        let context = tilo_actions::ActionContext::for_resource(
            &spec.label,
            resource.id.as_str(),
            &resource.kind,
            &resource.label,
        )
        .with_app(super::state::WORKSPACE_GOVERNANCE_APP_ID, "Public room");
        let mut tree = self.tree.write();
        let _ = tree.append_workspace_effect_receipt(
            workspace_id,
            request.invocation,
            request.actor.id(),
            context,
            tilo_actions::EffectReceiptStatus::Succeeded,
        );
        tree.desktop.external_revision = tree.desktop.external_revision.wrapping_add(1);
        tree.flush_to_disk();
        drop(tree);
        Ok(tilo_actions::ActionOutput {
            value: Some(answer),
            ..tilo_actions::ActionOutput::default()
        })
    }
}

/// Who `actor` is to `workspace`'s capability policy.
fn governance_caller(
    workspace: &super::state::Workspace,
    actor: &tilo_action_broker::collab_broker_capnp::BrokerActor,
) -> super::state::Actor {
    if let Some(app) = actor.id().strip_prefix("app:") {
        super::state::Actor::app(app).in_room(workspace.room.to_string())
    } else if actor.is_agent() {
        workspace.actor_for_agent(
            actor
                .id()
                .strip_prefix("agent:")
                .unwrap_or_else(|| actor.id()),
        )
    } else {
        workspace.actor_for_person(&actor.id().to_string())
    }
}

impl WorkspaceGovernanceServices for NativeWorkspaceGovernance {
    fn app_id(&self) -> &str {
        super::state::WORKSPACE_GOVERNANCE_APP_ID
    }
    fn handles(&self, resource: &str, action: &str) -> bool {
        resource.starts_with(&format!(
            "{}:",
            super::public_room_governance::RESOURCE_KIND
        )) || tilo_desktop::workspace_settings::is_governance_action(action)
    }
    fn descriptors(
        &self,
        workspace: &tilo_desktop_model::Workspace,
    ) -> Vec<tilo_actions::ActionDescriptor> {
        // Describe from the shell's own authority, not the broker's snapshot.
        let tree = self.tree.read();
        let room = workspace.room;
        tree.desktop
            .workspaces
            .iter()
            .find(|workspace| workspace.room == room)
            .map(|workspace| {
                tilo_actions::door::survey(&[
                    tilo_desktop::workspace_settings::permission_profile_resource_with_access(
                        workspace, true,
                    ),
                    // Public-room governance, the same actions the palette offers.
                    super::public_room_governance::resource(workspace),
                ])
            })
            .unwrap_or_default()
    }
    fn invoke(
        &self,
        request: &WorkspaceGovernanceRequest<'_>,
    ) -> Result<tilo_actions::ActionOutput, String> {
        if request.resource == super::public_room_governance::resource_id(request.room) {
            return self.invoke_public_room(request);
        }
        let actor = request.actor;
        let mut tree = self.tree.write();
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
        let caller = governance_caller(workspace, actor);
        let surface = tilo_desktop::workspace_settings::permission_profile_resource_with_access(
            workspace, true,
        );
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
        .with_resource_sensitivity(resource.sensitivity)
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
        Ok(tilo_actions::ActionOutput::default())
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
) -> Result<tilo_desktop::workspace_settings::PermissionProfileOperation, String> {
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
    if encoded.type_id != tilo_desktop::workspace_settings::PERMISSION_PROFILE_ACTION_TYPE {
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
    operation: tilo_desktop::workspace_settings::PermissionProfileOperation,
) -> Result<(), String> {
    match operation {
        tilo_desktop::workspace_settings::PermissionProfileOperation::Save {
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
        tilo_desktop::workspace_settings::PermissionProfileOperation::Assign {
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
        operation @ (tilo_desktop::workspace_settings::PermissionProfileOperation::GrantAppAction {
            ..
        }
        | tilo_desktop::workspace_settings::PermissionProfileOperation::RevokeAppAction {
            ..
        }) => {
            super::workspace_governance::apply_app_action_operation(
                tree,
                workspace_id,
                caller,
                &operation,
            )?;
        }
    }
    Ok(())
}

/// Grants people give applications for other apps' actions, recorded in the
/// workspace's own records by the person who gave them and noted in its
/// history.
pub struct NativeAppActionGrants(pub WorkspaceTreeHandle);
impl tilo_action_broker::services::AppActionGrants for NativeAppActionGrants {
    fn may_grant(&self, room: &str, person: &str) -> bool {
        let tree = self.0.read();
        tree.desktop
            .workspaces
            .iter()
            .find(|workspace| workspace.room.to_string() == room)
            .is_some_and(|workspace| {
                tree.governs_room(workspace.room) && workspace.may_administer(&person.to_string())
            })
    }
    fn record(
        &self,
        request: &tilo_action_broker::app_actions::AppActionConsentRequest,
        capability: collab_types::caps::CapOverride,
        answer: tilo_action_broker::app_actions::AppActionConsent,
    ) -> Option<String> {
        let mut tree = self.0.write();
        let workspace_id = tree
            .desktop
            .workspaces
            .iter()
            .find(|workspace| workspace.room.to_string() == request.room)?
            .id;
        let id = tree.grant_in_workspace(workspace_id, &request.person, capability)?;
        let label = |app: &str| {
            super::launcher::display_name_for_app_id(app)
                .unwrap_or_else(|| super::launcher::display_name_for_package_id(app))
        };
        let context = tilo_desktop_model::grants::app_action_grant_context(
            &request.room,
            &label(&request.app_id),
            &label(&request.target_app),
            &request.action_label,
            answer.lifetime(),
        );
        let _ = tree.append_workspace_effect_receipt(
            workspace_id,
            tilo_actions::InvocationId::new(),
            &request.person,
            context,
            tilo_actions::EffectReceiptStatus::Succeeded,
        );
        tree.desktop.external_revision = tree.desktop.external_revision.wrapping_add(1);
        tree.flush_to_disk();
        drop(tree);
        Some(id)
    }
    fn revoke(&self, room: &str, capability: &str) {
        let mut tree = self.0.write();
        let Some(workspace) = tree
            .desktop
            .workspaces
            .iter_mut()
            .find(|workspace| workspace.room.to_string() == room)
        else {
            return;
        };
        let workspace_id = workspace.id;
        if workspace.revoke_capability(capability) {
            tree.doc_revoke_capability(workspace_id, capability);
            tree.desktop.external_revision = tree.desktop.external_revision.wrapping_add(1);
            tree.flush_to_disk();
        }
    }
}

pub struct NativeContinuations;
impl ContinuationServices for NativeContinuations {
    fn report(
        &self,
        room: tilo_desktop_model::RoomId,
        app: String,
        instance: u64,
        schema: u16,
        metadata: Vec<u8>,
    ) -> Result<(), String> {
        super::personal_continuity_registry::global_registry()
            .lock()
            .map_err(|_| "continuation registry unavailable".to_string())?
            .report(
                room,
                app,
                super::personal_continuity::AppInstanceId(instance),
                schema,
                metadata,
            )
            .map(|_| ())
            .map_err(|e| e.to_string())
    }
    fn clear(
        &self,
        room: tilo_desktop_model::RoomId,
        app: &str,
        instance: u64,
    ) -> Result<(), String> {
        super::personal_continuity_registry::global_registry()
            .lock()
            .map_err(|_| "continuation registry unavailable".to_string())?
            .clear(
                room,
                app,
                super::personal_continuity::AppInstanceId(instance),
            );
        Ok(())
    }
    fn handoff(
        &self,
        room: tilo_desktop_model::RoomId,
        app: String,
        instance: u64,
        message: Vec<u8>,
    ) -> Result<(), String> {
        super::window_transfer::report_handoff(room, app, instance, &message)
    }
}

/// The native identity selector and fleet remain native service dependencies.
pub struct NativeCertificates;
impl tilo_action_broker::peer_actions::CertificateAuthority for NativeCertificates {
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
pub fn native_services(
    membership: Option<WorkspaceTreeHandle>,
    network: Option<Arc<super::network::ShellNetwork>>,
) -> HostServices {
    let committed_documents = membership.as_ref().map(|tree| {
        Arc::new(NativeOwnerCommitService::new(tree.clone(), network.clone()))
            as Arc<dyn NativeDocumentReads>
    });
    let governance = membership.as_ref().map(|tree| {
        Arc::new(NativeWorkspaceGovernance::new(
            tree.clone(),
            network.clone(),
        )) as Arc<dyn WorkspaceGovernanceServices>
    });
    let shortcut_library = membership.as_ref().map(|tree| {
        Arc::new(NativeShortcutLibrary(tree.clone())) as Arc<dyn ShortcutLibraryServices>
    });
    let nearby_transfer = membership
        .as_ref()
        .zip(network.as_ref())
        .map(|(tree, network)| {
            Arc::new(NativeNearbyTransfer::new(Arc::clone(network), tree.clone()))
                as Arc<dyn tilo_action_broker::services::NearbyTransferServices>
        });
    let app_action_grants = membership.as_ref().map(|tree| {
        Arc::new(NativeAppActionGrants(tree.clone()))
            as Arc<dyn tilo_action_broker::services::AppActionGrants>
    });
    let desktop_requests = membership.as_ref().map(|tree| {
        Arc::new(NativeDesktopRequests(tree.clone()))
            as Arc<dyn tilo_action_broker::services::DesktopRequestServices>
    });
    let launcher = Arc::new(NativeLauncher(membership.clone()));
    HostServices {
        app_action_access: Some(super::app_action_access::installed_resolver()),
        app_action_grants,
        desktop_requests,
        committed_documents,
        desktop_commands: None,
        native_certificates: Some(Arc::new(NativeCertificates)),
        legacy_workspace: membership.map(|tree| {
            Arc::new(NativeWorkspaceAuthority(tree)) as Arc<dyn LegacyWorkspaceProjection>
        }),
        launcher: Some(launcher),
        workspace_governance: governance,
        shortcut_library,
        keyboard_shortcuts: Some(Arc::new(NativeKeyboardShortcuts)),
        continuations: Some(Arc::new(NativeContinuations)),
        speakers: network.clone().map(|network| {
            Arc::new(NativeSpeakers(network))
                as Arc<dyn tilo_action_broker::services::SpeakerServices>
        }),
        wifi_continuity: network.clone().map(|network| {
            Arc::new(NativeWifiContinuity(network))
                as Arc<dyn tilo_action_broker::services::WifiContinuityServices>
        }),
        nearby_transfer,
        workspace_control: network.map(|network| {
            Arc::new(NativeWorkspaceControl(network)) as Arc<dyn WorkspaceControlServices>
        }),
        launchable_catalogue: launchable_catalogue_path(),
        ..Default::default()
    }
}

/// Where this session keeps closed apps' launchable actions. Unit tests
/// never touch the user's state directory.
fn launchable_catalogue_path() -> Option<std::path::PathBuf> {
    if cfg!(test) {
        return None;
    }
    xdg_paths::state_dir().map(|root| root.join("tilo/actions/launchable.json"))
}

/// A broker over `membership` with the native services an action host
/// reaches (launcher, governance, legacy projection), for tests.
#[cfg(test)]
pub(crate) fn test_broker_with_membership(
    membership: WorkspaceTreeHandle,
) -> tilo_action_broker::collab_broker::AppActionBroker {
    let authority = Arc::new(NativeWorkspaceAuthority(membership.clone()));
    let services = HostServices {
        legacy_workspace: Some(authority.clone()),
        launcher: Some(Arc::new(NativeLauncher(Some(membership.clone())))),
        workspace_governance: Some(Arc::new(NativeWorkspaceGovernance::new(
            membership.clone(),
            None,
        ))),
        shortcut_library: Some(Arc::new(NativeShortcutLibrary(membership.clone()))),
        keyboard_shortcuts: Some(Arc::new(NativeKeyboardShortcuts)),
        app_action_grants: Some(Arc::new(NativeAppActionGrants(membership.clone()))),
        desktop_requests: Some(Arc::new(NativeDesktopRequests(membership))),
        ..Default::default()
    };
    tilo_action_broker::collab_broker_capnp::CapnpBrokerState::test_broker_with_authority(
        authority, services,
    )
}

use capnp::capability::Promise;
use runtime_collab::raw::workspace_control_handle;
use std::rc::Rc;
use tokio::sync::Mutex as AsyncMutex;
/// Speaker groups over the certified personal-device network.
pub struct NativeSpeakers(pub Arc<super::network::ShellNetwork>);
impl tilo_action_broker::services::SpeakerServices for NativeSpeakers {
    fn command(&self, command: runtime_collab::SpeakerCommand) -> runtime_collab::SpeakerReply {
        crate::shell::speaker_settings::handle(&self.0, command)
    }
}
/// Wi-Fi credentials carried between the person's certified devices.
pub struct NativeWifiContinuity(pub Arc<super::network::ShellNetwork>);
impl tilo_action_broker::services::WifiContinuityServices for NativeWifiContinuity {
    fn command(
        &self,
        command: runtime_collab::WifiContinuityCommand,
    ) -> runtime_collab::WifiContinuityReply {
        let security = |security| match security {
            runtime_collab::PortableWifiSecurity::Wpa2 => os_actions::WifiSecurity::Wpa2,
            runtime_collab::PortableWifiSecurity::Wpa3 => os_actions::WifiSecurity::Wpa3,
        };
        match command {
            runtime_collab::WifiContinuityCommand::Lookup {
                ssid,
                security: mode,
            } => self
                .0
                .shared_wifi_password(&ssid, security(mode))
                .map_or(runtime_collab::WifiContinuityReply::NotFound, |password| {
                    runtime_collab::WifiContinuityReply::Password { password }
                }),
            runtime_collab::WifiContinuityCommand::Share {
                ssid,
                security: mode,
                password,
            } => {
                let password = zeroize::Zeroizing::new(password);
                match self
                    .0
                    .share_wifi_credential(&ssid, security(mode), &password)
                {
                    Ok(()) => runtime_collab::WifiContinuityReply::Shared { ssid },
                    Err(reason) => runtime_collab::WifiContinuityReply::Rejected { reason },
                }
            }
            runtime_collab::WifiContinuityCommand::ShareSavedProfile { profile_uuid } => {
                match os_actions::portable_wifi_profile_secret(&profile_uuid).and_then(|profile| {
                    self.0
                        .share_wifi_credential(&profile.ssid, profile.security, &profile.password)
                        .map(|()| profile.ssid)
                }) {
                    Ok(ssid) => runtime_collab::WifiContinuityReply::Shared { ssid },
                    Err(reason) => runtime_collab::WifiContinuityReply::Rejected { reason },
                }
            }
        }
    }
}
pub struct NativeWorkspaceControl(pub Arc<super::network::ShellNetwork>);
impl WorkspaceControlServices for NativeWorkspaceControl {
    fn open(
        &self,
        room: tilo_desktop_model::RoomId,
        target: [u8; 32],
        revalidate: Arc<dyn Fn() -> Result<(), capnp::Error> + Send + Sync>,
    ) -> Result<workspace_control_handle::Client, capnp::Error> {
        revalidate()?;
        let target = iroh::EndpointId::from_bytes(&target)
            .map_err(|_| capnp::Error::failed("invalid target device identity".into()))?;
        let session = self
            .0
            .open_workspace_control(super::network::room_topic_id(room), target)
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

    /// Public-room governance is offered through the action broker to an
    /// agent the room made its administrator, and to no one else; running it
    /// takes confirmation and Admin, and is then the network's to perform.
    #[test]
    #[allow(
        clippy::significant_drop_tightening,
        reason = "the broker host this test drives owns long-lived permits by design"
    )]
    fn public_room_governance_reaches_an_admin_agent_and_nobody_else() {
        use tilo_action_broker::collab_broker_capnp::BrokerActor;
        use tilo_actions::remote::{ActionHost as _, Caller, CatalogueQuery, PendingPoll};
        let mut workspace = super::super::state::Workspace::new(
            super::super::state::WorkspaceId(33),
            "Tilo Community",
        );
        workspace.set_owner("owner");
        workspace.channel_class = collab_types::ChannelClass::Public;
        for action in super::super::public_room_governance::ACTIONS {
            workspace.grant_capability(collab_types::CapOverride::grant(
                collab_types::Subject::agent("steward"),
                collab_types::Scope::Action {
                    app: super::super::state::WORKSPACE_GOVERNANCE_APP_ID.into(),
                    action: (*action).into(),
                },
                collab_types::Verb::Admin,
            ));
        }
        let cluster = workspace.room.to_string();
        let mut tree = super::super::state::WorkspaceTree::default();
        tree.desktop.workspaces.push(workspace);
        let membership = tree.shared();
        let host = super::super::remote_actions::BrokerActionHost::new(
            test_broker_with_membership(membership.clone()),
        );
        let catalogue_text = |caller: &Caller| {
            let mut page = host.query(
                caller,
                CatalogueQuery {
                    max_results: tilo_actions::remote::MAX_CATALOGUE_PAGE,
                    ..CatalogueQuery::default()
                },
            );
            let PendingPoll::Ready(Ok(page)) = page.poll_ready() else {
                panic!("the eager catalogue answers at once");
            };
            serde_json::to_string(&page).expect("catalogue encodes")
        };
        let steward = catalogue_text(&Caller::local_agent("steward").in_room(&cluster));
        for action in super::super::public_room_governance::ACTIONS {
            assert!(
                steward.contains(action),
                "an Admin agent is offered {action}"
            );
        }
        let bystander = catalogue_text(&Caller::local_agent("bystander").in_room(&cluster));
        assert!(
            !bystander.contains(super::super::public_room_governance::ISSUE_INVITE),
            "an agent without Admin is not offered public-room governance"
        );

        let governance = NativeWorkspaceGovernance::new(membership, None);
        let resource = super::super::public_room_governance::resource_id(&cluster);
        assert!(governance.handles(
            &resource,
            super::super::public_room_governance::ISSUE_INVITE
        ));
        let invite = |who: &str, confirm: bool| {
            governance.invoke(&WorkspaceGovernanceRequest {
                room: &cluster,
                actor: &BrokerActor::agent(format!("agent:{who}")),
                resource: &resource,
                action: super::super::public_room_governance::ISSUE_INVITE,
                argument: None,
                confirmation: &serde_json::json!({ "confirm": confirm }),
                invocation: tilo_actions::InvocationId(uuid::Uuid::from_u128(1)),
            })
        };
        assert!(
            invite("steward", false)
                .unwrap_err()
                .contains("confirmation")
        );
        assert!(
            invite("bystander", true)
                .unwrap_err()
                .contains("administer")
        );
        // Confirmed and Admin: the service hands it to the network, which
        // this fixture has none of; the refusal is that, not a gate.
        assert!(invite("steward", true).unwrap_err().contains("network"));
    }

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

    /// A workspace owner opening an app makes it an author of the space,
    /// recorded once as their grant; someone without authority records
    /// nothing, so the app launches a viewer.
    #[test]
    fn an_owner_opening_an_app_grants_it_authorship_once() {
        use super::super::state::WorkspaceExt as _;
        use collab_types::caps::{ResourceRef, Verb};
        let mut tree = crate::shell::state::WorkspaceTree::default();
        let mut workspace = crate::shell::state::Workspace::new(
            crate::shell::state::WorkspaceId(7),
            "authorship test",
        );
        workspace.set_owner("owner".to_string());
        tree.desktop.workspaces.push(workspace.clone());
        let handle = tree.shared();
        let launcher = NativeLauncher(Some(handle.clone()));
        let app = "so.tilo.word-processor";

        let mut stranger_copy = workspace.clone();
        launcher.grant_authorship("stranger", app, &mut stranger_copy);
        let actor = stranger_copy.actor_for_app(app);
        assert!(!stranger_copy.allows(&actor, &ResourceRef::Workspace, Verb::Write));

        let mut launch_copy = workspace.clone();
        launcher.grant_authorship("owner", app, &mut launch_copy);
        let actor = launch_copy.actor_for_app(app);
        assert!(launch_copy.allows(&actor, &ResourceRef::Workspace, Verb::Write));
        let stored = handle.read().desktop.workspaces[0].capabilities.len();

        let mut again = handle.read().desktop.workspaces[0].clone();
        launcher.grant_authorship("owner", app, &mut again);
        assert_eq!(
            handle.read().desktop.workspaces[0].capabilities.len(),
            stored,
            "granted once"
        );
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
                author: None,
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
                        author: None,
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
