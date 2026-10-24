#![allow(
    refining_impl_trait,
    reason = "the Cap'n Proto server impls return the concrete `Promise` where the generated traits declare `impl Future`"
)]
//! Adapters from the shared broker to the native session's existing services.
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
mod owner_source;
mod person_chat;
#[cfg(test)]
pub(crate) use owner_source::OPENING as NATIVE_OWNER_OPENING;
mod resource_views;
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
    writes: OwnerWrites,
}

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
impl tilo_owner_commit::OwnerCommitService for NativeOwnerCommitService {
    fn commit_member_effect_receipt(
        &self,
        request: tilo_owner_commit::OwnerCommitRequest,
        caller: tilo_user_identity::DeviceCert,
    ) -> tilo_owner_commit::CommitFuture {
        let owner = self.owner.clone();
        let network = self.network.clone();
        let writes = self.writes.clone();
        Box::pin(writes.run(move || {
            let owner = owner
                .resolve()
                .map_err(|error| format!("native effect governor unavailable: {error}"))?;
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
        }))
    }
    fn requires_room(&self, room: &str) -> bool {
        super::state::RoomId::parse(room)
            .is_some_and(|room| self.tree.read().commits_native_documents(room))
    }
    fn health(&self) -> Result<(), String> {
        self.opened_owner("native durable owner unavailable")?
            .health()
    }
    fn commit(
        &self,
        request: tilo_owner_commit::OwnerCommitRequest,
    ) -> tilo_owner_commit::CommitFuture {
        let owner = self.owner.clone();
        let network = self.network.clone();
        let writes = self.writes.clone();
        Box::pin(async move {
            let committed = writes
                .run({
                    let network = network.clone();
                    move || {
                        let owner = owner.resolve().map_err(|error| {
                            format!("native durable owner unavailable: {error}")
                        })?;
                        let room = super::state::RoomId::parse(&request.room)
                            .ok_or("invalid native room")?;
                        if let Some(network) = network.as_ref() {
                            network.check_native_custody(&owner, room)?;
                            // The room's receipts are its governor's to record.
                            if matches!(
                                request.operation,
                                tilo_owner_commit::CommitOperation::EffectReceipt { .. }
                            ) && network.governed_elsewhere(room)
                            {
                                return Ok(Committed::Elsewhere(Box::new(Elsewhere {
                                    owner,
                                    room,
                                    request,
                                })));
                            }
                        }
                        let now = std::time::SystemTime::now();
                        if matches!(
                            request.operation,
                            tilo_owner_commit::CommitOperation::EffectReceipt { .. }
                        ) {
                            let (key, governor) =
                                tilo_identity_host::broker::trusted_current_device(&request.room)
                                    .map_err(|error| {
                                    format!("effect governor custody unavailable: {error}")
                                })?;
                            owner
                                .commit_effect_receipt(&request, &governor, &key, now)
                                .map(Committed::Here)
                        } else {
                            owner.commit_document(request, now).map(Committed::Here)
                        }
                    }
                })
                .await?;
            match committed {
                Committed::Here(receipt) => Ok(receipt),
                Committed::Elsewhere(elsewhere) => {
                    let Elsewhere {
                        owner,
                        room,
                        request,
                    } = *elsewhere;
                    let network = network.ok_or("effect governor unavailable")?;
                    let tilo_owner_commit::CommitOperation::EffectReceipt { redacted, .. } =
                        &request.operation
                    else {
                        return Err("missing effect receipt operation".into());
                    };
                    record_receipt_elsewhere(&network, &owner, room, &request, redacted).await
                }
            }
        })
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
            if view.device() != view.governor() {
                return Err("native checkpoint requires the actual governing owner".into());
            }
            Ok(())
        })?;
        let (key, governor) = tilo_identity_host::broker::trusted_current_device(room)
            .map_err(|error| format!("checkpoint governor custody unavailable: {error}"))?;
        let now = std::time::SystemTime::now();
        owner.with_current(room_id, |view| {
            if view.device() != view.governor() || view.governor() != &governor {
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
                if view.device() != view.governor() {
                    return Err("native checkpoint requires the actual governing owner".into());
                }
                Ok(())
            })?;
            let (key, governor) = tilo_identity_host::broker::trusted_current_device(&request.room)
                .map_err(|error| format!("checkpoint governor custody unavailable: {error}"))?;
            owner.with_current(room, |view| {
                if view.governor() != &governor {
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

    fn requires_room(&self, room: &str) -> bool {
        tilo_owner_commit::OwnerCommitService::requires_room(self, room)
    }

    /// A room its node stewards: this owner forwards an opening app's
    /// authorship to that host ([`grant_authorship_at_host`]).
    fn grants_authorship_at_host(&self, room: &str) -> bool {
        super::state::RoomId::parse(room).is_some_and(|room| {
            let tree = self.tree.read();
            tree.requires_native_owner(room) && !tree.commits_native_documents(room)
        })
    }

    fn health(&self) -> Result<(), String> {
        self.opened_owner("native membership owner unavailable")?
            .health()
    }

    fn execute(
        &self,
        request: tilo_owner_commit::OwnerCommitRequest,
    ) -> tilo_owner_commit::CommitFuture {
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
                if network.governed_elsewhere(room) {
                    return grant_authorship_at_host(network, &owner, room, &request).await;
                }
            }
            let (key, governor) = tilo_identity_host::broker::trusted_current_device(&request.room)
                .map_err(|error| format!("workspace governor is unavailable: {error}"))?;
            let receipt = owner.commit_workspace_membership_command(
                request,
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
}

/// How long the app authorship a host committed may take to reach this
/// device's admitted room. With the host's own answer (at most
/// `relay_rooms::ADMIN_TIMEOUT`) it stays inside the 30 s a launch waits.
const HOST_AUTHORSHIP_ADMISSION_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// In a room a host of this person governs (a home relay), opening an app
/// there for the first time asks that host to let the app author the room's
/// records: the host is the one party that may commit it. Every other
/// membership change in a room governed elsewhere stays refused here.
/// Answers with this device's committed view once it carries the authorship.
async fn grant_authorship_at_host(
    network: &super::network::ShellNetwork,
    owner: &super::native_workspace_owner::NativeWorkspaceOwner,
    room: super::state::RoomId,
    request: &tilo_owner_commit::OwnerCommitRequest,
) -> Result<tilo_owner_commit::OwnerCommitReceipt, String> {
    const REFUSED: &str = "workspace membership requires the governing owner";
    let tilo_owner_commit::CommitOperation::WorkspaceMembershipCommand {
        caller,
        command:
            tilo_peer_state::workspace_membership::WorkspaceMembershipCommand::GrantAppAuthorship {
                app,
            },
        ..
    } = &request.operation
    else {
        return Err(REFUSED.into());
    };
    network
        .grant_app_authorship_at_host(room, caller, app)
        .await
        .ok_or(REFUSED)??;
    // The host's answer is not this device's commit barrier: wait until the
    // authorship it published is in this device's actual admitted room.
    tokio::time::timeout(HOST_AUTHORSHIP_ADMISSION_TIMEOUT, async {
        loop {
            network.check_native_custody(owner, room)?;
            let receipt = owner.with_current(room, |view| {
                let now = std::time::SystemTime::now();
                let member = view.peer().permissions(caller.device_public_key, now)?;
                if !member
                    .policy
                    .app_authors(app, &tilo_action_policy::Context::at_time(now))
                {
                    return Ok(None);
                }
                Ok(Some(tilo_owner_commit::OwnerCommitReceipt {
                    binding: request.binding()?,
                    generation: view.generation(),
                    aggregate_hash: *blake3::hash(&view.peer().durable()?).as_bytes(),
                }))
            })?;
            if let Some(receipt) = receipt {
                return Ok(receipt);
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
    })
    .await
    .map_err(|_| "the host committed the app authorship but it did not reach this device in time")?
}

/// A commit's outcome on the owner's thread: done here, or a receipt for
/// the room's governor on another device, recorded over the network.
enum Committed {
    Here(tilo_owner_commit::OwnerCommitReceipt),
    Elsewhere(Box<Elsewhere>),
}
struct Elsewhere {
    owner: super::native_workspace_owner::NativeWorkspaceOwner,
    room: super::state::RoomId,
    request: tilo_owner_commit::OwnerCommitRequest,
}

/// Have the governor of `room` record an effect receipt this device's broker
/// produced, and answer with this device's committed view of the room.
async fn record_receipt_elsewhere(
    network: &super::network::ShellNetwork,
    owner: &super::native_workspace_owner::NativeWorkspaceOwner,
    room: super::state::RoomId,
    request: &tilo_owner_commit::OwnerCommitRequest,
    redacted: &[u8],
) -> Result<tilo_owner_commit::OwnerCommitReceipt, String> {
    let tilo_owner_commit::CommitOperation::EffectReceipt { invocation, .. } = &request.operation
    else {
        return Err("missing effect receipt operation".into());
    };
    let expected = tilo_peer_state::PeerWorkspace::validate_workspace_effect_receipt_encoding(
        *invocation,
        redacted,
    )?;
    network
        .record_governed_effect_receipt(room, *invocation, redacted.to_vec())
        .await?;
    // A transport acknowledgement is not our commit barrier: wait until the
    // governor's signed receipt is in this device's actual admitted room.
    tokio::time::timeout(governed_receipt_wait(), async {
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
                    return Ok(Some(tilo_owner_commit::OwnerCommitReceipt {
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
    /// The room's agents whose delegation the room's governance verified for
    /// exactly their device ([`super::state::WorkspaceTree::verified_agents`]).
    fn history_agents(&self, room: &str) -> Vec<collab_types::history::RosterAgent> {
        super::state::RoomId::parse(room)
            .map(|room| self.0.read().verified_agents(room))
            .unwrap_or_default()
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
            .find(|w| w.room().to_string() == room)
            .map(|w| w.id)
            .ok_or("effect workspace no longer hosted")?;
        // Preserve native projection semantics; this void flush is not OwnerCommitService.
        let _ = tree.append_app_effect_receipt(id, invocation, actor, context, status);
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

/// How long an effect receipt recorded at a room's governor may take to come
/// back into this device's admitted room.
#[cfg(not(test))]
fn governed_receipt_wait() -> std::time::Duration {
    GOVERNED_OPEN_TIMEOUT
}
/// How long an effect receipt recorded at a room's governor may take to come
/// back into this device's admitted room: in test builds, whose fixture
/// governor and relay share a loaded runner, the shared test hang bound.
/// Running out reports the outcome unknown and turns the app's collaboration
/// off for good, so a slow round trip would otherwise read as a refusal.
#[cfg(test)]
fn governed_receipt_wait() -> std::time::Duration {
    tilo_test_env::hang_bound()
}

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
        place: &LaunchPlace,
    ) -> Option<Result<(), String>> {
        let resource = format!(
            "{}{app}",
            tilo_desktop_model::commands::OPEN_APP_RESOURCE_PREFIX
        );
        let answer = self
            .1
            .as_ref()
            .filter(|network| network.governor_opens_apps(workspace.room()))?
            .invoke_governor(
                workspace.room(),
                tilo_desktop_model::commands::APP_ID,
                &resource,
                tilo_desktop_model::commands::OPEN_APP_ACTION,
                self.beside_group(workspace, place)
                    .map(|group| serde_json::json!({ "beside": group })),
            );
        Some(match answer.wait_timeout(GOVERNED_OPEN_TIMEOUT) {
            Ok(Some(result)) => result.map(drop),
            Ok(None) => Err("the workspace host stopped before opening the app".into()),
            Err(_) => Err("the workspace host did not open the app in time".into()),
        })
    }

    /// The group of `workspace` an open at `place` goes beside, as the
    /// governor of a room names it: the view the actor works in.
    fn beside_group(
        &self,
        workspace: &tilo_desktop_model::Workspace,
        place: &LaunchPlace,
    ) -> Option<u64> {
        let LaunchPlace::Beside { app_id } = place else {
            return None;
        };
        let tree = self.0.as_ref()?.read();
        tree.desktop
            .workspaces
            .iter()
            .find(|candidate| candidate.room() == workspace.room())
            .and_then(|workspace| super::placement::group_showing(workspace, app_id))
    }

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
            .find(|candidate| candidate.room() == workspace.room())
            .map(|candidate| candidate.id)
        else {
            return;
        };
        let Some(stored) = tree.grant_app_authorship(id, &person.to_string(), app) else {
            return;
        };
        drop(tree);
        workspace.grant(stored);
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
    fn person_initiated(&self, app_id: &str) -> bool {
        self.0.as_ref().is_some_and(|tree| {
            crate::compositor::activation::person_initiated(&tree.read(), app_id)
        })
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
        if let Some(opened) = self.open_through_governor(app, workspace, place) {
            return opened;
        }
        self.place(app, workspace, place);
        // Opening an app yourself makes it an author. A launch that only
        // forwards the workspace's existing grants opens a viewer, and
        // typing into it does nothing.
        let mut workspace = workspace.clone();
        let person = super::state::local_user_id_for_room(&workspace.room());
        self.grant_authorship(&person, app, &mut workspace);
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
            None => super::launcher::launch_in_workspace(app, &workspace),
        };
        launched.map(|_| ()).map_err(|e| e.to_string())
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
                super::host_apps::launch(desktop_id, Some(workspace.room().to_string()))
                    .map(drop)
                    .map_err(|error| error.to_string())
            });
        }
        if self
            .1
            .as_ref()
            .is_some_and(|network| network.governor_opens_apps(workspace.room()))
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
            .is_some_and(|network| network.governor_opens_apps(workspace.room()))
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
        if content.hash.is_empty()
            && let Some(opened) = self.open_through_governor(app, workspace, place)
        {
            return opened.map(|()| Launched::Unconfirmed);
        }
        let mut workspace = workspace.clone();
        if let Some(person) = content.author {
            self.grant_authorship(person, app, &mut workspace);
        }
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
            .find(|workspace| workspace.room().to_string() == request.room)
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
            .find(|workspace| workspace.room().to_string() == room)
            .map(crate::shell::state::Workspace::room)?;
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
        let elsewhere = self.governor_elsewhere(request.room);
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
        if let Some((network, room)) = elsewhere.filter(|(_, room)| *room == workspace.room()) {
            drop(tree);
            return govern_elsewhere(network, room, operation);
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

/// Have the governor of `room` apply a governance `operation` as a desktop
/// command from this device.
fn govern_elsewhere(
    network: &super::network::ShellNetwork,
    room: super::state::RoomId,
    operation: tilo_desktop::workspace_settings::GovernanceOperation,
) -> Result<(), String> {
    use tilo_desktop::workspace_settings::GovernanceOperation as Operation;
    use tilo_desktop_model::commands::{BoundCommand, CommandSource, DesktopCommand};
    let command = match operation {
        // A grant is the room's policy, which the governor decides as it
        // would for a person at its own screen.
        operation @ Operation::Grant { .. } => {
            let grant = governed_grant(room, operation)?;
            return govern_membership(
                network,
                room,
                "grant",
                serde_json::json!({ "grant": grant }),
            );
        }
        Operation::Revoke { grant } => {
            return govern_membership(
                network,
                room,
                "revoke",
                serde_json::json!({ "grant": grant }),
            );
        }
        Operation::SavePreset {
            name,
            description,
            rules,
        } => DesktopCommand::SavePreset {
            preset_json: serde_json::to_string(&tilo_action_policy::Preset {
                name,
                description,
                rules,
            })
            .map_err(|error| error.to_string())?,
        },
        Operation::AssignPreset {
            preset,
            subject,
            expires_unix,
        } => {
            let (subject_kind, subject) = match subject {
                tilo_action_policy::Subject::Person(id) => ("person", id),
                tilo_action_policy::Subject::App(id) => ("app", id),
                tilo_action_policy::Subject::Agent(id) => ("agent", id),
                _ => return Err("a grant set is assigned to a person, an app or an agent".into()),
            };
            DesktopCommand::AssignPreset {
                preset,
                subject_kind: subject_kind.into(),
                subject,
                expires_unix,
            }
        }
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

/// The grant a governance `operation` gives in `room`; the governor records
/// who gave it.
fn governed_grant(
    room: super::state::RoomId,
    operation: tilo_desktop::workspace_settings::GovernanceOperation,
) -> Result<tilo_action_policy::Grant, String> {
    let tilo_desktop::workspace_settings::GovernanceOperation::Grant {
        subject,
        node,
        kinds,
        answer,
        lifetime,
    } = operation
    else {
        return Err("not a grant".into());
    };
    Ok(tilo_action_policy::Grant::new(
        format!("grant:{}", uuid::Uuid::new_v4()),
        subject,
        tilo_action_policy::TreePath::room(room.to_string()).join(&node),
        kinds,
        answer,
        tilo_action_policy::Subject::Person(String::new()),
    )
    .with_lifetime(lifetime.unwrap_or(tilo_action_policy::Lifetime::UntilRevoked)))
}

/// Have the governor of `room` apply the membership action `action` of the
/// room itself, with `argument`, as this device's person.
fn govern_membership(
    network: &super::network::ShellNetwork,
    room: super::state::RoomId,
    action: &str,
    argument: serde_json::Value,
) -> Result<(), String> {
    let answer = network.invoke_governor(
        room,
        tilo_desktop_model::commands::APP_ID,
        WORKSPACE_MEMBERSHIP_RESOURCE,
        action,
        Some(argument),
    );
    match answer.wait_timeout(GOVERNED_OPEN_TIMEOUT) {
        Ok(Some(result)) => result.map(drop),
        Ok(None) => Err("the workspace host stopped before applying the change".into()),
        Err(_) => Err("the workspace host did not apply the change in time".into()),
    }
}

/// The resource the room's own membership actions are published on.
const WORKSPACE_MEMBERSHIP_RESOURCE: &str = "desktop:workspace";

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
/// workspace's own records by the person who gave them and noted in its
/// history.
pub struct NativeAppActionGrants(pub WorkspaceTreeHandle);
impl tilo_action_broker::services::AppActionGrants for NativeAppActionGrants {
    fn may_grant(&self, room: &str, person: &str) -> bool {
        let tree = self.0.read();
        tree.desktop
            .workspaces
            .iter()
            .find(|workspace| workspace.room().to_string() == room)
            .is_some_and(|workspace| {
                tree.governs_room(workspace.room())
                    && workspace.may_govern_at(
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
    ) -> Option<String> {
        let mut tree = self.0.write();
        let workspace_id = tree
            .desktop
            .workspaces
            .iter()
            .find(|workspace| workspace.room().to_string() == request.room)?
            .id;
        let id = tree.grant_in_workspace(workspace_id, &request.person, grant)?;
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
        Some(id.as_str().to_owned())
    }
    fn revoke(&self, room: &str, grant: &str) {
        let mut tree = self.0.write();
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
        if workspace.revoke_grant(&grant) {
            tree.doc_write_policy(
                workspace_id,
                &super::state::PolicyEdit {
                    revoked: vec![grant],
                    ..super::state::PolicyEdit::default()
                },
            );
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
            if !ContinuityPolicySnapshot::current().permits(ContinuityCapability::AppContinuations)
            {
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
    history_keys: Option<tilo_action_broker::channel_encryption::ChannelKeyProvider>,
) -> Arc<dyn NativeDocumentReads> {
    Arc::new(NativeOwnerCommitService {
        history_keys,
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
/// one in the shell.
pub(crate) fn native_tracker(
    tree: &WorkspaceTreeHandle,
    network: Option<Arc<super::network::ShellNetwork>>,
    history_keys: Option<tilo_action_broker::channel_encryption::ChannelKeyProvider>,
) -> Arc<NativeTrackerRuns> {
    Arc::new(tracker_runs::NativeTrackerRuns::new(
        NativeOwnerCommitService {
            history_keys,
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
) -> HostServices {
    let tracker_runs = membership.as_ref().map(|tree| {
        native_tracker(tree, network.clone(), history_keys.clone())
            as Arc<dyn tilo_action_broker::services::TrackerRuns>
    });
    let committed_documents = membership
        .as_ref()
        .map(|tree| native_document_reads(tree, network.clone(), history_keys));
    let workspace_memberships = membership
        .as_ref()
        .map(|tree| native_workspace_memberships(tree, network.clone()));
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
            .chain([
                super::settings_records::broker_door(),
                super::activity_provider::broker_door(),
                super::crash_records::broker_door(),
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
        search_index: Some(Arc::new(
            crate::shell::search::app_content_index::SearchIndexService(
                crate::shell::search::app_content_index::session(),
            ),
        )),
        content_store: Some(Arc::new(SessionContentPuts)),
        speakers: network.clone().map(|network| {
            Arc::new(NativeSpeakers(network))
                as Arc<dyn tilo_action_broker::services::SpeakerServices>
        }),
        wifi_continuity: network.clone().map(|network| {
            Arc::new(NativeWifiContinuity(network))
                as Arc<dyn tilo_action_broker::services::WifiContinuityServices>
        }),
        nearby_transfer,
        own_device: NativeOwnDevice::service(network.as_ref()),
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
        caller: &tilo_actions::remote::Caller,
        checked: &tilo_actions::remote::own_device::Checked,
        request: &tilo_actions::remote::InvokeRequest,
    ) -> Result<
        tilo_actions::remote::Pending<tilo_action_broker::services::OwnDeviceOutcome>,
        tilo_actions::remote::ActionHostError,
    > {
        let tilo_actions::remote::CallerPrincipal::Person(person) = caller.principal() else {
            return Err(tilo_actions::remote::ActionHostError::NotPermitted);
        };
        self.0
            .invoke_own_device(person, checked, request)
            .map_err(|reason| tilo_actions::remote::ActionHostError::Refused { reason })
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
use tilo_lock::MutexRecover as _;
use tokio::sync::Mutex as AsyncMutex;
/// Speaker groups over the certified personal-device network.
pub struct NativeSpeakers(pub Arc<super::network::ShellNetwork>);
impl tilo_action_broker::services::SpeakerServices for NativeSpeakers {
    fn command(&self, command: runtime_collab::SpeakerCommand) -> runtime_collab::SpeakerReply {
        crate::shell::speaker_settings::handle(&self.0, command)
    }

    fn changed(&self, after: u64) -> Option<tilo_action_broker::services::SpeakerChange> {
        Some(Box::pin(crate::shell::speaker_settings::changed(
            &self.0, after,
        )))
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
            seq,
        } => runtime_collab::WorkspaceControlEvent::Frame {
            surface,
            encoded_h264,
            timestamp,
            seq,
        },
        crate::network::WorkspaceControlUpdate::Closed(reason) => {
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
                    runtime_collab::WorkspaceControlInput::RequestKeyframe { seq } => {
                        let published = this
                            .network
                            .publish_workspace_control_keyframe_request(&session, seq)
                            .map_err(capnp::Error::failed);
                        drop(session);
                        return published;
                    }
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
            &LaunchPlace::Focus,
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
        let now = tilo_clock::unix_now();
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
        assert!(!stranger_copy.app_is_author_at(app, now));

        let mut launch_copy = workspace.clone();
        launcher.grant_authorship("owner", app, &mut launch_copy);
        assert!(launch_copy.app_is_author_at(app, now));
        assert!(launch_copy.launch_grants(now).authors(app));
        let stored = handle.read().desktop.workspaces[0].policy.grants.len();

        let mut again = handle.read().desktop.workspaces[0].clone();
        launcher.grant_authorship("owner", app, &mut again);
        drop(launcher);
        assert_eq!(
            handle.read().desktop.workspaces[0].policy.grants.len(),
            stored,
            "granted once"
        );
    }

    /// Clicking or otherwise opening an app on this machine is the person
    /// here granting authorship, once.
    #[test]
    fn the_person_here_authors_an_app_they_open() {
        let now = tilo_clock::unix_now();
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
        assert!(opened.app_is_author_at("so.tilo.word-processor", now));
        let again = tree
            .workspace_after_opening_app("so.tilo.word-processor")
            .expect("still active");
        assert_eq!(
            again.policy.grants.len(),
            opened.policy.grants.len(),
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
