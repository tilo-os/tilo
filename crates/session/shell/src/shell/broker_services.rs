//! Adapters from the shared broker to the native session's existing services.
use super::state::WorkspaceTreeHandle;
#[cfg(target_os = "linux")]
mod airdrop_transfer;
#[cfg(target_os = "linux")]
pub mod approved_checks;
mod committed_document;
#[cfg(target_os = "linux")]
mod file_clipboard;
mod host_membership;

#[cfg(test)]
#[cfg(target_os = "linux")]
pub(crate) use file_clipboard::test_support::TestTransferPause;
#[cfg(test)]
#[path = "broker_services/native_effect_tests.rs"]
mod native_effect_tests;
mod nearby_transfer;
mod owner_source;
mod person_chat;
#[cfg(test)]
#[path = "broker_services/room_status_tests.rs"]
mod room_status_tests;
#[cfg(test)]
pub(crate) use owner_source::OPENING as NATIVE_OWNER_OPENING;
mod resource_views;
mod room_nodes;
#[cfg(target_os = "linux")]
mod service_sessions;
mod tracker_runs;
pub use nearby_transfer::NativeNearbyTransfer;
use std::sync::Arc;
use tilo_action_broker::services::{
    ContinuationServices, HostServices, LaunchContent, LaunchPlace, LaunchScope,
    LaunchableApplication, Launched, LauncherServices, LegacyWorkspaceProjection,
    NativeDocumentRead, NativeDocumentReadContext, NativeDocumentReads, ShortcutLibraryRequest,
    ShortcutLibraryServices, WorkspaceAuthority, WorkspaceControlServices,
    WorkspaceGovernanceRequest, WorkspaceGovernanceServices,
};
pub(crate) use tracker_runs::{FollowedRun, NativeTrackerRuns};

/// The same state.json owner used by canonical enrollment and ingress. Network
/// availability never supplies or removes this durable admission boundary.
///
/// Built on the compositor's loop, so building it only starts the owner's
/// open ([`owner_source`]): synchronous questions answer from an owner
/// already open, commits wait for the open inside their futures.
pub(crate) struct NativeOwnerCommitService {
    tree: WorkspaceTreeHandle,
    owner: owner_source::OwnerSource,
    network: Option<Arc<super::network::ShellNetwork>>,
    history_keys: Option<tilo_action_broker::channel_encryption::ChannelKeyProvider>,
    /// The broker's admitted ops, where the sealed writes this device holds
    /// are kept: what a cold read of a sealed room's record opens.
    sealed_writes: Option<SealedWrites>,
    /// The channels a pending read of a sealed record waits on for keys.
    key_watches: Arc<std::sync::Mutex<std::collections::HashSet<collab_types::ChannelId>>>,
    writes: OwnerWrites,
}

/// The collaboration broker's admitted ops and durable history.
type SealedWrites = Arc<std::sync::Mutex<tilo_action_broker::collab_core::BrokerChannels>>;

/// Where the owner's commits run. A commit waits for the disk (opening the
/// owner, the state file's data and directory syncs), which on a loaded disk
/// takes seconds, and its future is awaited on the broker's single-threaded
/// loop, which answers every peer action, catalogue and liveness probe. So a
/// commit runs on a blocking thread instead, and the loop goes on answering
/// while it waits. Commits still run one at a time, in the order they were
/// asked for: the turn is held until the write is done, also when the future
/// awaiting it is dropped (a dropped wait never cancels a started commit).
#[derive(Clone, Default)]
struct OwnerWrites(Arc<tokio::sync::Mutex<()>>);
impl OwnerWrites {
    async fn run<T: Send + 'static>(
        self,
        write: impl FnOnce() -> Result<T, String> + Send + 'static,
    ) -> Result<T, String> {
        let turn = self.0.lock_owned().await;
        tokio::task::spawn_blocking(move || {
            let written = write();
            drop(turn);
            written
        })
        .await
        .map_err(|_| "native owner commit ended without an outcome".to_string())?
    }
}
impl NativeOwnerCommitService {
    pub(crate) fn new(
        tree: WorkspaceTreeHandle,
        network: Option<Arc<super::network::ShellNetwork>>,
    ) -> Self {
        Self {
            owner: owner_source::OwnerSource::new(&tree, network.as_ref()),
            tree,
            network,
            history_keys: None,
            sealed_writes: None,
            key_watches: Arc::default(),
            writes: OwnerWrites::default(),
        }
    }
    /// The owner, waiting for its open: for a worker thread, never the loop.
    #[cfg(test)]
    pub(crate) fn resolve_owner(
        &self,
    ) -> Result<super::native_workspace_owner::NativeWorkspaceOwner, String> {
        self.owner.resolve()
    }
    /// The owner if it is open, never waiting: a synchronous question while
    /// it opens hears so, and `unavailable` when there is none.
    fn opened_owner(
        &self,
        unavailable: &str,
    ) -> Result<super::native_workspace_owner::NativeWorkspaceOwner, String> {
        self.owner.opened(unavailable)
    }
}
impl tilo_peer_state::owner_commit::OwnerCommitService for NativeOwnerCommitService {
    fn requires_room(&self, room: &str) -> bool {
        super::state::RoomId::parse(room)
            .is_some_and(|room| self.tree.read().commits_native_documents(room))
    }
    /// A held room's effect receipts are this device's own to record in it,
    /// whichever model its records follow.
    fn records_effects(&self, room: &str) -> bool {
        super::state::RoomId::parse(room).is_some_and(|room| self.tree.read().projects_room(room))
    }
    fn health(&self) -> Result<(), String> {
        self.opened_owner("native durable owner unavailable")?
            .health()
    }
    fn commit(
        &self,
        request: tilo_peer_state::owner_commit::OwnerCommitRequest,
    ) -> tilo_peer_state::owner_commit::CommitFuture {
        let owner = self.owner.clone();
        let network = self.network.clone();
        let writes = self.writes.clone();
        Box::pin(writes.run(move || {
            let owner = owner
                .resolve()
                .map_err(|error| format!("native durable owner unavailable: {error}"))?;
            let room = super::state::RoomId::parse(&request.room).ok_or("invalid native room")?;
            let now = std::time::SystemTime::now();
            if !matches!(
                request.operation,
                tilo_peer_state::owner_commit::CommitOperation::EffectReceipt { .. }
            ) {
                if let Some(network) = network.as_ref() {
                    network.check_native_custody(&owner, room)?;
                }
                return owner.commit_document(request, now);
            }
            // A receipt is this device's own, recorded in its person's slice
            // of the room with its own key and published as its own change.
            let key = match network.as_ref() {
                Some(network) => network.native_signing_key_of(&owner, room)?,
                None => {
                    tilo_identity_host::broker::trusted_current_device(&request.room)
                        .map_err(|error| format!("this device's key is unavailable: {error}"))?
                        .0
                }
            };
            let receipt = owner.commit_effect_receipt(&request, &key, now)?;
            if let Some(network) = network {
                network.schedule_native_owner_publication(room);
            }
            Ok(receipt)
        }))
    }
}

impl NativeOwnerCommitService {
    /// The rooms' status, as this service's tree and network tell it.
    fn rooms(&self) -> RoomStatusSource {
        RoomStatusSource::new(self.tree.clone(), self.network.clone())
    }
}

impl tilo_action_broker::services::WorkspaceMemberships for NativeOwnerCommitService {
    fn checkpoint_result_access(
        &self,
        room: &str,
        caller: &tilo_user_identity::DeviceCert,
        id: uuid::Uuid,
        mutation: bool,
    ) -> Result<(), String> {
        let owner = &self.opened_owner("native checkpoint governor unavailable")?;
        owner.health()?;
        let room_id = super::state::RoomId::parse(room).ok_or("invalid checkpoint room")?;
        if let Some(network) = self.network.as_ref() {
            network.check_native_custody(owner, room_id)?;
            if network.governed_elsewhere(room_id) {
                return Err("checkpoint requires the governing owner".into());
            }
        }
        owner.with_current(room_id, |view| {
            if !view.hosted_here() {
                return Err("native checkpoint requires the actual governing owner".into());
            }
            Ok(())
        })?;
        let (key, governor) = tilo_identity_host::broker::trusted_current_device(room)
            .map_err(|error| format!("checkpoint governor custody unavailable: {error}"))?;
        let now = std::time::SystemTime::now();
        owner.with_current(room_id, |view| {
            if !view.hosted_here() || view.room_host() != &governor {
                return Err("checkpoint governor custody changed".into());
            }
            view.peer().workspace().check_checkpoint_writer(&key, now)?;
            if mutation {
                view.peer()
                    .workspace()
                    .check_checkpoint_namer(caller, now)?;
            }
            Ok(())
        })?;
        owner.read_checkpoint(room_id, caller, id, now)?;
        Ok(())
    }

