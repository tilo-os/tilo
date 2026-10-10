#![allow(refining_impl_trait)]
//! Adapters from the shared broker to the native session's existing services.
use super::state::WorkspaceAuthority as _;
use super::state::WorkspaceTreeHandle;
#[cfg(target_os = "linux")]
mod airdrop_transfer;
#[cfg(target_os = "linux")]
pub mod approved_checks;
mod committed_document;
#[cfg(target_os = "linux")]
mod file_clipboard;
#[cfg(test)]
#[cfg(target_os = "linux")]
pub(crate) use file_clipboard::test_support::TestTransferPause;
#[cfg(test)]
#[path = "broker_services/native_effect_tests.rs"]
mod native_effect_tests;
mod nearby_transfer;
mod resource_views;
mod task_execution;
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
    pub(crate) fn owner(&self) -> Option<super::native_workspace_owner::NativeWorkspaceOwner> {
        self.owner.clone()
    }
}
impl tilo_peer_state::owner_commit::OwnerCommitService for NativeOwnerCommitService {
    fn commit_member_effect_receipt(
        &self,
        request: tilo_peer_state::owner_commit::OwnerCommitRequest,
        caller: tilo_user_identity::DeviceCert,
    ) -> tilo_peer_state::owner_commit::CommitFuture {
        let owner = self.owner.clone();
        let network = self.network.clone();
        Box::pin(async move {
            let owner = owner.ok_or("native effect governor unavailable")?;
            let room = super::state::RoomId::parse(&request.room).ok_or("invalid effect room")?;
            if let Some(network) = network.as_ref() {
                network.check_native_custody(&owner, room)?;
                if network.governed_elsewhere(room) {
                    return Err("member receipt requires the actual governing owner".into());
                }
            }
            let (key, governor) = tilo_identity_host::broker::trusted_current_device(&request.room)
                .map_err(|error| format!("effect governor custody unavailable: {error}"))?;
            let receipt = owner.commit_member_effect_receipt(
                &request,
                &caller,
                &governor,
                &key,
                std::time::SystemTime::now(),
            )?;
            if let Some(network) = network {
                network.schedule_native_owner_publication(room);
            }
            Ok(receipt)
        })
    }
    fn requires_room(&self, room: &str) -> bool {
        super::state::RoomId::parse(room)
            .is_some_and(|room| self.tree.read().commits_native_documents(room))
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
                // The room's receipts are its governor's to record.
                if let tilo_peer_state::owner_commit::CommitOperation::EffectReceipt {
                    redacted,
                    ..
                } = &request.operation
                    && network.governed_elsewhere(room)
                {
                    return record_receipt_elsewhere(&network, &owner, room, &request, redacted)
                        .await;
                }
            }
            if matches!(
                request.operation,
                tilo_peer_state::owner_commit::CommitOperation::WorkspaceTaskCommand { .. }
            ) {
                return owner.commit_workspace_task_command(request, std::time::SystemTime::now());
            }
            if matches!(
                request.operation,
                tilo_peer_state::owner_commit::CommitOperation::EffectReceipt { .. }
            ) {
                let (key, governor) =
                    tilo_identity_host::broker::trusted_current_device(&request.room)
                        .map_err(|error| format!("effect governor custody unavailable: {error}"))?;
                owner.commit_effect_receipt(&request, &governor, &key, std::time::SystemTime::now())
            } else {
                owner.commit_document(request, std::time::SystemTime::now())
            }
        })
    }
}

impl tilo_action_broker::services::WorkspaceMemberships for NativeOwnerCommitService {
    fn requires_room(&self, room: &str) -> bool {
        tilo_peer_state::owner_commit::OwnerCommitService::requires_room(self, room)
    }

    fn health(&self) -> Result<(), String> {
        self.owner
            .as_ref()
            .ok_or("native membership owner unavailable")?
            .health()
    }

    fn task_snapshot(
        &self,
        room: &str,
    ) -> Result<tilo_peer_state::workspace_task::WorkspaceTaskSnapshot, String> {
        let room = super::state::RoomId::parse(room).ok_or("invalid native task room")?;
        self.owner
            .as_ref()
            .ok_or("native task owner unavailable")?
            .workspace_task_snapshot(room)
    }

    fn task_agent_authority(
        &self,
        invocation: &tilo_action_broker::services::WorkspaceTaskAgentInvocation,
    ) -> Result<tilo_peer_state::workspace_task::WorkspaceTaskAuthority, String> {
        if !tilo_action_broker::services::WorkspaceMemberships::requires_room(
            self,
            invocation.room(),
        ) {
            return Err("native task agent workspace is unavailable".into());
        }
        let (_, attestor) =
            tilo_identity_host::broker::trusted_current_device(invocation.room())
                .map_err(|error| format!("native task host custody unavailable: {error}"))?;
        Ok(
            tilo_peer_state::workspace_task::WorkspaceTaskAuthority::Agent {
                attestor,
                agent_id: invocation.agent_id().into(),
            },
        )
    }