    fn checkpoint(
        &self,
        request: tilo_peer_state::workspace_checkpoint::CheckpointRequest,
    ) -> tilo_peer_state::workspace_checkpoint::CheckpointFuture {
        let owner = self.owner.clone();
        let network = self.network.clone();
        Box::pin(async move {
            let owner = owner
                .resolve()
                .map_err(|error| format!("native checkpoint governor unavailable: {error}"))?;
            owner.health()?;
            let room =
                super::state::RoomId::parse(&request.room).ok_or("invalid checkpoint room")?;
            if let Some(network) = network.as_ref() {
                network.check_native_custody(&owner, room)?;
                if network.governed_elsewhere(room) {
                    return Err("native checkpoint requires the actual governing owner".into());
                }
            }
            owner.with_current(room, |view| {
                if !view.hosted_here() {
                    return Err("native checkpoint requires the actual governing owner".into());
                }
                Ok(())
            })?;
            let (key, governor) = tilo_identity_host::broker::trusted_current_device(&request.room)
                .map_err(|error| format!("checkpoint governor custody unavailable: {error}"))?;
            owner.with_current(room, |view| {
                if view.room_host() != &governor {
                    return Err("checkpoint governor custody changed".into());
                }
                Ok(())
            })?;
            let result = owner.execute_checkpoint(&request, &key, std::time::SystemTime::now())?;
            if request.command.mutates()
                && let Some(network) = network
            {
                network.schedule_native_owner_publication(room);
            }
            Ok(result)
        })
    }

    fn room_status(&self, room: &str) -> runtime_collab::RoomStatus {
        super::state::RoomId::parse(room).map_or_else(runtime_collab::RoomStatus::default, |room| {
            self.rooms().status(room)
        })
    }

    fn room_status_change(
        &self,
        room: &str,
        seen: runtime_collab::RoomStatus,
    ) -> tilo_action_broker::services::RoomStatusFuture {
        match super::state::RoomId::parse(room) {
            Some(room) => Box::pin(self.rooms().change(room, seen)),
            None => Box::pin(std::future::ready(runtime_collab::RoomStatus::default())),
        }
    }
    fn requires_room(&self, room: &str) -> bool {
        tilo_peer_state::owner_commit::OwnerCommitService::requires_room(self, room)
    }

    /// The one app-access decision every opening of an app takes
    /// ([`super::launcher::native_app_open_access`]): the room does not let
    /// `app` author there yet, and `certificate`'s person may grant it.
    fn app_opens_by_granting(
        &self,
        room: &str,
        app: &str,
        certificate: &tilo_user_identity::DeviceCert,
    ) -> bool {
        let Some(room) = super::state::RoomId::parse(room) else {
            return false;
        };
        if !self.tree.read().holds_room(room) {
            return false;
        }
        self.opened_owner("native membership owner unavailable")
            .and_then(|owner| {
                super::launcher::native_app_open_access(&owner, room, certificate, app)
                    .map_err(|error| error.to_string())
            })
            .is_ok_and(|access| access == super::launcher::NativeAppOpenAccess::Delegate)
    }

    /// A room this device holds, joins or has placed on its host: its
    /// membership actions are published once its replica is projected
    /// ([`super::state::WorkspaceTree::project_native_room`]), which a
    /// commit installing the room may precede.
    fn membership_pending(&self, room: &str) -> bool {
        super::state::RoomId::parse(room).is_some_and(|room| {
            let tree = self.tree.read();
            tree.holds_room(room) || tree.documents_unavailable(room).is_some()
        })
    }

    /// A held room whose admitted governor is this device: its checkpoints
    /// are this device's custody, sealed records or not.
    fn keeps_checkpoints(&self, room: &str) -> bool {
        let Some(room) = super::state::RoomId::parse(room) else {
            return false;
        };
        if !self.tree.read().projects_room(room) {
            return false;
        }
        self.opened_owner("native checkpoint governor unavailable")
            .and_then(|owner| owner.with_current(room, |view| Ok(view.hosted_here())))
            .unwrap_or(false)
    }

    /// A room this device's own host serves: what an app opening there needs
    /// is for this device's owner to record.
    fn hosted_here(&self, room: &str) -> bool {
        super::state::RoomId::parse(room).is_some_and(|room| {
            self.network
                .as_ref()
                .is_some_and(|net| net.hosted_here(room))
        })
    }

    /// A room whose channels its node takes part in: this device writes its
    /// membership changes there itself, as in any room it holds.
    fn grants_authorship_at_host(&self, room: &str) -> bool {
        super::state::RoomId::parse(room).is_some_and(|room| self.tree.read().seals_records(room))
    }

    fn health(&self) -> Result<(), String> {
        self.opened_owner("native membership owner unavailable")?
            .health()
    }

    /// A membership change this device writes itself, signed with its own
    /// key: the room's admission decides it against the caller's grants,
    /// and its host admits it as it admits every member's change. In a room
    /// this device's own host governs, that host commits it
    /// ([`host_membership::commit_at_own_host`]).
    fn execute(
        &self,
        request: tilo_peer_state::owner_commit::OwnerCommitRequest,
    ) -> tilo_peer_state::owner_commit::CommitFuture {
        let owner = self.owner.clone();
        let network = self.network.clone();
        Box::pin(async move {
            let owner = owner
                .resolve()
                .map_err(|error| format!("native membership owner unavailable: {error}"))?;
            let room = super::state::RoomId::parse(&request.room)
                .ok_or("invalid native membership room")?;
            if let Some(network) = network.as_ref() {
                network.check_native_custody(&owner, room)?;
                if let tilo_peer_state::owner_commit::CommitOperation::WorkspaceMembershipCommand {
                    caller,
                    command,
                    ..
                } = &request.operation
                    && let Some(receipt) = self::host_membership::commit_at_own_host(
                        network, &owner, room, &request, caller, command,
                    )
                    .await
                {
                    return receipt;
                }
            }
            let key = match network.as_ref() {
                Some(network) => network.native_signing_key_of(&owner, room)?,
                None => {
                    tilo_identity_host::broker::trusted_current_device(&request.room)
                        .map_err(|error| format!("this device's key is unavailable: {error}"))?
                        .0
                }
            };
            let receipt = owner.commit_workspace_membership_command(
                request,
                &key,
                std::time::SystemTime::now(),
            )?;
            if let Some(network) = network {
                network.schedule_native_owner_publication(room);
            }
            Ok(receipt)
        })
    }
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
    /// The room's agents whose delegation the room's governance verified for
    /// exactly their device ([`super::state::WorkspaceTree::verified_agents`]).
    fn history_agents(&self, room: &str) -> Vec<collab_types::history::RosterAgent> {
        super::state::RoomId::parse(room)
            .map(|room| self.0.read().verified_agents(room))
            .unwrap_or_default()
    }
    fn room_revocations(&self, room: &str) -> Option<tilo_user_identity::RevocationList> {
        super::state::RoomId::parse(room).and_then(|room| self.0.read().room_revocations(room))
    }
}
impl LegacyWorkspaceProjection for NativeWorkspaceAuthority {
    /// Every room's effects are committed by its native owner, never
    /// recorded into the tree's projection.
    fn record_effect(
        &self,
        _room: &str,
        _invocation: tilo_actions::InvocationId,
        _actor: &str,
        _context: tilo_actions::ActionContext,
        _status: tilo_actions::EffectReceiptStatus,
    ) -> Result<(), String> {
        Err("native enrolled mutation requires its typed durable owner".into())
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

    let mut session = SESSION_BLOBS.lock_recover();
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
        *SESSION_BLOBS.lock_recover() = None;
    }
    result
}

/// How long the filesystem names an agent as the cause of the writes to the
/// paths it was told: long enough for an action's writes to land, short
/// enough that a later write of the person's own stays theirs.
const WRITE_ATTRIBUTION_TTL: std::time::Duration = std::time::Duration::from_secs(60);

/// Name `agent` (`agent:<id>`, which the broker took from its custody of a
/// live invocation and which acts for this session's person) as the cause of
/// this device's next writes to `paths`: each version the filesystem then
/// makes of them records the agent, for the person who signs it, in the
/// agent's presence colour. Over the session-only document media socket, which
/// no sandbox reaches. It grants nothing.
///
/// # Errors
/// Returns why the filesystem could not be reached or refused.
pub(super) fn attribute_session_writes(agent: &str, paths: Vec<String>) -> Result<(), String> {
    let label = ui::history_source::agent_principal_name(agent)
        .ok_or("only an agent is named as the cause of a write")?;
    let (red, green, blue) = ui::presence::user_color(agent);
    with_session_blobs(|client| {
        client.attribute_writes(runtime_fs::blobs::WriteAttribution {
            paths,
            actor: agent.to_owned(),
            label,
            color: [red, green, blue],
            ttl: WRITE_ATTRIBUTION_TTL,
        })
    })
    .map_err(|error| error.to_string())
}

/// The filesystem daemon's content store, as content puts write to it:
/// each room's under the neutral [`CONTENT_PUT_SCOPE`], which every app of
/// the room reads by reference.
///
/// [`CONTENT_PUT_SCOPE`]: tilo_action_broker::services::CONTENT_PUT_SCOPE
struct SessionContentPuts;

impl tilo_action_broker::services::ContentStoreServices for SessionContentPuts {
    fn max_bytes(&self) -> u64 {
        runtime_fs::blobs::MAX_BLOB_BYTES
    }

    fn begin(
        &self,
        room: &str,
    ) -> Result<Box<dyn tilo_action_broker::services::ContentSink>, String> {
        with_session_blobs(|client| {
            client
                .scope(room, tilo_action_broker::services::CONTENT_PUT_SCOPE)?
                .begin_upload("application/octet-stream")
        })
        .map(|upload| Box::new(SessionContentSink(upload)) as Box<_>)
        .map_err(|error| error.to_string())
    }

    /// Whether the room still references `hash` as content it holds. The
    /// daemon answers for every app of the room at once (`has` through the
    /// room's neutral scope), and a store that cannot be reached keeps the
    /// attribution rather than dropping a put it cannot check.
    fn holds(&self, room: &str, hash: &str) -> bool {
        with_session_blobs(|client| {
            let scope = client.scope(room, tilo_action_broker::services::CONTENT_PUT_SCOPE)?;
            scope.has(hash)
        })
        .unwrap_or(true)
    }
}

/// One upload into the daemon's store; dropped unfinished, it is aborted.
struct SessionContentSink(runtime_fs::blobs::BlobUpload);

impl tilo_action_broker::services::ContentSink for SessionContentSink {
    fn append(&mut self, bytes: &[u8]) -> Result<(), String> {
        self.0.append(bytes).map_err(|error| error.to_string())
    }

    fn finish(self: Box<Self>) -> Result<(String, u64), String> {
        let stored = self.0.finish().map_err(|error| error.to_string())?;
        Ok((stored.hash, stored.len))
    }
}

/// Decide every change this session's rooms receive by its claim against
/// the app surface its op names: the one the installed app ships, or one
/// read from the session's content store in the record's room and app; name,
/// in the changes the session signs for an app it hosts, the surface that
/// app was started with.
pub fn decide_receipt_against_surfaces() {
    use runtime_fs::blobs::ReadOutcome;
    tilo_action_broker::decide_receipt_against_surfaces(|record, hash| {
        if let Some(bytes) = crate::shell::app_versions::installed_surface(record.app, hash) {
            return Some(bytes);
        }
        match with_session_blobs(|client| {
            client
                .scope(record.room, record.app)
                .and_then(|scoped| scoped.read_all(hash))
        }) {
            Ok(ReadOutcome::Ready { bytes, .. }) => Some(bytes),
            // Still arriving, or nowhere: the op waits.
            Ok(ReadOutcome::Pending | ReadOutcome::Unavailable) | Err(_) => None,
        }
    });
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
    stage_content_bytes(app_id, content_hash, &bytes, workspace).map(Some)
}

/// The workspace content `hash` of `room`: what content puts, agents' and
/// apps', keep under the room's neutral scope, which every app of the room
/// reads by reference.
///
/// # Errors
///
/// Why it cannot be read here: still arriving, nowhere, or the store is
/// unreachable.
pub(crate) fn read_room_content(room: &str, hash: &str) -> Result<Vec<u8>, String> {
    use runtime_fs::blobs::ReadOutcome;
    let result = with_session_blobs(|client| {
        client
            .scope(room, tilo_action_broker::services::CONTENT_PUT_SCOPE)
            .and_then(|scoped| scoped.read_all(hash))
    });
    match result {
        Ok(ReadOutcome::Ready { bytes, .. }) => Ok(bytes),
        Ok(ReadOutcome::Pending) => Err(format!(
            "tilo.content:{hash} is still arriving on this device"
        )),
        Ok(ReadOutcome::Unavailable) => Err(format!(
            "tilo.content:{hash} is not in this workspace's content"
        )),
        Err(error) => Err(format!("the workspace content store: {error}")),
    }
}