    fn execute(
        &self,
        request: tilo_peer_state::owner_commit::OwnerCommitRequest,
    ) -> tilo_peer_state::owner_commit::CommitFuture {
        let owner = self.owner.clone();
        let network = self.network.clone();
        Box::pin(async move {
            let owner = owner.ok_or("native membership owner unavailable")?;
            let room = super::state::RoomId::parse(&request.room)
                .ok_or("invalid native membership room")?;
            if let Some(network) = network.as_ref() {
                network.check_native_custody(&owner, room)?;
                if network.governed_elsewhere(room) {
                    return Err("workspace membership requires the governing owner".into());
                }
            }
            let receipt = if matches!(
                request.operation,
                tilo_peer_state::owner_commit::CommitOperation::WorkspaceTaskCommand { .. }
            ) {
                owner.commit_workspace_task_command(request, std::time::SystemTime::now())?
            } else {
                let (key, governor) =
                    tilo_identity_host::broker::trusted_current_device(&request.room)
                        .map_err(|error| format!("workspace governor is unavailable: {error}"))?;
                owner.commit_workspace_membership_command(
                    request,
                    &governor,
                    &key,
                    std::time::SystemTime::now(),
                )?
            };
            if let Some(network) = network {
                network.schedule_native_owner_publication(room);
            }
            Ok(receipt)
        })
    }
}