/// Stage `bytes`, whose hash is `content_hash`, as a read-only file granted
/// to `app_id` in `workspace`; its path.
///
/// # Errors
///
/// The bytes do not match the hash or are too large, or staging failed.
pub(crate) fn stage_content_bytes(
    app_id: &str,
    content_hash: &str,
    bytes: &[u8],
    workspace: &mut super::state::Workspace,
) -> Result<std::path::PathBuf, String> {
    if bytes.is_empty()
        || bytes.len() > MAX_LAUNCH_CONTENT_BYTES
        || !tilo_content::verify(content_hash, bytes)
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
    // Content travels without the name its sender gave it: its bytes say
    // what it is, and so what the opened app takes it as.
    let (extension, _) = tilo_content::opened_as(bytes);
    let path = dir.join(format!("{content_hash}.{extension}"));
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
    {
        Ok(mut file) => {
            use std::io::Write;
            file.write_all(bytes)
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
    Ok(path)
}

/// Launches installed apps, in every room on this device itself: with the
/// session's workspace tree it places their windows.
pub struct NativeLauncher(pub Option<WorkspaceTreeHandle>);

impl NativeLauncher {
    /// Note where `app`'s window lands when it arrives: at `place` in
    /// `workspace`, decided now against the desk as it stands.
    fn place(&self, app: &str, workspace: &tilo_desktop_model::Workspace, place: &LaunchPlace) {
        let Some(tree) = &self.0 else { return };
        let placement = super::placement::placement_at(&tree.read(), workspace.room(), place);
        if let Some(placement) = placement {
            super::launcher::set_pending_placement(app, placement);
        }
    }

    /// [`Self::place`] for host application `desktop_id`, whose window
    /// announces an app id Tilo did not mint: the note is keyed on every
    /// identity its entry declares. Without it the window would land where
    /// a person's own launch does, taking their focus for an agent.
    fn place_host_application(
        &self,
        desktop_id: &str,
        workspace: &tilo_desktop_model::Workspace,
        place: &LaunchPlace,
    ) {
        let Some(tree) = &self.0 else { return };
        let Some(placement) = super::placement::placement_at(&tree.read(), workspace.room(), place)
        else {
            return;
        };
        if let Some(app) = super::host_apps::find(desktop_id) {
            super::host_apps::note_placement(&app, placement);
        }
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
            capabilities: Vec::new(),
            package_id: format!("{HOST_APPLICATION_PREFIX}{}", app.desktop_id),
            display_name: app.name,
            unconfined: true,
            offers: Vec::new(),
        });
    packages
        .into_iter()
        .map(|app| LaunchableApplication {
            // Only the capabilities a grant is decided on, and only for the
            // apps the image could grant one to: the manifest is read for
            // those alone.
            capabilities: if settings_store::app_scope::SYSTEM_SETTINGS_APPS
                .contains(&app.package_id.as_str())
            {
                app.declared_capabilities()
                    .into_iter()
                    .filter(|capability| {
                        capability == settings_store::app_scope::SYSTEM_SETTINGS_CAPABILITY
                    })
                    .collect()
            } else {
                Vec::new()
            },
            package_id: app.package_id,
            display_name: app.display_name,
            unconfined: false,
            offers: app.offers,
        })
        .chain(hosts)
        .collect()
}

/// Native package discovery needs neither host-entry conversion/deduplication
/// nor other packages' capability manifests. Host IDs retain the full merge so
/// an installed Tilo package still shadows its corresponding desktop entry.
fn launchable_applications_for(
    app_id: &str,
    packages: Vec<super::launcher::InstalledApp>,
    hosts: impl FnOnce() -> Vec<super::host_apps::HostApp>,
) -> Vec<LaunchableApplication> {
    if app_id.starts_with(HOST_APPLICATION_PREFIX) {
        return launchable_applications(packages, hosts())
            .into_iter()
            .filter(|app| app.package_id == app_id)
            .collect();
    }
    let packages = packages
        .into_iter()
        .filter(|app| app.package_id == app_id)
        .collect();
    launchable_applications(packages, Vec::new())
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
    super::host_apps::launch(desktop_id, Some(workspace.room().to_string()))
        .map(drop)
        .map_err(|e| e.to_string())
}

impl LauncherServices for NativeLauncher {
    fn begin_executing_binding_observation(&self, launcher_pid: u32) -> Option<u64> {
        super::launcher::LaunchSession::current()
            .launch_history
            .lock_recover()
            .binding(
                launcher_pid,
                super::launcher::launch_history::Phase::BindingStarted,
            )
    }
    fn observe_executing_binding(
        &self,
        observation: u64,
        phase: tilo_action_broker::services::ExecutingBindingPhase,
    ) {
        use super::launcher::launch_history::Phase;
        use tilo_action_broker::services::ExecutingBindingPhase as Binding;
        let phase = match phase {
            Binding::Started => Phase::BindingStarted,
            Binding::Challenge => Phase::BindingChallenge,
            Binding::Answer => Phase::BindingAnswer,
            Binding::Admitted => Phase::BindingAdmitted,
            Binding::Refused => Phase::BindingRefused,
            Binding::Abandoned => Phase::BindingAbandoned,
        };
        super::launcher::LaunchSession::current()
            .launch_history
            .lock_recover()
            .event(observation, phase);
    }
    fn applications(&self) -> Vec<LaunchableApplication> {
        launchable_applications(super::launcher::gui_apps(), super::host_apps::host_apps())
    }
    fn earlier_ids(&self) -> tilo_action_policy::EarlierIds {
        super::launcher::installed_earlier_ids()
    }
    fn applications_for(&self, app_id: &str) -> Vec<LaunchableApplication> {
        launchable_applications_for(
            app_id,
            super::launcher::gui_apps(),
            super::host_apps::host_apps,
        )
    }
    fn person_initiated(&self, app_id: &str) -> bool {
        // A person's input reaches an app through the Linux compositor's
        // windows; elsewhere no window of the app took it.
        #[cfg(target_os = "linux")]
        {
            self.0.as_ref().is_some_and(|tree| {
                crate::compositor::activation::person_initiated(&tree.read(), app_id)
            })
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = app_id;
            false
        }
    }
    fn launch(
        &self,
        app: &str,
        workspace: &tilo_desktop_model::Workspace,
        record: Option<&str>,
        place: &LaunchPlace,
    ) -> Result<(), String> {
        if let Some(desktop_id) = app.strip_prefix(HOST_APPLICATION_PREFIX) {
            self.place_host_application(desktop_id, workspace, place);
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
        self.place(app, workspace, place);
        // Opening an app yourself makes it an author: this device writes
        // that through the native owner ([`NativeWorkspaceMembership`]), and
        // the app opens as a viewer until the authorship arrives.
        let launched = match record {
            Some(record) => {
                let record = ui::record::RecordId::parse(record)
                    .ok_or_else(|| format!("{record} is not a record of {app}"))?;
                super::launcher::launch_uri_in_room(
                    app,
                    workspace.room(),
                    workspace.launch_grants(tilo_clock::unix_now()),
                    &record.uri(),
                )
            }
            None => super::launcher::launch_in_workspace(app, workspace),
        };
        launched.map(|_| ()).map_err(|e| e.to_string())
    }
    fn windowless(
        &self,
        app: &str,
        room: &str,
    ) -> Result<Option<tilo_action_broker::services::WindowlessStart>, String> {
        Ok(super::windowless::request(app, room))
    }
    fn begin_launch(
        &self,
        app: &str,
        package_root: Option<&str>,
        workspace: &tilo_desktop_model::Workspace,
        _author: Option<&str>,
    ) -> Result<tilo_action_broker::services::LaunchReadiness, String> {
        if let Some(desktop_id) = app.strip_prefix(HOST_APPLICATION_PREFIX) {
            if package_root.is_some() {
                return Err("a host application cannot name a Tilo package root".into());
            }
            return super::launch_readiness::observe(|| {
                super::host_apps::launch(desktop_id, Some(workspace.room().to_string()))
                    .map(drop)
                    .map_err(|error| error.to_string())
            });
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
        super::launch_readiness::observe(|| {
            super::launcher::launch_resolved_in_workspace(selected, workspace)
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
                workspace.room(),
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
        place: &LaunchPlace,
    ) -> Result<Launched, String> {
        if let Some(desktop_id) = app.strip_prefix(HOST_APPLICATION_PREFIX) {
            return launch_host_application(desktop_id, workspace, content)
                .map(|()| Launched::Unconfirmed);
        }
        let mut workspace = workspace.clone();
        let document = stage_launch_content(app, content, &mut workspace, &SessionContentStore)?;
        self.place(app, &workspace, place);
        // The process it starts keeps the attempt's token in its launch
        // record, and presents it when its connection is authenticated.
        super::launch_readiness::identified(|| {
            super::launcher::launch_document_in_workspace(app, &workspace, document.as_deref())
                .map(drop)
                .map_err(|e| e.to_string())
        })
        .map(|ticket| Launched::Started { ticket })
    }
    fn authenticated_launch(
        &self,
        pid: u32,
        app: &str,
        room: &str,
        window_instance: Option<u64>,
    ) -> Result<Option<tilo_action_broker::services::AuthenticatedApplicationLaunch>, String> {
        Ok(super::launcher::authenticated_window_launch(
            pid,
            app,
            room,
            window_instance,
        ))
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
        let Some((_, start)) = super::host_runtime::proc_stat(launcher_pid) else {
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
            room: workspace.room(),
        })
    }
}

/// Where each room stands for the apps on this device: what a join into one
/// waits on, and a launch into one ([`super::launcher`]).
#[derive(Clone)]
pub(crate) struct RoomStatusSource {
    tree: WorkspaceTreeHandle,
    network: Option<Arc<super::network::ShellNetwork>>,
}

impl RoomStatusSource {
    pub(crate) const fn new(
        tree: WorkspaceTreeHandle,
        network: Option<Arc<super::network::ShellNetwork>>,
    ) -> Self {
        Self { tree, network }
    }

    /// Whether `room` is being placed on its host, rather than joined.
    pub(crate) fn placing(&self, room: super::state::RoomId) -> bool {
        self.tree.read().pending_placements.contains(&room)
    }

    /// Where `room` stands now.
    pub(crate) fn status(&self, room: super::state::RoomId) -> runtime_collab::RoomStatus {
        room_status(&self.tree, self.network.as_deref(), room)
    }

    /// `room`'s status once it is no longer `seen`, woken by the change
    /// itself ([`super::state::RoomReadiness`]), never by a clock.
    pub(crate) fn change(
        &self,
        room: super::state::RoomId,
        seen: runtime_collab::RoomStatus,
    ) -> impl std::future::Future<Output = runtime_collab::RoomStatus> + Send + 'static {
        let source = self.clone();
        async move {
            let mut changes = source.tree.room_readiness().subscribe();
            loop {
                let now = source.status(room);
                if now != seen {
                    return now;
                }
                // Subscribed before looking: a change after the look wakes
                // this. With the tree gone, nothing changes any more.
                if changes.changed().await.is_err() {
                    return now;
                }
            }
        }
    }
}

/// Where `room` stands for the apps on this device. A join that failed or
/// needs the person is refused; a room still being joined or placed on its
/// host waits, saying what last held its placement up.
fn room_status(
    tree: &WorkspaceTreeHandle,
    network: Option<&super::network::ShellNetwork>,
    room: super::state::RoomId,
) -> runtime_collab::RoomStatus {
    let (placing, unavailable) = {
        let tree = tree.read();
        (
            tree.pending_placements.contains(&room),
            tree.documents_unavailable(room),
        )
    };
    let Some(reason) = unavailable else {
        return runtime_collab::RoomStatus::Ready;
    };
    // A placement that failed is placed again once what it depends on
    // changes: it waits. A join that failed waits for the person.
    if !placing && let Some(refusal) = network.and_then(|network| network.join_refusal(room)) {
        return runtime_collab::RoomStatus::Refused(refusal);
    }
    runtime_collab::RoomStatus::Waiting {
        reason: super::i18n::t(reason),
        detail: network.and_then(|network| network.placement_failure(room)),
    }
}

/// The workspace's shortcuts, saved in its room document and run on the
/// session runtime. The tree only mirrors what the room's governor
/// committed: a save is this device's signed write of the room, through the
/// room's governor, under the rule admission holds every writer to.
pub struct NativeShortcutLibrary(
    pub WorkspaceTreeHandle,
    pub Option<Arc<super::network::ShellNetwork>>,
);
impl NativeShortcutLibrary {
    /// `change` written into `room` by this device, signed with its own key.
    /// A saved shortcut carries this device's proof that its person,
    /// `actor`, saved it. Runs on the session runtime; the owner's durable
    /// publication on a blocking thread.
    pub(crate) async fn commit(
        tree: WorkspaceTreeHandle,
        network: Option<Arc<super::network::ShellNetwork>>,
        room: super::state::RoomId,
        actor: String,
        change: tilo_action_broker::automation::ShortcutChange,
    ) -> Result<(), String> {
        use tilo_action_broker::automation::ShortcutChange;
        use tilo_network_actor::room_governance::trace;
        trace::note(|| format!("shortcut commit in {room} by {actor}"));
        let unavailable = tree.read().documents_unavailable(room);
        if let Some(reason) = unavailable {
            return Err(super::i18n::t(reason));
        }
        let owner = super::native_workspace_owner::attached(&tree)
            .ok_or("the workspace is no longer hosted here")?;
        let (key, device) = match &network {
            Some(network) => network.room_signer(room)?,
            None => tilo_identity_host::broker::trusted_current_device(&room.to_string())
                .map_err(|error| format!("this device cannot sign in the workspace: {error}"))?,
        };
        let change = match change {
            ShortcutChange::Save(mut shortcut) => {
                if shortcut.saved_by.as_deref() != Some(actor.as_str())
                    || actor != hex::encode(device.user_public_key)
                {
                    return Err("a shortcut is saved from its saver's own device".into());
                }
                tilo_peer_state::room::sign_saved_shortcut(
                    &room.to_string(),
                    &mut shortcut,
                    &device,
                    &key,
                )?;
                ShortcutChange::Save(shortcut)
            }
            delete @ ShortcutChange::Delete(_) => delete,
        };
        if let Some(network) = &network {
            network.check_native_custody(&owner, room)?;
        }
        let request = tilo_peer_state::owner_commit::OwnerCommitRequest {
            room: room.to_string(),
            operation: tilo_peer_state::owner_commit::CommitOperation::ShortcutChange {
                invocation: uuid::Uuid::new_v4(),
                caller: device.clone(),
                change: serde_json::to_vec(&change).map_err(|error| error.to_string())?,
            },
        };
        tokio::task::spawn_blocking(move || {
            owner.commit_shortcut_change(&request, &key, std::time::SystemTime::now())
        })
        .await
        .map_err(|_| "the shortcut's commit stopped".to_owned())??;
        if let Some(network) = &network {
            network.schedule_native_owner_publication(room);
        }
        Ok(())
    }
}
impl ShortcutLibraryServices for NativeShortcutLibrary {
    fn keymap(&self) -> settings_store::hardware::ShortcutConfig {
        settings_store::hardware::ShortcutConfig::load()
    }
    fn store(
        &self,
        request: ShortcutLibraryRequest,
    ) -> tilo_action_broker::services::ShortcutStore {
        let tree = self.0.clone();
        let network = self.1.clone();
        Box::pin(async move {
            let room =
                super::state::RoomId::parse(&request.room).ok_or("invalid workspace room")?;
            let committed = Self::commit(
                tree.clone(),
                network,
                room,
                request.actor.clone(),
                request.change,
            )
            .await;
            tilo_network_actor::room_governance::trace::note(|| {
                format!(
                    "shortcut store {:?} in {room}: {committed:?}",
                    request.invocation
                )
            });
            committed?;
            let mut tree = tree.write();
            if let Some(workspace_id) = tree
                .desktop
                .workspaces
                .iter()
                .find(|workspace| workspace.room() == room)
                .map(|workspace| workspace.id)
            {
                let _ = tree.append_workspace_effect_receipt(
                    workspace_id,
                    request.invocation,
                    &request.actor,
                    request.context,
                    tilo_actions::EffectReceiptStatus::Succeeded,
                );
            }
            drop(tree);
            Ok(())
        })
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
        settings_sections::APP_ID
    }
    fn is_resource(&self, resource: &str) -> bool {
        super::keyboard_shortcuts::is_resource(resource)
    }
    fn catalogue_rows(
        &self,
        provider: &str,
        _: &settings_store::hardware::ShortcutConfig,
    ) -> Vec<tilo_actions::remote::CatalogueRow> {
        super::keyboard_shortcuts::catalogue_rows(provider)
    }
    fn descriptor(
        &self,
        _: &settings_store::hardware::ShortcutConfig,
        address: &tilo_actions::ActionAddress,
    ) -> Option<tilo_actions::ActionDescriptor> {
        super::keyboard_shortcuts::descriptor_at(address)
    }
    fn apply(
        &self,
        keymap: &settings_store::hardware::ShortcutConfig,
        address: &tilo_actions::ActionAddress,
        argument: Option<&str>,
    ) -> Result<tilo_actions::ActionOutput, Box<tilo_actions::ActionFailure>> {
        super::keyboard_shortcuts::apply(keymap, address, argument)
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
    /// The network that asks `room`'s governor, its host, and the room.
    /// The native owner locks the tree under its own mutex, so this must
    /// not be asked while the tree is held.
    fn governor_of(
        &self,
        room: &str,
    ) -> Option<(&Arc<super::network::ShellNetwork>, super::state::RoomId)> {
        let room = self
            .0
            .read()
            .desktop
            .workspaces
            .iter()
            .find(|workspace| workspace.room().to_string() == room)
            .map(crate::shell::state::Workspace::room)?;
        self.1.as_ref().map(|network| (network, room))
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
        let room = workspace.room();
        tree.desktop
            .workspaces
            .iter()
            .find(|workspace| workspace.room() == room)
            .map(|workspace| {
                tilo_actions::door::survey(&[
                    tilo_desktop::workspace_settings::governance_resource_with_access(
                        workspace, true,
                    ),
                ])
            })
            .unwrap_or_default()
    }
    fn invoke(&self, request: &WorkspaceGovernanceRequest<'_>) -> Result<(), String> {
        let actor = request.actor;
        // Governing a room is a person's own act: an agent, an app or a
        // device that is not theirs never governs it.
        let person = actor
            .direct_person()
            .ok_or_else(|| "only a person governs a workspace".to_string())?
            .to_owned();
        let governor = self.governor_of(request.room);
        let mut tree = self.0.write();
        let workspace_index = tree
            .desktop
            .workspaces
            .iter()
            .position(|workspace| workspace.room().to_string() == request.room)
            .ok_or_else(|| "the application workspace is no longer hosted".to_string())?;
        let workspace = tree
            .desktop
            .workspaces
            .get(workspace_index)
            .ok_or_else(|| "the application workspace is no longer hosted".to_string())?;
        let surface =
            tilo_desktop::workspace_settings::governance_resource_with_access(workspace, true);
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
        let operation = decode_governance_operation(spec, request.argument)?;
        if !workspace.may_govern_at(&person, operation.action_id(), tilo_clock::unix_now()) {
            return Err("the caller is not allowed to govern this workspace".into());
        }
        if let Some((network, room)) = governor.filter(|(_, room)| *room == workspace.room()) {
            drop(tree);
            return write_governance_here(network, room, operation);
        }
        let workspace_id = workspace.id;
        super::workspace_governance::apply_governance_operation(
            &mut tree,
            workspace_id,
            &person,
            operation,
        )?;
        let context = tilo_actions::ActionContext::for_resource(
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

/// Write a governance `operation` made at this device into `room`, and wait
/// for the room's admission.
fn write_governance_here(
    network: &super::network::ShellNetwork,
    room: super::state::RoomId,
    operation: tilo_desktop::workspace_settings::GovernanceOperation,
) -> Result<(), String> {
    let request = super::workspace_governance::governance_request(room, operation)?;
    network.write_governance(room, request)
}

/// Find `id` in `resource`'s subtree.
fn find_resource<'a>(
    resource: &'a tilo_actions::ResourceSpec,
    id: &str,
) -> Option<&'a tilo_actions::ResourceSpec> {
    if resource.id.as_str() == id {
        return Some(resource);
    }
    resource
        .children
        .iter()
        .find_map(|child| find_resource(child, id))
}

/// Decode a workspace governance invocation's typed `argument` into the
/// operation `spec` encodes.
fn decode_governance_operation(
    spec: &tilo_actions::ActionSpec,
    argument: Option<&str>,
) -> Result<tilo_desktop::workspace_settings::GovernanceOperation, String> {
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
    if encoded.type_id != tilo_desktop::workspace_settings::GOVERNANCE_ACTION_TYPE {
        return Err("workspace governance action encoded an unexpected operation".into());
    }
    serde_json::from_slice(&encoded.payload)
        .map_err(|error| format!("invalid workspace governance operation: {error}"))
}

/// Grants people give applications for other apps' actions, recorded in the
/// room's own records at its governor, the room's host, as given by the
/// person who gave them, and noted in the room's history. A grant is in
/// force once the room as this device holds it carries it: the call it was
/// given for waits until then ([`super::network::ShellNetwork::put_grant_in_force`]).
pub struct NativeAppActionGrants {
    tree: WorkspaceTreeHandle,
    /// The way to the rooms' hosts; without one nothing is granted.
    network: Option<Arc<super::network::ShellNetwork>>,
}

impl NativeAppActionGrants {
    pub fn new(
        tree: WorkspaceTreeHandle,
        network: Option<Arc<super::network::ShellNetwork>>,
    ) -> Self {
        Self { tree, network }
    }

    /// The receipt noting `answer` to `request` in the room's history,
    /// as `request.person` gave it.
    fn receipt(
        request: &tilo_action_broker::app_actions::AppActionConsentRequest,
        answer: tilo_action_broker::app_actions::AppActionConsent,
    ) -> tilo_desktop_model::WorkspaceEffectReceipt {
        let label = |app: &str| {
            super::launcher::display_name_for_app_id(app)
                .unwrap_or_else(|| super::launcher::display_name_for_package_id(app))
        };
        // A grant for the place a confirmation ran in (an issue) is that
        // place's, whoever asked.
        let grantee = request
            .scope
            .as_ref()
            .map_or_else(|| label(&request.app_id), |scope| format!("“{scope}”"));
        let context = tilo_desktop_model::grants::app_action_grant_context(
            &request.room,
            &grantee,
            &label(&request.target_app),
            &format!("{} · {}", request.resource_label, request.action_label),
            answer.lifetime(),
        );
        tilo_desktop_model::WorkspaceEffectReceipt::recorded(
            tilo_actions::InvocationId::new(),
            &request.person,
            context,
            tilo_actions::EffectReceiptStatus::Succeeded,
            i64::try_from(tilo_clock::unix_now()).unwrap_or(i64::MAX),
        )
    }

    /// Whether the room's policy as this device holds it lets `person`
    /// give `grant` in `room`: what the host would refuse is refused here
    /// without asking. Why not, when it does not.
    fn precheck(
        &self,
        room: super::state::RoomId,
        person: &str,
        grant: &tilo_action_policy::Grant,
    ) -> Result<(), String> {
        let tree = self.tree.read();
        let mut edited = tree
            .desktop
            .workspaces
            .iter()
            .find(|workspace| workspace.room() == room)
            .cloned()
            .ok_or("the workspace is not on this device")?;
        drop(tree);
        edited
            .govern_all_at(
                person,
                vec![tilo_action_policy::Change::Grant(grant.clone())],
                tilo_clock::unix_now(),
            )
            .map(drop)
            .map_err(|refusal| refusal.to_string())
    }
}

impl tilo_action_broker::services::AppActionGrants for NativeAppActionGrants {
    fn may_grant(&self, room: &str, person: &str) -> bool {
        let tree = self.tree.read();
        tree.desktop
            .workspaces
            .iter()
            .find(|workspace| workspace.room().to_string() == room)
            .is_some_and(|workspace| {
                workspace.may_govern_at(
                    person,
                    tilo_action_policy::roles::governance::GRANT,
                    tilo_clock::unix_now(),
                )
            })
    }
    fn record(
        &self,
        request: &tilo_action_broker::app_actions::AppActionConsentRequest,
        grant: tilo_action_policy::Grant,
        answer: tilo_action_broker::app_actions::AppActionConsent,
    ) -> tilo_action_broker::services::GrantRecording {
        let Some(room) = super::state::RoomId::parse(&request.room) else {
            return Box::pin(std::future::ready(Err("no such workspace".into())));
        };
        let receipt = Self::receipt(request, answer);
        let tree = self.tree.clone();
        let checked = self.precheck(room, &request.person, &grant);
        let network = self.network.clone();
        Box::pin(async move {
            let asked = grant.clone();
            let recorded = match (checked, network) {
                (Err(refusal), _) => Err(refusal),
                (Ok(()), None) => Err("this workspace's host is out of reach".into()),
                (Ok(()), Some(network)) => network.put_grant_in_force(room, grant, receipt).await,
            };
            recorded
                .map(|id| id.as_str().to_owned())
                .inspect_err(|reason| {
                    // Said as any refused change to the room is.
                    tree.write().refuse_governance(
                        room,
                        super::state::GovernanceRequest::Policy(tilo_action_policy::Change::Grant(
                            asked,
                        )),
                        reason.clone(),
                    );
                })
        })
    }
    fn revoke(&self, room: &str, grant: &str) {
        let mut tree = self.tree.write();
        let Some(workspace) = tree
            .desktop
            .workspaces
            .iter_mut()
            .find(|workspace| workspace.room().to_string() == room)
        else {
            return;
        };
        let workspace_id = workspace.id;
        let grant = tilo_action_policy::GrantId::new(grant);
        let Some(revoker) = workspace
            .policy
            .grants
            .iter()
            .find(|held| held.id == grant)
            .and_then(|held| match &held.granted_by {
                tilo_action_policy::Subject::Person(person) => Some(person.clone()),
                _ => None,
            })
        else {
            return;
        };
        tree.revoke_in_workspace(workspace_id, &revoker, &grant);
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
        super::continuation_registry::global_registry()
            .lock()
            .map_err(|_| "continuation registry unavailable".to_string())?
            .report(
                room,
                app,
                super::continuation::AppInstanceId(instance),
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
        super::continuation_registry::global_registry()
            .lock()
            .map_err(|_| "continuation registry unavailable".to_string())?
            .clear(room, app, super::continuation::AppInstanceId(instance));
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
            collab_types::handoff::HandoffReport::decode(&message),
            Some(collab_types::handoff::HandoffReport::OwnerSubscribe { .. })
        ) {
            super::continuation_registry::global_registry()
                .lock()
                .map_err(|_| "continuation registry unavailable".to_owned())?
                .refresh_owner_view(room, &app, super::continuation::AppInstanceId(instance));
            return Ok(());
        }
        if let Some(collab_types::handoff::HandoffReport::OwnerTakeover {
            token,
            schema,
            metadata,
        }) = collab_types::handoff::HandoffReport::decode(&message)
        {
            use super::continuity_policy::{ContinuityCapability, ContinuityPolicySnapshot};
            if !ContinuityPolicySnapshot::current().permits(ContinuityCapability::AppContinuations)
            {
                return Err("app continuation permission is disabled".into());
            }
            return super::continuation_registry::global_registry()
                .lock()
                .map_err(|_| "continuation registry unavailable".to_owned())?
                .claim_owner_view(
                    room,
                    app,
                    super::continuation::AppInstanceId(instance),
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
    history_keys: Option<tilo_action_broker::channel_encryption::ChannelKeyProvider>,
    sealed_writes: Option<SealedWrites>,
) -> Arc<dyn NativeDocumentReads> {
    Arc::new(NativeOwnerCommitService {
        history_keys,
        sealed_writes,
        ..NativeOwnerCommitService::new(tree.clone(), network)
    })
}

fn native_workspace_memberships(
    tree: &WorkspaceTreeHandle,
    network: Option<Arc<super::network::ShellNetwork>>,
) -> Arc<dyn tilo_action_broker::services::WorkspaceMemberships> {
    Arc::new(NativeOwnerCommitService::new(tree.clone(), network))
}

/// The room's tracker, read for the broker's runs and for whoever follows
/// one in the shell: from the owner where it commits the room, from the
/// sealed writes this device holds where the room seals its records.
pub(crate) fn native_tracker(
    tree: &WorkspaceTreeHandle,
    network: Option<Arc<super::network::ShellNetwork>>,
    history_keys: Option<tilo_action_broker::channel_encryption::ChannelKeyProvider>,
    sealed_writes: Option<SealedWrites>,
) -> Arc<NativeTrackerRuns> {
    Arc::new(tracker_runs::NativeTrackerRuns::new(
        NativeOwnerCommitService {
            history_keys,
            sealed_writes,
            ..NativeOwnerCommitService::new(tree.clone(), network)
        },
    ))
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

fn native_person_chat(
    tree: &WorkspaceTreeHandle,
    network: Option<Arc<super::network::ShellNetwork>>,
) -> tilo_action_broker::services::person_chat::PersonChatService {
    Arc::new(NativeOwnerCommitService::new(tree.clone(), network))
}

#[allow(
    clippy::too_many_lines,
    reason = "the broker's composition root: one entry per service the shell gives it"
)]
pub fn native_services(
    membership: Option<WorkspaceTreeHandle>,
    network: Option<Arc<super::network::ShellNetwork>>,
    history_keys: Option<tilo_action_broker::channel_encryption::ChannelKeyProvider>,
    sealed_writes: Option<SealedWrites>,
) -> HostServices {
    let tracker_runs = membership.as_ref().map(|tree| {
        native_tracker(
            tree,
            network.clone(),
            history_keys.clone(),
            sealed_writes.clone(),
        ) as Arc<dyn tilo_action_broker::services::TrackerRuns>
    });
    let committed_documents = membership
        .as_ref()
        .map(|tree| native_document_reads(tree, network.clone(), history_keys, sealed_writes));
    let workspace_memberships = membership
        .as_ref()
        .map(|tree| native_workspace_memberships(tree, network.clone()));
    let resource_views = native_resource_views(membership.as_ref(), network.as_ref());
    let governance = membership.as_ref().map(|tree| {
        Arc::new(NativeWorkspaceGovernance(tree.clone(), network.clone()))
            as Arc<dyn WorkspaceGovernanceServices>
    });
    let shortcut_library = membership.as_ref().map(|tree| {
        Arc::new(NativeShortcutLibrary(tree.clone(), network.clone()))
            as Arc<dyn ShortcutLibraryServices>
    });
    let nearby_transfer = membership
        .as_ref()
        .zip(network.as_ref())
        .map(|(tree, network)| {
            Arc::new(NativeNearbyTransfer::new(Arc::clone(network), tree.clone()))
                as Arc<dyn tilo_action_broker::services::NearbyTransferServices>
        });
    let app_action_grants = membership.as_ref().map(|tree| {
        Arc::new(NativeAppActionGrants::new(tree.clone(), network.clone()))
            as Arc<dyn tilo_action_broker::services::AppActionGrants>
    });
    #[cfg(target_os = "linux")]
    let approved_checks = membership
        .as_ref()
        .map(|tree| approved_checks::service(tree.clone(), network.clone()));
    let launcher = Arc::new(NativeLauncher(membership.clone()));
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
        browser_service_sessions: membership.as_ref().zip(network.as_ref()).map(
            |(tree, network)| {
                Arc::new(service_sessions::NativeBrowserServiceSessions::new(
                    Arc::new(NativeOwnerCommitService::new(
                        tree.clone(),
                        Some(network.clone()),
                    )),
                ))
                    as Arc<dyn tilo_action_broker::services::BrowserServiceSessionServices>
            },
        ),
        #[cfg(target_os = "linux")]
        file_clipboard: membership.as_ref().map(|tree| {
            Arc::new(file_clipboard::NativeFileClipboard::new(tree.clone()))
                as Arc<dyn tilo_action_broker::services::FileClipboardServices>
        }),
        app_action_access: Some(super::app_action_access::installed_resolver()),
        app_action_grants,
        committed_documents,
        workspace_memberships,
        tracker_runs,
        person_chat: membership
            .as_ref()
            .map(|tree| native_person_chat(tree, network.clone())),
        resource_views,
        native_certificates: Some(Arc::new(NativeCertificates::default())),
        local_person,
        host_steps: membership
            .as_ref()
            .map(|tree| {
                [
                    Arc::new(super::device_pairing::BrokerDoor {
                        tree: tree.downgrade(),
                        pairing: super::device_pairing::Pairing::session(),
                    })
                        as Arc<dyn tilo_action_broker::services::HostStepServices>,
                    super::identity_admin::broker_door(tree),
                ]
            })
            .into_iter()
            .flatten()
            .chain(
                membership
                    .as_ref()
                    .zip(network.clone())
                    .map(|(tree, network)| super::home_relay_hosting::broker_door(tree, network)),
            )
            .chain([
                super::settings_records::broker_door(),
                super::activity_provider::broker_door(),
                super::desktop_door::broker_door(),
                super::crash_records::broker_door(),
                super::task_programs::broker_door(),
            ])
            .collect(),
        legacy_workspace: membership.map(|tree| {
            Arc::new(NativeWorkspaceAuthority(tree)) as Arc<dyn LegacyWorkspaceProjection>
        }),
        launcher: Some(launcher),
        workspace_governance: governance,
        shortcut_library,
        keyboard_shortcuts: Some(Arc::new(NativeKeyboardShortcuts)),
        continuations: Some(Arc::new(NativeContinuations)),
        search_index: Some(Arc::new(SearchIndexService(
            tilo_desktop_search::content_index::session(),
        ))),
        content_store: Some(Arc::new(SessionContentPuts)),
        speakers: network.clone().map(|network| {
            Arc::new(NativeSpeakers(network))
                as Arc<dyn tilo_action_broker::services::SpeakerServices>
        }),
        #[cfg(target_os = "linux")]
        drives: Some(Arc::new(NativeDrives)),
        applications: Some(Arc::new(NativeApplications)),
        wifi_continuity: network.clone().map(|network| {
            Arc::new(NativeWifiContinuity(network))
                as Arc<dyn tilo_action_broker::services::WifiContinuityServices>
        }),
        nearby_transfer,
        own_device: NativeOwnDevice::service(network.as_ref()),
        room_nodes: room_nodes::NativeRoomNodes::service(network.as_ref()),
        workspace_control: network.map(|network| {
            Arc::new(NativeWorkspaceControl(network)) as Arc<dyn WorkspaceControlServices>
        }),
        launchable_catalogue: launchable_catalogue_path(),
        // The session's records, installed before any broker starts.
        settings: settings_store::installed_source(),
        display_control,
        live_activities: Some(super::live_activities::board()),
        record_changes: Some(Arc::new(TaskChanges)),
        ..Default::default()
    }
}

/// The network carrying an application's action to another of the
/// person's own devices.
struct NativeOwnDevice(Arc<super::network::ShellNetwork>);

impl NativeOwnDevice {
    /// The service over `network`; none without a network.
    fn service(
        network: Option<&Arc<super::network::ShellNetwork>>,
    ) -> Option<Arc<dyn tilo_action_broker::services::OwnDeviceActions>> {
        network.map(|network| {
            Arc::new(Self(Arc::clone(network)))
                as Arc<dyn tilo_action_broker::services::OwnDeviceActions>
        })
    }
}

impl tilo_action_broker::services::OwnDeviceActions for NativeOwnDevice {
    fn invoke(
        &self,
        _caller: &tilo_actions::remote::Caller,
        _checked: &tilo_actions::remote::own_device::Checked,
        _request: &tilo_actions::remote::InvokeRequest,
    ) -> Result<
        tilo_actions::remote::Pending<tilo_action_broker::services::OwnDeviceOutcome>,
        tilo_actions::remote::ActionHostError,
    > {
        // Running an action on another of the person's devices is the device
        // engine's, which this session does not hand actions to yet.
        Err(tilo_actions::remote::ActionHostError::Unavailable {
            reason: "actions run on this device only".into(),
        })
    }

    fn ask_person(
        &self,
        caller: &tilo_actions::remote::Caller,
        question: &tilo_actions::remote::ConfirmationQuestion,
    ) -> Result<
        tilo_actions::remote::Pending<
            Result<tilo_actions::remote::Confirmation, tilo_actions::remote::ActionHostError>,
        >,
        tilo_actions::remote::ActionHostError,
    > {
        let Some(person) = caller.user_public_key() else {
            return Err(tilo_actions::remote::ActionHostError::NotPermitted);
        };
        let person = tilo_action_broker::collab_app_broker::principal_id_for_root(*person);
        if caller.principal() != &tilo_actions::remote::CallerPrincipal::Person(person) {
            return Err(tilo_actions::remote::ActionHostError::NotPermitted);
        }
        let Some(room) = crate::shell::state::RoomId::parse(&question.room)
            .filter(|room| self.0.workspace_of_room(*room).is_some())
        else {
            return Err(tilo_actions::remote::ActionHostError::ConfirmationRequired);
        };
        // The token names the answer they gave; the host binds it to the one
        // invocation it asked about.
        let witness = hex::encode(question.id);
        self.0.ask_person(question, room).map(|asked| {
            asked.map(|answer| {
                answer.map(|_| tilo_actions::remote::Confirmation::Witnessed(witness))
            })
        })
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
        launcher: Some(Arc::new(NativeLauncher(Some(membership.clone())))),
        workspace_governance: Some(Arc::new(NativeWorkspaceGovernance(
            membership.clone(),
            None,
        ))),
        shortcut_library: Some(Arc::new(NativeShortcutLibrary(membership.clone(), None))),
        keyboard_shortcuts: Some(Arc::new(NativeKeyboardShortcuts)),
        app_action_grants: Some(Arc::new(NativeAppActionGrants::new(membership, None))),
        applications: Some(Arc::new(NativeApplications)),
        ..Default::default()
    };
    tilo_action_broker::collab_app_broker::AppBrokerState::test_broker_with_authority(
        authority, services,
    )
}

use std::rc::Rc;
use tilo_lock::MutexRecover as _;
use tokio::sync::Mutex as AsyncMutex;
/// Speaker groups over the certified personal-device network.
pub struct NativeSpeakers(pub tilo_device_fabric::link::SharedLink);
impl tilo_action_broker::services::SpeakerServices for NativeSpeakers {
    fn command(&self, command: runtime_collab::SpeakerCommand) -> runtime_collab::SpeakerReply {
        tilo_speaker_service::settings::handle(&self.0, command)
    }

    fn changed(&self, after: u64) -> Option<tilo_action_broker::services::SpeakerChange> {
        Some(Box::pin(tilo_speaker_service::settings::changed(
            &self.0, after,
        )))
    }
}
/// The session's NTFS drives, which tell the certified Files route when
/// the list it is given changes.
#[cfg(target_os = "linux")]
pub struct NativeDrives;

#[cfg(target_os = "linux")]
impl tilo_action_broker::services::DriveServices for NativeDrives {
    fn changed(&self, after: u64) -> Option<tilo_action_broker::services::DriveChange> {
        Some(Box::pin(crate::shell::drives::changed(after)))
    }
}
/// The person's apps, which tell the certified Files route when the list it
/// is given changes.
pub struct NativeApplications;

impl tilo_action_broker::services::ApplicationServices for NativeApplications {
    fn changed(&self, after: u64) -> Option<tilo_action_broker::services::ApplicationChange> {
        Some(Box::pin(crate::shell::app_list::changed(after)))
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
            runtime_collab::WifiContinuityCommand::NetworkWake {
                profile_uuid,
                enabled,
            } => {
                let result = match enabled {
                    Some(enabled) => os_actions::network_wake::configure(&profile_uuid, enabled),
                    None => os_actions::network_wake::status(&profile_uuid),
                };
                match result {
                    Ok(status) => runtime_collab::WifiContinuityReply::NetworkWake {
                        status: runtime_collab::NetworkWakeStatus {
                            profile: status.profile,
                            interface: status.interface,
                            configured: status.configured,
                            supported: status.supported,
                            applied: status.applied,
                            problem: status
                                .problem
                                .map(|problem| problem.chars().take(128).collect()),
                        },
                    },
                    Err(reason) => runtime_collab::WifiContinuityReply::Rejected {
                        reason: reason.chars().take(128).collect(),
                    },
                }
            }
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
        revalidate: tilo_action_broker::services::ControlRevalidate,
    ) -> Result<Rc<dyn tilo_action_broker::services::WorkspaceControlSession>, String> {
        revalidate()?;
        let target = iroh::EndpointId::from_bytes(&target)
            .map_err(|_| "invalid target device identity".to_owned())?;
        let session = self
            .0
            .open_workspace_control(tilo_network_actor::room_topic_id(room), target)?;
        let initial_route = match session.route() {
            tilo_network_actor::WorkspaceControlRoute::Direct { .. } => {
                runtime_collab::WorkspaceControlRoute::Direct
            }
            tilo_network_actor::WorkspaceControlRoute::Relayed { via } => {
                runtime_collab::WorkspaceControlRoute::Relayed {
                    via: *via.as_bytes(),
                }
            }
        };
        Ok(Rc::new(NativeWorkspaceControlSession {
            revalidate,
            network: self.0.clone(),
            session: AsyncMutex::new(session),
            inbound: AsyncMutex::new(self.0.subscribe_workspace_control()),
            initial: AsyncMutex::new(Some(
                runtime_collab::WorkspaceControlEvent::WaitingForConsent {
                    route: initial_route,
                },
            )),
        }))
    }
}
struct NativeWorkspaceControlSession {
    revalidate: tilo_action_broker::services::ControlRevalidate,
    network: Arc<crate::shell::network::ShellNetwork>,
    session: AsyncMutex<tilo_network_actor::workspace_control::WorkspaceControlSession>,
    inbound: AsyncMutex<
        tokio::sync::mpsc::UnboundedReceiver<crate::shell::network::WorkspaceControlNetworkEvent>,
    >,
    initial: AsyncMutex<Option<runtime_collab::WorkspaceControlEvent>>,
}

fn workspace_control_event(
    update: tilo_network_actor::workspace_control::WorkspaceControlUpdate,
) -> runtime_collab::WorkspaceControlEvent {
    match update {
        tilo_network_actor::workspace_control::WorkspaceControlUpdate::ConsentGranted {
            surface,
            width,
            height,
        } => runtime_collab::WorkspaceControlEvent::StreamStarted {
            surface,
            width,
            height,
        },
        tilo_network_actor::workspace_control::WorkspaceControlUpdate::Frame {
            surface,
            encoded_h264,
            timestamp,
            seq,
        } => runtime_collab::WorkspaceControlEvent::Frame {
            surface,
            encoded_h264,
            timestamp,
            seq,
        },
        tilo_network_actor::workspace_control::WorkspaceControlUpdate::Closed(reason) => {
            runtime_collab::WorkspaceControlEvent::Closed(reason)
        }
    }
}

fn closed_workspace_control_event(
    session: &tilo_network_actor::workspace_control::WorkspaceControlSession,
) -> Option<runtime_collab::WorkspaceControlEvent> {
    let tilo_network_actor::workspace_control::WorkspaceControlStatus::Closed(reason) =
        session.status()
    else {
        return None;
    };
    Some(workspace_control_event(
        tilo_network_actor::workspace_control::WorkspaceControlUpdate::Closed(reason.clone()),
    ))
}

impl NativeWorkspaceControlSession {
    async fn next_event(&self) -> Result<runtime_collab::WorkspaceControlEvent, String> {
        let initial_event = self.initial.lock().await.take();
        if let Some(event) = initial_event {
            (self.revalidate)()?;
            return Ok(event);
        }
        loop {
            let closed = closed_workspace_control_event(&*self.session.lock().await);
            if let Some(event) = closed {
                return Ok(event);
            }
            if let Err(error) = (self.revalidate)() {
                return Ok(workspace_control_event(
                    self.session.lock().await.deny(error),
                ));
            }
            let network_event =
                match tokio::time::timeout(std::time::Duration::from_millis(250), async {
                    self.inbound.lock().await.recv().await
                })
                .await
                {
                    Ok(Some(event)) => event,
                    Ok(None) => return Err("workspace-control observer closed".to_owned()),
                    Err(_) => continue,
                };
            let mut session = self.session.lock().await;
            (self.revalidate)()?;
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
            drop(session);
            if let Some(update) = update {
                return Ok(workspace_control_event(update));
            }
        }
    }

    async fn input(&self, input: runtime_collab::WorkspaceControlInput) -> Result<(), String> {
        (self.revalidate)()?;
        let session = self.session.lock().await;
        (self.revalidate)()?;
        let input = match input {
            runtime_collab::WorkspaceControlInput::RequestKeyframe { seq } => {
                let published = self
                    .network
                    .publish_workspace_control_keyframe_request(&session, seq);
                drop(session);
                return published;
            }
            runtime_collab::WorkspaceControlInput::PointerMotion { x, y } => {
                tilo_window_share::streaming::InputEvent::PointerMotion { x, y }
            }
            runtime_collab::WorkspaceControlInput::PointerButton { button, pressed } => {
                tilo_window_share::streaming::InputEvent::PointerButton { button, pressed }
            }
            runtime_collab::WorkspaceControlInput::Key {
                keycode,
                pressed,
                modifiers,
            } => tilo_window_share::streaming::InputEvent::Key {
                keysym: keycode,
                pressed,
                modifiers,
            },
            runtime_collab::WorkspaceControlInput::KeyboardFocus { focused } => {
                let tilo_network_actor::workspace_control::WorkspaceControlStatus::Active {
                    surface,
                    ..
                } = session.status()
                else {
                    return Err("target has not granted control".to_owned());
                };
                tilo_window_share::streaming::InputEvent::KeyboardFocus {
                    surface: *surface,
                    focused,
                }
            }
        };
        let published = self
            .network
            .publish_workspace_control_input(&session, &input);
        drop(session);
        published
    }
}

impl tilo_action_broker::services::WorkspaceControlSession for NativeWorkspaceControlSession {
    fn read(
        &self,
    ) -> tilo_action_broker::services::ControlAnswer<'_, runtime_collab::WorkspaceControlEvent>
    {
        Box::pin(self.next_event())
    }

    fn send_input(
        &self,
        input: runtime_collab::WorkspaceControlInput,
    ) -> tilo_action_broker::services::ControlAnswer<'_, ()> {
        Box::pin(self.input(input))
    }

    fn close(&self) -> tilo_action_broker::services::ControlAnswer<'_, ()> {
        Box::pin(async move {
            self.session.lock().await.disconnect();
            Ok(())
        })
    }
}

/// The host service the broker hands apps' search publications to: the
/// session's content index.
struct SearchIndexService(Arc<tilo_desktop_search::content_index::AppContentIndex>);

impl tilo_action_broker::services::SearchIndexServices for SearchIndexService {
    fn publish(
        &self,
        room: tilo_desktop_model::RoomId,
        app: &str,
        record: &str,
        delta: &[u8],
    ) -> Result<(), String> {
        self.0.publish(&room.to_string(), app, record, delta)
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
        let certificate =
            tilo_peer_state::owner::testing::certificate(&root, &device, "clock test");
        let room_id = uuid::Uuid::parse_str(tilo_peer_state::owner::testing::ROOM).unwrap();
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
            keywords: Vec::new(),
            category: None,
            version: "1".into(),
            manifest_path: std::path::PathBuf::from("/x/manifest.toml"),
            payload_root: std::path::PathBuf::from("/x"),
            has_gui: true,
            opens_extensions: Vec::new(),
            opens_mime_types: Vec::new(),
            opens_uri_schemes: Vec::new(),
            opens_secondary: false,
            opens_resource_kinds: Vec::new(),
            offers: Vec::new(),
        };
        let host = |id: &str, name: &str| crate::shell::host_apps::HostApp {
            desktop_id: id.into(),
            name: name.into(),
            kind: desktop_entry::EntryKind::System,
            categories: Vec::new(),
            keywords: Vec::new(),
            window_identities: vec![id.into()],
            mime_types: Vec::new(),
            terminal: false,
            icon: None,
            downloaded: None,
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
        let native = launchable_applications_for(
            "so.tilo.word-processor",
            vec![package("so.tilo.notes"), package("so.tilo.word-processor")],
            || panic!("native lookup must not enumerate host apps"),
        );
        assert_eq!(native.len(), 1);
        assert_eq!(native[0].package_id, apps[0].package_id);
        assert_eq!(native[0].display_name, apps[0].display_name);
        assert!(!native[0].unconfined);
        assert!(
            launchable_applications_for("missing", Vec::new(), || {
                panic!("missing native lookup must not enumerate host apps")
            })
            .is_empty()
        );
        for id in ["host:chromium-browser", "host:SO.TILO.WORD-PROCESSOR"] {
            let selected =
                launchable_applications_for(id, vec![package("so.tilo.word-processor")], || {
                    vec![
                        host("chromium-browser", "Chromium"),
                        host("SO.TILO.WORD-PROCESSOR", "Shadowed"),
                    ]
                });
            assert_eq!(
                selected
                    .iter()
                    .map(|app| (&app.package_id, app.unconfined))
                    .collect::<Vec<_>>(),
                apps.iter()
                    .filter(|app| app.package_id == id)
                    .map(|app| (&app.package_id, app.unconfined))
                    .collect::<Vec<_>>(),
            );
        }
    }

    #[test]
    fn a_host_application_refuses_launch_content_before_spawning() {
        let workspace = crate::shell::state::Workspace::new(
            crate::shell::state::WorkspaceId(3),
            "host launch test",
        );
        let refused = NativeLauncher(None).launch_content(
            "host:chromium-browser",
            &workspace,
            LaunchContent {
                hash: "00",
                caller: None,
                author: None,
            },
            &LaunchPlace::Focus,
        );
        assert_eq!(
            refused,
            Err("opening launch content in a host application is unsupported".into())
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
        assert!(
            !workspace
                .app_capabilities_at("so.tilo.photos", tilo_clock::unix_now())
                .is_empty()
        );

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
        let mut session = tilo_network_actor::workspace_control::WorkspaceControlSession::new(
            7,
            11,
            target,
            tilo_network_actor::WorkspaceControlRoute::Direct { target },
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

#[cfg(test)]
#[path = "broker_services/host_admission_tests.rs"]
mod host_admission_tests;

/// Where an agent task's changes of a tilolang app's record are applied: at the record's
/// origin, by the host of the app's workspace and author, with no instance
/// ([`super::launcher::apply_task_change`]).
struct TaskChanges;

impl tilo_action_broker::services::RecordChangeServices for TaskChanges {
    fn apply(
        &self,
        room: &str,
        change: &tilo_actions::remote::RecordChange,
        invoker: &tilo_actions::ActionInvoker,
    ) -> Result<serde_json::Value, String> {
        let applied = runtime_portal::tilolang_host::Change {
            resource: change.resource.clone(),
            action: change.action.clone(),
            arguments: change.arguments.clone(),
            invoker: serde_json::to_value(invoker).map_err(|error| error.to_string())?,
            context: None,
            preview: change.preview.clone(),
        };
        super::launcher::apply_task_change(room, &change.app, &applied)
            .map_err(|error| error.to_string())
    }
}