/// Have the governor of `room` record an effect receipt this device's broker
/// produced, and answer with this device's committed view of the room.
async fn record_receipt_elsewhere(
    network: &super::network::ShellNetwork,
    owner: &super::native_workspace_owner::NativeWorkspaceOwner,
    room: super::state::RoomId,
    request: &tilo_peer_state::owner_commit::OwnerCommitRequest,
    redacted: &[u8],
) -> Result<tilo_peer_state::owner_commit::OwnerCommitReceipt, String> {
    let tilo_peer_state::owner_commit::CommitOperation::EffectReceipt { invocation, .. } =
        &request.operation
    else {
        return Err("missing effect receipt operation".into());
    };
    let expected = tilo_peer_state::StatePeer::validate_workspace_effect_receipt_encoding(
        *invocation,
        redacted,
    )?;
    network
        .record_governed_effect_receipt(room, *invocation, redacted.to_vec())
        .await?;
    // A transport acknowledgement is not our commit barrier: wait until the
    // governor's signed receipt is in this device's actual admitted room.
    tokio::time::timeout(GOVERNED_OPEN_TIMEOUT, async {
        loop {
            network.check_native_custody(owner, room)?;
            let receipt = owner.with_current(room, |view| {
                let effects = tilo_desktop_model::projection::workspace_effect_receipts_of(
                    view.peer()
                        .admitted_room()
                        .ok_or("effect room unavailable")?,
                );
                if let Some(actual) = effects
                    .iter()
                    .find(|effect| effect.invocation == expected.invocation)
                {
                    if actual != &expected {
                        return Err(
                            "admitted terminal receipt differs from this broker's outcome".into(),
                        );
                    }
                    return Ok(Some(tilo_peer_state::owner_commit::OwnerCommitReceipt {
                        binding: request.binding()?,
                        generation: view.generation(),
                        aggregate_hash: *blake3::hash(&view.peer().durable()?).as_bytes(),
                    }));
                }
                Ok(None)
            })?;
            if let Some(receipt) = receipt {
                return Ok(receipt);
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
    })
    .await
    .map_err(|_| "signed effect receipt did not reach the native owner in time; outcome unknown")?
}

pub struct NativeWorkspaceAuthority(pub WorkspaceTreeHandle);
impl WorkspaceAuthority for NativeWorkspaceAuthority {
    /// Borrows the committed tree under the compositor's read lock for one
    /// short policy read; nothing is copied.
    fn with_tree(
        &self,
        read: &mut dyn FnMut(&tilo_desktop_model::DesktopTree),
    ) -> Result<(), String> {
        read(&self.0.read().desktop);
        Ok(())
    }
    fn legacy_journal_root(&self) -> Option<std::path::PathBuf> {
        self.0.read().persist_path.as_ref().map(|path| {
            let mut name = path.clone().into_os_string();
            name.push(".invocations");
            name.into()
        })
    }
    /// A tree write that changes policy announces as its guard is released;
    /// the fleet and registry announce on save and through the identity
    /// directory's watch; the native owner announces each publication
    /// (`tilo_identity_host::policy_change`).
    fn announces_policy_changes(&self) -> bool {
        true
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

/// Run `call` against the filesystem daemon's content store as the desktop
/// session, connecting on first use and again after the daemon went away.
///
/// # Errors
/// Returns why the store could not be reached, or `call`'s refusal.
pub(super) fn with_session_blobs<T>(
    call: impl FnOnce(&runtime_fs::blobs::BlobClient) -> Result<T, runtime_fs::blobs::BlobError>,
) -> Result<T, runtime_fs::blobs::BlobError> {
    use runtime_fs::blobs::{BlobClient, BlobError};

    let mut session = SESSION_BLOBS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let client = if let Some(client) = session.as_ref() {
        client.clone()
    } else {
        let socket = ::launcher::services::blob_socket();
        let client = BlobClient::connect_session(&socket)?;
        *session = Some(client.clone());
        client
    };
    drop(session);
    let result = call(&client);
    if matches!(result, Err(BlobError::Disconnected(_))) {
        // Reconnect on the next call rather than keep a dead session.
        *SESSION_BLOBS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
    }
    result
}

impl LaunchContentReader for SessionContentStore {
    fn read(&self, room: &str, app: &str, hash: &str) -> Result<Vec<u8>, String> {
        use runtime_fs::blobs::ReadOutcome;

        let result = with_session_blobs(|client| {
            client
                .scope(room, app)
                .and_then(|scoped| scoped.read_all(hash))
        });
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
    let dir = tilo_paths::xdg::runtime_dir()
        .ok_or("launch staging needs XDG_RUNTIME_DIR")?
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
/// a workspace author granting an app authorship before launching it; with
/// the network, an app opened in a room another device governs is opened by
/// that governor, which creates and places its record.
pub struct NativeLauncher(
    pub Option<WorkspaceTreeHandle>,
    pub Option<Arc<super::network::ShellNetwork>>,
);

/// How long a launch waits for a room's governor to open the app.
const GOVERNED_OPEN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(35);

impl NativeLauncher {
    /// Open `app` through the governor of `workspace`'s room, when another
    /// device governs it and opens its apps. `None` when this session opens
    /// it itself.
    ///
    /// Such a room only holds records its governor created, so a local
    /// launch would open an app whose document the room does not hold. The
    /// governor's open places the window in the room; the session then opens
    /// its own instance of that window through [`super::peer_windows`]. A
    /// host governing the room (a node's relay) runs no apps: the session
    /// runs the app and publishes its window as its own, as it does a legacy
    /// window, for the room's members to watch.
    fn open_through_governor(
        &self,
        app: &str,
        workspace: &tilo_desktop_model::Workspace,
    ) -> Option<Result<(), String>> {
        let resource = format!(
            "{}{app}",
            tilo_desktop_model::commands::OPEN_APP_RESOURCE_PREFIX
        );
        let answer = self
            .1
            .as_ref()
            .filter(|network| network.governor_opens_apps(workspace.room))?
            .invoke_governor(
                workspace.room,
                tilo_desktop_model::commands::APP_ID,
                &resource,
                tilo_desktop_model::commands::OPEN_APP_ACTION,
                None,
            );
        Some(match answer.wait_timeout(GOVERNED_OPEN_TIMEOUT) {
            Ok(Some(result)) => result.map(drop),
            Ok(None) => Err("the workspace host stopped before opening the app".into()),
            Err(_) => Err("the workspace host did not open the app in time".into()),
        })
    }

    /// Record `person` granting `app` authorship of `workspace`'s space and
    /// add the grant to the launch's copy, unless the app already holds it.
    /// The tree checks `person`'s own authority; a refusal launches a viewer.
    fn grant_authorship(
        &self,
        person: &str,
        app: &str,
        workspace: &mut tilo_desktop_model::Workspace,
    ) {
        let Some(tree) = &self.0 else { return };
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
        let Some(stored) = tree.grant_app_authorship(id, &person.to_string(), app) else {
            return;
        };
        drop(tree);
        workspace.capabilities.push(stored);
    }
}

/// The launchable id of host application `desktop_id`, so the workspace's
/// `app.<id>` open action can name an application no package provides.
pub(crate) const HOST_APPLICATION_PREFIX: &str = "host:";

/// Tilo packages first, then every host application no installed package
/// answers for, marked unconfined.
fn launchable_applications(
    packages: Vec<super::launcher::InstalledApp>,
    hosts: Vec<super::host_apps::HostApp>,
) -> Vec<LaunchableApplication> {
    let package_ids: std::collections::HashSet<String> = packages
        .iter()
        .map(|app| app.package_id.to_ascii_lowercase())
        .collect();
    let hosts = hosts
        .into_iter()
        .filter(|app| !package_ids.contains(&app.desktop_id.to_ascii_lowercase()))
        .map(|app| LaunchableApplication {
            package_id: format!("{HOST_APPLICATION_PREFIX}{}", app.desktop_id),
            display_name: app.name,
            unconfined: true,
        });
    packages
        .into_iter()
        .map(|app| LaunchableApplication {
            package_id: app.package_id,
            display_name: app.display_name,
            unconfined: false,
        })
        .chain(hosts)
        .collect()
}

/// Open host application `desktop_id` in `workspace`, where its X11 bridge,
/// if it needs one, is that workspace's own.
fn launch_host_application(
    desktop_id: &str,
    workspace: &tilo_desktop_model::Workspace,
    content: LaunchContent<'_>,
) -> Result<(), String> {
    if !content.hash.is_empty() {
        return Err("opening launch content in a host application is unsupported".into());
    }
    super::host_apps::launch(desktop_id, Some(workspace.room.to_string()))
        .map(drop)
        .map_err(|e| e.to_string())
}

impl LauncherServices for NativeLauncher {
    fn applications(&self) -> Vec<LaunchableApplication> {
        launchable_applications(super::launcher::gui_apps(), super::host_apps::host_apps())
    }
    fn launch(&self, app: &str, workspace: &tilo_desktop_model::Workspace) -> Result<(), String> {
        if let Some(desktop_id) = app.strip_prefix(HOST_APPLICATION_PREFIX) {
            return launch_host_application(
                desktop_id,
                workspace,
                LaunchContent {
                    hash: "",
                    caller: None,
                    author: None,
                },
            );
        }
        if let Some(opened) = self.open_through_governor(app, workspace) {
            return opened;
        }
        // Opening an app yourself makes it an author. A launch that only
        // forwards the workspace's existing grants opens a viewer, and
        // typing into it does nothing.
        let mut workspace = workspace.clone();
        let person = super::state::local_user_id_for_room(&workspace.room);
        self.grant_authorship(&person, app, &mut workspace);
        super::launcher::launch_in_workspace(app, &workspace)
            .map(|_| ())
            .map_err(|e| e.to_string())
    }
    fn begin_launch(
        &self,
        app: &str,
        package_root: Option<&str>,
        workspace: &tilo_desktop_model::Workspace,
        author: Option<&str>,
    ) -> Result<tilo_action_broker::services::LaunchReadiness, String> {
        if let Some(desktop_id) = app.strip_prefix(HOST_APPLICATION_PREFIX) {
            if package_root.is_some() {
                return Err("a host application cannot name a Tilo package root".into());
            }
            return super::launch_readiness::observe(|| {
                super::host_apps::launch(desktop_id, Some(workspace.room.to_string()))
                    .map(drop)
                    .map_err(|error| error.to_string())
            });
        }
        if self
            .1
            .as_ref()
            .is_some_and(|network| network.governor_opens_apps(workspace.room))
        {
            return Err(
                "observed package startup must be requested from the workspace host".into(),
            );
        }
        let catalog = package_store::Catalog::from_env().map_err(|error| error.to_string())?;
        let selected = match package_root {
            Some(root) => {
                let root = root.parse().map_err(|_| "invalid installed package root")?;
                tilo_installed_apps::read_exact(&catalog, app, root)
                    .map_err(|error| error.to_string())?
            }
            None => tilo_installed_apps::read(&catalog)
                .map_err(|error| error.to_string())?
                .into_iter()
                .find(|installed| installed.package_id == app),
        }
        .ok_or_else(|| {
            "the exact selected application package is no longer installed".to_string()
        })?;
        let mut workspace = workspace.clone();
        if let Some(person) = author {
            self.grant_authorship(person, app, &mut workspace);
        }
        super::launch_readiness::observe(|| {
            super::launcher::launch_resolved_in_workspace(selected, &workspace)
                .map_err(|error| error.to_string())
        })
    }
    fn begin_launch_from_original(
        &self,
        app: &str,
        package_root: Option<&str>,
        workspace: &tilo_desktop_model::Workspace,
        author: Option<&str>,
        original: Option<tilo_action_broker::services::ObservedApplicationLaunch>,
    ) -> Result<tilo_action_broker::services::LaunchReadiness, String> {
        let native_tree = self
            .0
            .as_ref()
            .filter(|tree| tree.read().installed_native_owner().is_some());
        let Some(tree) = native_tree else {
            return self.begin_launch(app, package_root, workspace, author);
        };
        if app.starts_with(HOST_APPLICATION_PREFIX) {
            return self.begin_launch(app, package_root, workspace, author);
        }
        if self
            .1
            .as_ref()
            .is_some_and(|network| network.governor_opens_apps(workspace.room))
        {
            return Err(
                "observed package startup must be requested from the workspace host".into(),
            );
        }
        let original =
            original.ok_or("native observed launch requires its original certified caller")?;
        if !original.matches(app, package_root, workspace) {
            return Err("original application launch scope mismatch".into());
        }
        original.certificate()?;
        let catalog = package_store::Catalog::from_env().map_err(|error| error.to_string())?;
        let selected = match package_root {
            Some(root) => tilo_installed_apps::read_exact(
                &catalog,
                app,
                root.parse().map_err(|_| "invalid installed package root")?,
            )
            .map_err(|error| error.to_string())?,
            None => tilo_installed_apps::read(&catalog)
                .map_err(|error| error.to_string())?
                .into_iter()
                .find(|installed| installed.package_id == app),
        }
        .ok_or("the exact selected application package is no longer installed")?;
        super::launch_readiness::observe(|| {
            super::launcher::queue_observed_native_launch(
                selected,
                tree.clone(),
                workspace.room,
                original,
            )
            .map_err(|error| error.to_string())
        })
    }
    fn launch_content(
        &self,
        app: &str,
        workspace: &tilo_desktop_model::Workspace,
        content: LaunchContent<'_>,
    ) -> Result<(), String> {
        if let Some(desktop_id) = app.strip_prefix(HOST_APPLICATION_PREFIX) {
            return launch_host_application(desktop_id, workspace, content);
        }
        if content.hash.is_empty()
            && let Some(opened) = self.open_through_governor(app, workspace)
        {
            return opened;
        }
        let mut workspace = workspace.clone();
        if let Some(person) = content.author {
            self.grant_authorship(person, app, &mut workspace);
        }
        let document = stage_launch_content(app, content, &mut workspace, &SessionContentStore)?;
        super::launcher::launch_document_in_workspace(app, &workspace, document.as_deref())
            .map(|_| ())
            .map_err(|e| e.to_string())
    }
    fn authenticated_launch(
        &self,
        pid: u32,
        app: &str,
        room: &str,
    ) -> Result<Option<tilo_action_broker::services::AuthenticatedApplicationLaunch>, String> {
        Ok(super::launcher::authenticated_window_launch(pid, app, room))
    }
    fn attest_process(&self, pid: u32, app: &str, room: &str) -> bool {
        super::launcher::attest_trusted_service_launch_for_pid(pid, app, room).is_some()
            || super::launcher::consume_in_process_launch(pid, app, room)
    }
    #[cfg(target_os = "linux")]
    fn attest_executing_process(
        &self,
        launcher_pid: u32,
        peer: &runtime_portal::process_binding::ExecutingPeer,
        app: &str,
        room: &str,
    ) -> bool {
        let Some((_, start)) = super::launcher::proc_stat(launcher_pid) else {
            return false;
        };
        super::launcher::attest_trusted_service_launch_for_pid(launcher_pid, app, room).is_some()
            && peer.belongs_to(launcher_pid, start)
            && super::launcher::attest_trusted_service_launch_for_pid(launcher_pid, app, room)
                .is_some()
    }
    fn application_creation_evidence(
        &self,
        pid: u32,
        app: &str,
        room: &str,
    ) -> Result<Option<tilo_action_broker::services::ApplicationCreationEvidence>, String> {
        Ok(
            super::launcher::attest_trusted_service_launch_for_pid(pid, app, room)
                .and_then(|identity| identity.application_creation),
        )
    }
    fn note_acted_on(&self, app: &str, _room: &str) {
        super::launcher::note_launch_acted_on(app);
    }
    fn closes_applications(&self) -> bool {
        super::app_close::installed()
    }
    fn close(&self, app: &str, workspace: &tilo_desktop_model::Workspace) -> Result<(), String> {
        super::app_close::request(super::app_close::AppCloseRequest {
            app_id: app.to_string(),
            room: workspace.room,
        })
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

/// Workspace governance (permission profiles) applied under the shell's
/// canonical capability authority. There is no application behind these
/// actions: the shell validates, mutates, records the effect and persists.
/// With the network, a room another device governs has that governor apply
/// them, with this device's own authority there.
pub struct NativeWorkspaceGovernance(
    pub WorkspaceTreeHandle,
    pub Option<Arc<super::network::ShellNetwork>>,
);
impl NativeWorkspaceGovernance {
    /// The network and room when another device governs `room`. The native
    /// owner locks the tree under its own mutex, so this must not be asked
    /// while the tree is held.
    fn governor_elsewhere(
        &self,
        room: &str,
    ) -> Option<(&Arc<super::network::ShellNetwork>, super::state::RoomId)> {
        let room = self
            .0
            .read()
            .desktop
            .workspaces
            .iter()
            .find(|workspace| workspace.room.to_string() == room)
            .map(|workspace| workspace.room)?;
        self.1
            .as_ref()
            .filter(|network| network.governed_elsewhere(room))
            .map(|network| (network, room))
    }
}
impl WorkspaceGovernanceServices for NativeWorkspaceGovernance {
    fn app_id(&self) -> &str {
        super::state::WORKSPACE_GOVERNANCE_APP_ID
    }
    fn handles(&self, action: &str) -> bool {
        tilo_desktop::workspace_settings::is_governance_action(action)
    }
    fn descriptors(
        &self,
        workspace: &tilo_desktop_model::Workspace,
    ) -> Vec<tilo_actions::ActionDescriptor> {
        // Describe from the shell's own authority, not the broker's snapshot.
        let tree = self.0.read();
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
                ])
            })
            .unwrap_or_default()
    }
    fn invoke(&self, request: &WorkspaceGovernanceRequest<'_>) -> Result<(), String> {
        let actor = request.actor;
        let elsewhere = self.governor_elsewhere(request.room);
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
        let caller = if let Some(app) = actor.id().strip_prefix("app:") {
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
        };
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
        // The resource the action is invoked on, as the catalogue checked it.
        if workspace.decide(
            &caller,
            &super::state::ResourceRef::Action {
                app: super::state::WORKSPACE_GOVERNANCE_APP_ID.into(),
                resource: resource.id.as_str().to_owned(),
                action: request.action.into(),
            },
            super::state::Verb::Admin,
        ) != super::state::CapDecision::Allow
        {
            return Err("the caller is not allowed to manage permission profiles".into());
        }
        let operation = decode_permission_profile_operation(spec, request.argument)?;
        if let Some((network, room)) = elsewhere.filter(|(_, room)| *room == workspace.room) {
            drop(tree);
            return govern_elsewhere(network, room, operation);
        }
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
        tree.queue_save();
        drop(tree);
        Ok(())
    }
}

/// Have the governor of `room` apply a permission-profile `operation` as a
/// desktop command from this device.
fn govern_elsewhere(
    network: &super::network::ShellNetwork,
    room: super::state::RoomId,
    operation: tilo_desktop::workspace_settings::PermissionProfileOperation,
) -> Result<(), String> {
    use tilo_desktop::workspace_settings::PermissionProfileOperation as Operation;
    use tilo_desktop_model::commands::{BoundCommand, CommandSource, DesktopCommand};
    let command = match operation {
        Operation::Save {
            name,
            description,
            rules,
        } => DesktopCommand::SavePermissionProfile {
            profile_json: serde_json::to_string(&collab_types::PermissionProfile {
                name,
                description,
                rules,
            })
            .map_err(|error| error.to_string())?,
        },
        Operation::Assign {
            profile,
            subject_kind,
            subject,
            expires_unix,
        } => DesktopCommand::AssignPermissionProfile {
            profile,
            subject_kind,
            subject,
            expires_unix,
        },
        Operation::GrantAppAction {
            app,
            target_app,
            resource,
            action,
            lifetime,
        } => DesktopCommand::GrantAppAction {
            app,
            target_app,
            resource,
            action,
            lifetime,
        },
        Operation::RevokeAppAction {
            app,
            target_app,
            resource,
            action,
        } => DesktopCommand::RevokeAppAction {
            app,
            target_app,
            resource,
            action,
        },
    };
    let bound = serde_json::to_string(&BoundCommand {
        source: CommandSource {
            room: room.to_string(),
            heads: Vec::new(),
        },
        command,
    })
    .map_err(|error| error.to_string())?;
    let answer = network.invoke_governor(
        room,
        tilo_desktop_model::commands::APP_ID,
        tilo_desktop_model::commands::RESOURCE,
        tilo_desktop_model::commands::ACTION,
        Some(serde_json::json!({ "command": bound })),
    );
    match answer.wait_timeout(GOVERNED_OPEN_TIMEOUT) {
        Ok(Some(result)) => result.map(drop),
        Ok(None) => Err("the workspace host stopped before applying the change".into()),
        Err(_) => Err("the workspace host did not apply the change in time".into()),
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
            &format!("{} · {}", request.resource_label, request.action_label),
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
        tree.queue_save();
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
            tree.queue_save();
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
        if matches!(
            ui::collaboration::handoff::HandoffReport::decode(&message),
            Some(ui::collaboration::handoff::HandoffReport::OwnerSubscribe { .. })
        ) {
            super::personal_continuity_registry::global_registry()
                .lock()
                .map_err(|_| "continuation registry unavailable".to_owned())?
                .refresh_owner_view(
                    room,
                    &app,
                    super::personal_continuity::AppInstanceId(instance),
                );
            return Ok(());
        }
        if let Some(ui::collaboration::handoff::HandoffReport::OwnerTakeover {
            token,
            schema,
            metadata,
        }) = ui::collaboration::handoff::HandoffReport::decode(&message)
        {
            use super::continuity_policy::{ContinuityCapability, ContinuityPolicySnapshot};
            if !ContinuityPolicySnapshot::load().permits(ContinuityCapability::AppContinuations) {
                return Err("app continuation permission is disabled".into());
            }
            return super::personal_continuity_registry::global_registry()
                .lock()
                .map_err(|_| "continuation registry unavailable".to_owned())?
                .claim_owner_view(
                    room,
                    app,
                    super::personal_continuity::AppInstanceId(instance),
                    token,
                    ui::collaboration::ContinuationMetadata {
                        schema_version: schema,
                        bytes: metadata,
                    },
                );
        }
        super::window_transfer::report_handoff(room, app, instance, &message)
    }
}

/// The native identity selector and fleet remain native service dependencies.
pub struct NativeCertificates {
    origin: Result<
        tilo_action_broker::peer_actions::PolicyOrigin,
        tilo_action_broker::peer_actions::PolicyClockError,
    >,
}
impl Default for NativeCertificates {
    fn default() -> Self {
        Self {
            origin: tilo_action_broker::peer_actions::OwnerPolicyClock::new()
                .map(|clock| clock.new_origin()),
        }
    }
}
impl tilo_action_broker::peer_actions::CertificateAuthority for NativeCertificates {
    fn policy_time(&self) -> Result<Option<tilo_action_broker::peer_actions::PolicyTime>, String> {
        self.origin
            .as_ref()
            .map_err(ToString::to_string)?
            .capture()
            .map(Some)
            .map_err(|error| error.to_string())
    }
    fn authorize_with_policy_time(
        &self,
        room: uuid::Uuid,
        certificate: &tilo_user_identity::DeviceCert,
        time: &tilo_action_broker::peer_actions::PolicyTime,
    ) -> Result<(), String> {
        let origin = self.origin.as_ref().map_err(ToString::to_string)?;
        if !origin.owns(time) {
            return Err("native certificate policy origin changed".into());
        }
        let now = time.now_ns().map_err(|error| error.to_string())?;
        tilo_user_identity::verify_device_cert_at(certificate, now)
            .map_err(|_| "native certificate expired at retained policy time")?;
        self.authorize(room, certificate)
    }
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
fn native_document_reads(
    tree: &WorkspaceTreeHandle,
    network: Option<Arc<super::network::ShellNetwork>>,
) -> Arc<dyn NativeDocumentReads> {
    Arc::new(NativeOwnerCommitService::new(tree.clone(), network))
}

fn native_workspace_memberships(
    tree: &WorkspaceTreeHandle,
    network: Option<Arc<super::network::ShellNetwork>>,
) -> Arc<dyn tilo_action_broker::services::WorkspaceMemberships> {
    Arc::new(NativeOwnerCommitService::new(tree.clone(), network))
}

fn native_task_execution(
    tree: &WorkspaceTreeHandle,
    network: Option<Arc<super::network::ShellNetwork>>,
) -> Arc<dyn tilo_action_broker::services::WorkspaceTaskExecution> {
    Arc::new(NativeOwnerCommitService::new(tree.clone(), network))
}

fn native_resource_views(
    membership: Option<&WorkspaceTreeHandle>,
    network: Option<&Arc<super::network::ShellNetwork>>,
) -> Option<Arc<dyn tilo_action_broker::services::NativeResourceViews>> {
    membership.zip(network).map(|(tree, network)| {
        Arc::new(resource_views::NativeResourceViews {
            owner: Arc::new(NativeOwnerCommitService::new(
                tree.clone(),
                Some(network.clone()),
            )),
            network: network.clone(),
        }) as Arc<dyn tilo_action_broker::services::NativeResourceViews>
    })
}

pub fn native_services(
    membership: Option<WorkspaceTreeHandle>,
    network: Option<Arc<super::network::ShellNetwork>>,
) -> HostServices {
    let committed_documents = membership
        .as_ref()
        .map(|tree| native_document_reads(tree, network.clone()));
    let workspace_memberships = membership
        .as_ref()
        .map(|tree| native_workspace_memberships(tree, network.clone()));
    let task_execution = membership
        .as_ref()
        .map(|tree| native_task_execution(tree, network.clone()));
    let resource_views = native_resource_views(membership.as_ref(), network.as_ref());
    let governance = membership.as_ref().map(|tree| {
        Arc::new(NativeWorkspaceGovernance(tree.clone(), network.clone()))
            as Arc<dyn WorkspaceGovernanceServices>
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
    #[cfg(target_os = "linux")]
    let approved_checks = membership
        .as_ref()
        .map(|tree| approved_checks::service(tree.clone(), network.clone()));
    let launcher = Arc::new(NativeLauncher(membership.clone(), network.clone()));
    let display_control = membership
        .as_ref()
        .zip(settings_store::installed_source())
        .map(|(tree, source)| {
            Arc::new(super::display_control::NativeDisplayControl {
                tree: tree.downgrade(),
                source,
            }) as Arc<dyn tilo_action_broker::services::DisplayControlServices>
        });
    let local_person = membership.as_ref().map(|tree| {
        Arc::new(super::local_person::NativeLocalPerson {
            tree: tree.downgrade(),
        }) as Arc<dyn tilo_action_broker::services::LocalPersonServices>
    });
    HostServices {
        #[cfg(target_os = "linux")]
        approved_checks,
        #[cfg(target_os = "linux")]
        file_clipboard: membership.as_ref().map(|tree| {
            Arc::new(file_clipboard::NativeFileClipboard::new(tree.clone()))
                as Arc<dyn tilo_action_broker::services::FileClipboardServices>
        }),
        app_action_access: Some(super::app_action_access::installed_resolver()),
        app_action_grants,
        committed_documents,
        workspace_memberships,
        task_execution,
        resource_views,
        desktop_commands: None,
        native_certificates: Some(Arc::new(NativeCertificates::default())),
        local_person,
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
        // The session's records, installed before any broker starts.
        settings: settings_store::installed_source(),
        display_control,
        live_activities: Some(super::live_activities::board()),
        ..Default::default()
    }
}

/// Where this session keeps closed apps' launchable actions. Unit tests
/// never touch the user's state directory.
fn launchable_catalogue_path() -> Option<std::path::PathBuf> {
    if cfg!(test) {
        return None;
    }
    tilo_paths::state_dir().map(|root| root.join("tilo/actions/launchable.json"))
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
        launcher: Some(Arc::new(NativeLauncher(Some(membership.clone()), None))),
        workspace_governance: Some(Arc::new(NativeWorkspaceGovernance(
            membership.clone(),
            None,
        ))),
        shortcut_library: Some(Arc::new(NativeShortcutLibrary(membership.clone()))),
        keyboard_shortcuts: Some(Arc::new(NativeKeyboardShortcuts)),
        app_action_grants: Some(Arc::new(NativeAppActionGrants(membership))),
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

    #[test]
    fn native_certificate_policy_time_is_retained_and_retires_with_its_service() {
        use tilo_action_broker::peer_actions::CertificateAuthority;

        let authority = NativeCertificates::default();
        let first = authority.policy_time().unwrap().unwrap();
        let second = authority.policy_time().unwrap().unwrap();
        assert!(first.same_origin(&second));
        let initial = first.now_ns().unwrap();
        assert!(second.now_ns().unwrap() >= initial);
        let replacement = NativeCertificates::default();
        assert!(!first.same_origin(&replacement.policy_time().unwrap().unwrap()));
        drop(authority);
        assert!(first.now_ns().is_err());
        assert!(second.now_ns().is_err());
        assert!(replacement.policy_time().unwrap().unwrap().now_ns().is_ok());
    }

    #[test]
    fn native_certificate_policy_clock_failure_never_falls_back_to_wall_time() {
        use tilo_action_broker::peer_actions::CertificateAuthority;

        let authority = NativeCertificates {
            origin: Err(tilo_action_broker::peer_actions::PolicyClockError),
        };
        assert!(authority.policy_time().is_err());
    }

    #[test]
    fn native_certificate_policy_refuses_another_services_retained_origin() {
        use tilo_action_broker::peer_actions::CertificateAuthority;

        let authority = NativeCertificates::default();
        let other = NativeCertificates::default();
        let foreign_time = other.policy_time().unwrap().unwrap();
        let root = ed25519_dalek::SigningKey::from_bytes(&[73; 32]);
        let device = ed25519_dalek::SigningKey::from_bytes(&[74; 32]);
        let certificate = tilo_joined_room::testing::certificate(&root, &device, "clock test");
        let room_id = uuid::Uuid::parse_str(tilo_joined_room::testing::ROOM).unwrap();
        assert_eq!(
            authority
                .authorize_with_policy_time(room_id, &certificate, &foreign_time)
                .unwrap_err(),
            "native certificate policy origin changed"
        );
        drop(other);
        assert!(foreign_time.now_ns().is_err());
        assert_eq!(
            authority
                .authorize_with_policy_time(room_id, &certificate, &foreign_time)
                .unwrap_err(),
            "native certificate policy origin changed"
        );
    }

    #[test]
    fn native_certificate_policy_time_never_replaces_an_unavailable_origin() {
        use tilo_action_broker::peer_actions::CertificateAuthority as _;
        let certificates = NativeCertificates {
            origin: Err(tilo_action_broker::peer_actions::PolicyClockError),
        };
        for _ in 0..2 {
            assert!(matches!(certificates.policy_time(), Err(error)
                if error == tilo_action_broker::peer_actions::PolicyClockError.to_string()));
        }
        let certificates = NativeCertificates::default();
        let captured = certificates.policy_time().unwrap().unwrap();
        certificates.origin.as_ref().unwrap().invalidate();
        assert!(certificates.policy_time().is_err());
        assert!(captured.now_ns().is_err());
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

    #[test]
    fn host_applications_are_launchable_unconfined_unless_a_package_answers_for_them() {
        let package = |id: &str| crate::shell::launcher::InstalledApp {
            package_id: id.into(),
            display_name: "Writer".into(),
            localized_names: std::collections::BTreeMap::default(),
            category: None,
            version: "1".into(),
            manifest_path: std::path::PathBuf::from("/x/manifest.toml"),
            payload_root: std::path::PathBuf::from("/x"),
            has_gui: true,
            opens_extensions: Vec::new(),
            opens_mime_types: Vec::new(),
            opens_uri_schemes: Vec::new(),
        };
        let host = |id: &str, name: &str| crate::shell::host_apps::HostApp {
            desktop_id: id.into(),
            name: name.into(),
            source_label: "System",
            categories: Vec::new(),
            keywords: Vec::new(),
            window_identities: vec![id.into()],
            mime_types: Vec::new(),
            terminal: false,
            icon: None,
        };
        let apps = launchable_applications(
            vec![package("so.tilo.word-processor")],
            vec![
                host("chromium-browser", "Chromium"),
                host("SO.TILO.WORD-PROCESSOR", "Shadowed"),
            ],
        );
        let listed: Vec<_> = apps
            .iter()
            .map(|app| (app.package_id.as_str(), app.unconfined))
            .collect();
        assert_eq!(
            listed,
            [
                ("so.tilo.word-processor", false),
                ("host:chromium-browser", true),
            ]
        );
    }

    #[test]
    fn a_host_application_refuses_launch_content_before_spawning() {
        let workspace = crate::shell::state::Workspace::new(
            crate::shell::state::WorkspaceId(3),
            "host launch test",
        );
        let refused = NativeLauncher(None, None).launch_content(
            "host:chromium-browser",
            &workspace,
            LaunchContent {
                hash: "00",
                caller: None,
                author: None,
            },
        );
        assert_eq!(
            refused,
            Err("opening launch content in a host application is unsupported".into())
        );
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
        let launcher = NativeLauncher(Some(handle.clone()), None);
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
        drop(launcher);
        assert_eq!(
            handle.read().desktop.workspaces[0].capabilities.len(),
            stored,
            "granted once"
        );
    }

    /// Clicking or otherwise opening an app on this machine is the person
    /// here granting authorship, once.
    #[test]
    fn the_person_here_authors_an_app_they_open() {
        use super::super::state::WorkspaceExt as _;
        use collab_types::caps::{ResourceRef, Verb};
        let mut tree = crate::shell::state::WorkspaceTree::default();
        let person = crate::shell::state::local_user_id();
        let mut workspace =
            crate::shell::state::Workspace::new(crate::shell::state::WorkspaceId(9), "person open");
        workspace.set_owner(person);
        let id = workspace.id;
        tree.desktop.workspaces.push(workspace);
        tree.desktop.active = id;
        let opened = tree
            .workspace_after_opening_app("so.tilo.word-processor")
            .expect("active workspace");
        let actor = opened.actor_for_app("so.tilo.word-processor");
        assert!(opened.allows(&actor, &ResourceRef::Workspace, Verb::Write));
        let again = tree
            .workspace_after_opening_app("so.tilo.word-processor")
            .expect("still active");
        assert_eq!(
            again.capabilities.len(),
            opened.capabilities.len(),
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
