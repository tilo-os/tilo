//! Member copies: a workspace another person's device serves, held here
//! because this device joined it.
//!
//! A member copy keeps the same stored workspace a governed workspace and a
//! held copy keep, in the same directory ([`super::GovernedWorkspace`]), and
//! its resources travel a resource this device writes is admitted into
//! its document store and answered once it is durable
//! ([`crate::workspace_documents::WorkspaceDocuments`]), and every write the
//! serving device sends is taken into the copy and handed to the apps that follow it.
//! What differs is where its changes come from and go: a member neither
//! governs the workspace nor holds its keys for other devices, so it dials
//! the device its enrollment pinned over the workspace's direct link, as a
//! member device, and sends what it writes there.
//!
//! The device a member copy acts as is the person's own, the one whose
//! invitation that device admitted. It is handed to the service
//! ([`Workspaces::set_member_device`]); a service without one holds no member
//! copies.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use collab_types::signer::DeviceSigner;
use neighbour_wire::NeighbourMsg;
use tilo_delivery_protocol::workspace_join::WorkspaceDeviceHintV1;
use tilo_peer_state::StatePeer;
use tilo_peer_state::direct::DirectSync;
use tilo_peer_state::join::{
    JoinBootstrap, JoinChallengeRequest, JoinCompleteRequest, JoinTransportRequest,
    JoinTransportResponse, TRANSPORT_IROH_WORKSPACE_HOST_ENDPOINT_ADDR_V1, TransportHint,
};
use tilo_peer_state::join_transport::WorkspaceJoinClient;
use tilo_peer_state::owner::session::hosted_by;
use tilo_server_protocol::WorkspaceId;
use tilo_user_identity::DeviceCert;

use super::{GovernedWorkspace, Workspaces, appended, peer_parts};
use crate::workspace_documents::HostCommit;

/// The member copies a device holds: the links of the workspaces it joined,
/// by workspace while each is up, and the device they act as.
#[derive(Default)]
pub(super) struct Members {
    /// The link of each member copy, while it is up.
    links: std::sync::Mutex<std::collections::HashMap<WorkspaceId, DirectSync>>,
    /// The device the copies act as, once the embedder hands it to the
    /// service ([`Workspaces::set_member_device`]).
    device: std::sync::OnceLock<Arc<MemberDevice>>,
    /// Whether each member copy's link is up, by workspace: what a write
    /// waits for before it is published, since a write published before the
    /// serving device listens is in no store of its own.
    up: std::sync::Mutex<std::collections::HashMap<WorkspaceId, tokio::sync::watch::Sender<bool>>>,
}

/// The extension of the file beside a member copy's workspace naming the
/// device it syncs with ([`member_of`]).
const MEMBER: &str = "member";

/// How long after the link ends this device dials that device again.
const REDIAL: std::time::Duration = std::time::Duration::from_secs(5);
/// How long a write waits for the member copy's link before it is refused:
/// the write is undone, as one the owner could not take is.
const REACHED_WITHIN: std::time::Duration = std::time::Duration::from_secs(10);

/// The device a member copy acts as: the person's own, whose key the
/// workspace's serving device admitted when the invitation was redeemed.
#[derive(Clone)]
pub struct MemberDevice {
    /// The certificate the serving device admitted this device with.
    pub cert: DeviceCert,
    /// Signs with the key that certificate names.
    pub signer: Arc<dyn DeviceSigner>,
}

impl MemberDevice {
    /// The device key this device signs as.
    #[must_use]
    pub fn device(&self) -> [u8; 32] {
        self.cert.device_public_key
    }
}

/// The device a member copy syncs with: the address its enrollment pinned,
/// signed by that device itself.
#[derive(Clone, Debug)]
pub(super) struct Member {
    /// That device's signed address, where it is reached.
    addr: iroh_base::EndpointAddr,
}

/// The member copy kept at `path`, by the file beside it: none for a
/// governed workspace, a held copy, or one whose file does not hold up.
pub(super) fn member_of(path: &Path) -> Option<Member> {
    let file = path.with_extension(MEMBER);
    let bytes = match std::fs::read(&file) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return None,
        Err(error) => {
            tracing::warn!(path = %file.display(), %error, "a member copy's device was not read");
            return None;
        }
    };
    match postcard::from_bytes::<TransportHint>(&bytes) {
        Ok(hint) => match WorkspaceDeviceHintV1::from_transport_hint(&hint) {
            Ok(address) => Some(Member {
                addr: address.endpoint_addr,
            }),
            Err(error) => {
                tracing::warn!(path = %file.display(), %error, "a member copy's device is not an address");
                None
            }
        },
        Err(error) => {
            tracing::warn!(path = %file.display(), %error, "a member copy's device does not decode");
            None
        }
    }
}

/// Keep `hint`, the address of the device `path`'s member copy syncs with,
/// beside its workspace.
fn keep_member(path: &Path, hint: &TransportHint) -> Result<(), String> {
    let bytes = postcard::to_stdvec(hint).map_err(|error| error.to_string())?;
    tilo_files::native::atomic::write(&path.with_extension(MEMBER), &bytes)
        .map_err(|error| error.to_string())
}

/// The replica of a member copy: the workspace as that device answered it,
/// with
/// the governance its answer carried. A member writes as its own device,
/// never as the device serving it; the link and the store decide its writes
/// as
/// every member's are decided.
fn replica(bootstrap: &JoinBootstrap, now: web_time::SystemTime) -> Result<StatePeer, String> {
    let mut peer = StatePeer::from_join(
        &bootstrap.workspace_id.to_string(),
        bootstrap.host.clone(),
        bootstrap.certified_roster.clone(),
        &bootstrap.revocations,
        now,
    )?;
    take_governance(&mut peer, bootstrap, now)?;
    if !peer.has_governance() {
        return Err("the workspace was answered without its governance".into());
    }
    Ok(peer)
}

/// Take `bootstrap`'s governance into `peer`, a member copy's replica.
fn take_governance(
    peer: &mut StatePeer,
    bootstrap: &JoinBootstrap,
    now: web_time::SystemTime,
) -> Result<(), String> {
    let origin =
        tilo_peer_state::TransportOrigin::RetainedServerDelta(bootstrap.host.device_public_key);
    for bytes in &bootstrap.governance_messages {
        let message: NeighbourMsg =
            postcard::from_bytes(bytes).map_err(|error| error.to_string())?;
        if matches!(message, NeighbourMsg::AppDocument { .. }) {
            continue;
        }
        peer.receive_from(origin, message, now)
            .map_err(|error| error.to_string())?;
    }
    Ok(())
}

impl GovernedWorkspace {
    /// A member copy of the workspace `bootstrap` answers, kept under `path`:
    /// the device serving it as the answer names it, nothing of its own
    /// published.
    fn adopt_member(
        path: PathBuf,
        workspace: String,
        bootstrap: &JoinBootstrap,
        device: Option<&super::DeviceKeys>,
        now: web_time::SystemTime,
    ) -> Result<Self, String> {
        let peer = replica(bootstrap, now)?;
        let mut parts = peer_parts::PeerParts::open(&path)?;
        let entry = super::WorkspaceDocumentEntry {
            version: super::DOCUMENT_VERSION,
            workspace,
            workspace_id: bootstrap.workspace_id,
            owner: bootstrap.host.clone(),
            peer: parts.stage(&peer)?,
            generation: bootstrap.generation.max(1),
            outgoing: Vec::new(),
            published: 0,
            collaboration: super::collaboration::CollaborationState::default(),
        };
        let (publications, _) = appended::Appended::open(&path, super::PUBLICATIONS, 0)?;
        let (receipts, _) = appended::Appended::open(&path, super::RECEIPTS, 0)?;
        let mut this = Self::assemble(entry, peer, path, parts, (publications, receipts), device)?;
        this.persist()?;
        Ok(this)
    }
}

impl Workspaces {
    /// Take `device` as the device this service's member copies act as: the
    /// person's own, whose key the devices serving the workspaces it joined
    /// admitted.
    pub fn set_member_device(&self, device: Arc<MemberDevice>) {
        if self.inner.member.device.set(device).is_err() {
            tracing::warn!("the service's member device was already set; it is kept");
        }
    }

    /// The device a member copy acts as, when this service serves one.
    pub(super) fn member_identity(&self) -> Result<Arc<MemberDevice>, String> {
        self.inner
            .member
            .device
            .get()
            .cloned()
            .ok_or_else(|| "this service serves no device's member copies".to_owned())
    }

    /// Redeem `invitation` with the device it names and hold the workspace as a
    /// member copy.
    ///
    /// # Errors
    ///
    /// The invitation is not for this device, that device refuses or cannot be
    /// reached, or the copy could not be kept.
    pub async fn join_member(&self, invitation: &str) -> Result<WorkspaceId, String> {
        let device = self.member_identity()?;
        let now_ns = tilo_clock::try_unix_now_ns().ok_or("the clock is before the Unix epoch")?;
        let join = tilo_peer_state::join::decode_and_verify_invitation_text(
            invitation,
            Some(device.cert.user_public_key),
            &tilo_user_identity::RevocationList::new(),
            now_ns,
        )?;
        let workspace = join.grant().workspace_id;
        if workspace.is_nil() {
            return Err("invitation names no valid workspace".into());
        }
        let hint = join
            .grant()
            .transport
            .iter()
            .find(|hint| hint.kind == TRANSPORT_IROH_WORKSPACE_HOST_ENDPOINT_ADDR_V1)
            .cloned()
            .ok_or("invitation has no authenticated native Iroh route")?;
        let (address, _) =
            WorkspaceDeviceHintV1::authenticate_transport_hint(&hint).map_err(|e| e.to_string())?;
        address
            .validate_invitation(join.grant())
            .map_err(|e| e.to_string())?;
        let client = WorkspaceJoinClient::new(self.inner.host.endpoint.clone());
        let request_id: [u8; 32] = rand::random();
        let challenge = match client
            .request(
                address.endpoint_addr.clone(),
                JoinTransportRequest::Challenge(JoinChallengeRequest {
                    signed_invitation: join.signed_invitation().to_vec(),
                    device_cert: device.cert.clone(),
                    request_id,
                }),
            )
            .await
            .map_err(|error| error.to_string())?
        {
            JoinTransportResponse::Challenge(challenge) => challenge.challenge,
            JoinTransportResponse::Error { message, .. } => {
                return Err(format!("the workspace's device refused: {message}"));
            }
            _ => return Err("the host answered no enrollment challenge".into()),
        };
        let proof = tilo_peer_state::join::sign_join_proof(challenge, device.signer.as_ref())?;
        let bootstrap = match client
            .request(
                address.endpoint_addr.clone(),
                JoinTransportRequest::Complete(JoinCompleteRequest { proof }),
            )
            .await
            .map_err(|error| error.to_string())?
        {
            JoinTransportResponse::Complete(bootstrap) => bootstrap,
            JoinTransportResponse::Error { message, .. } => {
                return Err(format!("the workspace's device refused: {message}"));
            }
            _ => return Err("the host did not complete the enrollment".into()),
        };
        if bootstrap.workspace_id != workspace {
            return Err("the device admitted this to another workspace".into());
        }
        self.hold_member(&device, &bootstrap, &hint).await
    }

    /// Hold `bootstrap` as a member copy of the workspace it answers, synced
    /// with the device `hint` names, as `device` certifies. Asked again, the
    /// copy takes in what the answer carries.
    ///
    /// # Errors
    ///
    /// The answer or the address does not hold up, or the copy could not be
    /// kept here.
    pub(super) async fn hold_member(
        &self,
        device: &MemberDevice,
        bootstrap: &JoinBootstrap,
        hint: &TransportHint,
    ) -> Result<WorkspaceId, String> {
        let (address, _) =
            WorkspaceDeviceHintV1::authenticate_transport_hint(hint).map_err(|e| e.to_string())?;
        if hosted_by(hint, &device.cert.device_public_key) {
            return Err("this device's own service enters it".into());
        }
        address
            .validate_bootstrap(bootstrap)
            .map_err(|e| e.to_string())?;
        if address.workspace_id != bootstrap.workspace_id {
            return Err("the address names another workspace".into());
        }
        let workspace = bootstrap.workspace_id;
        let name = workspace.to_string();
        let now = tilo_clock::wall();
        let held = self.inner.workspaces.lock().await.get(&name).cloned();
        let state = if let Some(held) = held {
            let governed = held.lock().await;
            if governed.member.is_none() {
                return Err("this device governs that workspace here".into());
            }
            let mut peer = governed.peer().clone();
            take_governance(&mut peer, bootstrap, now)?;
            *governed.peer.lock() = peer;
            drop(governed);
            held
        } else {
            let path = self.inner.dir.join(format!("{name}.workspace"));
            let keys = self.device_keys();
            let mut governed = if path.exists() {
                let governed = GovernedWorkspace::open(path.clone(), keys.as_ref())?;
                let mut peer = governed.peer().clone();
                take_governance(&mut peer, bootstrap, now)?;
                *governed.peer.lock() = peer;
                governed
            } else {
                GovernedWorkspace::adopt_member(
                    path.clone(),
                    name.clone(),
                    bootstrap,
                    keys.as_ref(),
                    now,
                )?
            };
            governed.member = Some(Member {
                addr: address.endpoint_addr.clone(),
            });
            governed.persist()?;
            if let Err(error) = keep_member(&governed.path, hint) {
                tracing::warn!(%name, %error, "a member copy's device address was not kept");
            }
            self.bind_member(&governed).await?;
            let state = Arc::new(tokio::sync::Mutex::new(governed));
            self.inner
                .workspaces
                .lock()
                .await
                .insert(name.clone(), Arc::clone(&state));
            state
        };
        self.serve_member(&state, device)?;
        tracing::info!(%name, workspace_id = %workspace, "holding a workspace this device joined as a member");
        Ok(workspace)
    }

    /// Whether `workspace` is held here as a member copy of another device.
    ///
    /// # Errors
    ///
    /// The workspace could not be read now.
    pub async fn is_member(&self, workspace: &str) -> Result<bool, String> {
        let state = self.inner.workspaces.lock().await.get(workspace).cloned();
        let Some(state) = state else {
            return Ok(false);
        };
        Ok(state.lock().await.member.is_some())
    }

    /// Keep `commit`, a write this device makes to `workspace`, a member
    /// copy: admitted into the copy's documents in this device's name, and
    /// sent to the device serving it, which fans it out to the workspace's
    /// other members.
    ///
    /// # Errors
    ///
    /// The workspace is not a member copy held here, or the write could not be
    /// kept.
    pub async fn commit_member_document(
        &self,
        workspace: &str,
        commit: HostCommit,
    ) -> Result<u64, String> {
        let workspace_id = WorkspaceId::parse(workspace).ok_or("not a workspace id")?;
        if !self.is_member(workspace).await? {
            return Err("this workspace is not held here as a member copy".into());
        }
        let device = self.member_identity()?;
        let mut up = self.member_up(workspace_id)?;
        tokio::time::timeout(REACHED_WITHIN, up.wait_for(|up| *up))
            .await
            .map_err(|_| "the device serving the workspace was not reached in time")?
            .map_err(|_| "the member copy's link closed")?;
        let body = commit.body.clone();
        let order = self
            .inner
            .host
            .records
            .carry(workspace_id, device.device(), commit)
            .await?
            .durable()
            .await?;
        let message: NeighbourMsg =
            postcard::from_bytes(&body).map_err(|error| error.to_string())?;
        if let Some(link) = self.member_link(workspace_id)
            && let Err(error) = link.send(message)
        {
            tracing::debug!(%workspace, %error, "a member write did not reach the serving device");
        }
        Ok(order)
    }

    /// Write `command`, this device's own membership change (a lease it
    /// claims, a setting it saves), into `workspace`'s member copy and answer
    /// the write to commit and send to the device serving it.
    ///
    /// # Errors
    ///
    /// The workspace is not a member copy here, this device may not make the
    /// change, or it could not be signed.
    pub async fn member_membership_write(
        &self,
        workspace: &str,
        command: tilo_peer_state::workspace_membership::WorkspaceMembershipCommand,
    ) -> Result<HostCommit, String> {
        if !self.is_member(workspace).await? {
            return Err("this workspace is not held here as a member copy".into());
        }
        let device = self.member_identity()?;
        let state = self.workspace(workspace).await.map_err(|e| e.to_string())?;
        let governed = state.lock().await;
        let now = tilo_clock::wall();
        let generation = governed.record.generation.saturating_add(1);
        let peer = governed.peer();
        let mut candidate = peer.clone();
        candidate
            .workspace_mut()
            .apply_workspace_membership_command(
                &device.cert,
                &command,
                device.signer.as_ref(),
                now,
            )?;
        let messages = candidate.publication_mut().durable_mutation_messages(
            &peer,
            device.signer.as_ref(),
            now,
            generation,
        )?;
        drop(peer);
        *governed.peer.lock() = candidate;
        let message = messages
            .into_iter()
            .next()
            .ok_or("the membership change wrote nothing")?;
        let (channel, retention) = neighbour_wire::retention(&message, false)
            .ok_or("the write carries no retention route")?;
        let body = postcard::to_stdvec(&message).map_err(|error| error.to_string())?;
        drop(governed);
        Ok(HostCommit {
            body,
            channel,
            checkpoint: retention == neighbour_wire::Retention::Checkpoint,
        })
    }

    /// Ticks true when `workspace`'s member copy is linked to the device
    /// serving it, and false when that link ends.
    fn member_up(
        &self,
        workspace: WorkspaceId,
    ) -> Result<tokio::sync::watch::Receiver<bool>, String> {
        self.inner
            .member
            .up
            .lock()
            .map_err(|_| "the member copies closed".to_owned())?
            .get(&workspace)
            .map(tokio::sync::watch::Sender::subscribe)
            .ok_or_else(|| "the workspace is not held here as a member copy".to_owned())
    }

    /// The link of `workspace`'s member copy, while it is up.
    fn member_link(&self, workspace: WorkspaceId) -> Option<DirectSync> {
        self.inner
            .member
            .links
            .lock()
            .ok()
            .and_then(|links| links.get(&workspace).cloned())
    }

    /// Open every workspace recorded under `dir`, bind each to the service,
    /// and answer them by their names: a member copy is bound to this
    /// device's store, every other workspace served to its members.
    ///
    /// # Errors
    ///
    /// A workspace or a member copy's documents could not be opened.
    pub(super) async fn open_kept(
        &self,
        dir: &Path,
    ) -> Result<
        std::collections::BTreeMap<String, Arc<tokio::sync::Mutex<GovernedWorkspace>>>,
        String,
    > {
        let device = self.device_keys();
        let mut workspaces = std::collections::BTreeMap::new();
        let entries = std::fs::read_dir(dir).map_err(|error| error.to_string())?;
        for entry in entries {
            let path = entry.map_err(|error| error.to_string())?.path();
            // A workspace this service released stays on disk, unserved.
            if super::delete::passed_over(dir, &self.inner.direct, &path)
                || path.with_extension(super::RELEASED).exists()
            {
                continue;
            }
            let workspace = GovernedWorkspace::open(path.clone(), device.as_ref())
                .map_err(|error| super::document_format::unopened(&path, dir, &error))?;
            let name = workspace.workspace().to_owned();
            let state = if workspace.member.is_some() {
                self.kept_member(workspace).await?
            } else {
                self.serve_direct(&workspace)?;
                Arc::new(tokio::sync::Mutex::new(workspace))
            };
            workspaces.insert(name, state);
        }
        Ok(workspaces)
    }

    /// Keep `governed`, a member copy this service opened, bound and ready to
    /// sync: it keeps this device's own resources, and is served to no one.
    ///
    /// # Errors
    ///
    /// Its document store could not be opened.
    pub(super) async fn kept_member(
        &self,
        governed: GovernedWorkspace,
    ) -> Result<Arc<tokio::sync::Mutex<GovernedWorkspace>>, String> {
        self.bind_member(&governed).await?;
        Ok(Arc::new(tokio::sync::Mutex::new(governed)))
    }

    /// The workspaces this service serves to their members: every kept copy
    /// but the member copies, which sync with the device serving them.
    pub(super) async fn served_kept(&self) -> Vec<Arc<tokio::sync::Mutex<GovernedWorkspace>>> {
        let states = self
            .inner
            .workspaces
            .lock()
            .await
            .values()
            .cloned()
            .collect::<Vec<_>>();
        let mut served = Vec::with_capacity(states.len());
        for state in states {
            if state
                .try_lock()
                .is_ok_and(|governed| governed.member.is_none())
            {
                served.push(state);
            }
        }
        served
    }

    /// Sync every member copy this service opened with the device serving it.
    pub(super) async fn serve_kept_members(&self) {
        let members = self
            .inner
            .workspaces
            .lock()
            .await
            .values()
            .cloned()
            .collect::<Vec<_>>();
        let device = match self.member_identity() {
            Ok(device) => device,
            Err(error) => {
                tracing::warn!(%error, "member copies are kept here but not synced");
                return;
            }
        };
        for state in members {
            if state
                .try_lock()
                .is_ok_and(|governed| governed.member.is_some())
                && let Err(error) = self.serve_member(&state, &device)
            {
                tracing::warn!(%error, "a member copy is not synced with its device");
            }
        }
    }

    /// Bind `governed`, a member copy, to the service: its document store
    /// serves this device's apps, and its changes are this device's own, as a
    /// held copy's are.
    pub(super) async fn bind_member(&self, governed: &GovernedWorkspace) -> Result<(), String> {
        let services = &self.inner.host;
        let workspace_id = governed.workspace_id();
        let open = services.workspaces.lock().await.contains_key(&workspace_id);
        if !open {
            let store = crate::store::WorkspaceStore::open_with_payloads(
                services.access.state_root(),
                workspace_id,
                true,
                services.access.shell_payloads().clone(),
            )
            .map_err(|error| error.to_string())?;
            if store
                .load_snapshot()
                .map_err(|error| error.to_string())?
                .is_none()
            {
                let baseline = governed
                    .peer()
                    .admitted_workspace()
                    .ok_or("workspace governance missing")?
                    .clone()
                    .save();
                store
                    .save_snapshot(&baseline)
                    .map_err(|error| error.to_string())?;
            }
        }
        let runtime_state = crate::workspace_io::get_or_initialize(
            &services.workspaces,
            services.access.state_root().to_path_buf(),
            services.access.shell_payloads(),
            workspace_id,
            crate::workspace_access::AdmissionMode::Governed,
            crate::peer::MAX_HELD_WORKSPACES,
        )
        .await
        .map_err(|error| error.to_string())?;
        runtime_state.lock().await.governs_here = false;
        Ok(())
    }

    /// Sync `governed`, a member copy, with the device serving it, as `device`:
    /// take in what that device sends, and answer it as a member answers.
    pub(super) fn serve_member(
        &self,
        state: &Arc<tokio::sync::Mutex<GovernedWorkspace>>,
        device: &MemberDevice,
    ) -> Result<(), String> {
        let governed = state
            .try_lock()
            .map_err(|_| "the workspace is held".to_owned())?;
        let member = governed
            .member
            .clone()
            .ok_or("the workspace is not a member copy")?;
        let workspace_id = governed.workspace_id();
        let name = governed.workspace().to_owned();
        let link = DirectSync::over(governed.peer.clone(), Arc::clone(&device.signer))
            .with_store(Arc::clone(&self.inner.store));
        drop(governed);
        if let Ok(mut links) = self.inner.member.links.lock() {
            links.insert(workspace_id, link.clone());
        }
        self.follow_member(&name, &link);
        let (up, waiting) = tokio::sync::watch::channel(false);
        drop(waiting);
        if let Ok(mut ups) = self.inner.member.up.lock() {
            ups.insert(workspace_id, up.clone());
        }
        let endpoint = self.inner.host.endpoint.clone();
        let serving = device.device();
        tokio::spawn(async move {
            loop {
                match link.dial(&endpoint, member.addr.clone(), serving).await {
                    Ok(dialed) => {
                        up.send_replace(true);
                        if let Err(refusal) = dialed.run(&link).await {
                            tracing::debug!(%name, ?refusal, "a member copy's link ended");
                        }
                        up.send_replace(false);
                    }
                    Err(error) => {
                        tracing::debug!(%name, %error, "a member copy's device is unavailable");
                    }
                }
                tokio::time::sleep(REDIAL).await;
            }
        });
        Ok(())
    }

    /// Read what the serving device sends a member copy: every resource write it
    /// carries is taken into the copy's documents in its author's name and
    /// handed to the apps following it; group governance goes to this device's
    /// key service; content pieces and asks are handled as the workspace's link
    /// handles them.
    fn follow_member(&self, workspace: &str, link: &DirectSync) {
        let name = workspace.to_owned();
        let workspaces = self.downgrade();
        let mut reader = link.reader();
        let link = link.clone();
        tokio::spawn(async move {
            while let Some(frame) = reader.recv().await {
                let Some(workspaces) = workspaces.upgrade() else {
                    return;
                };
                let answering = matches!(
                    frame.message,
                    NeighbourMsg::DocumentContentPiece { .. }
                        | NeighbourMsg::DocumentContentLacks { .. }
                );
                let asking = matches!(frame.message, NeighbourMsg::DocumentContentWant { .. });
                let Ok(body) = postcard::to_allocvec(&frame.message) else {
                    continue;
                };
                if frame.fresh
                    && frame.report.is_ok()
                    && let Err(error) = workspaces
                        .keep_member_write(&name, frame.author, &frame.message, &body)
                        .await
                {
                    tracing::warn!(%name, %error, "a member copy did not keep what the serving device sent");
                }
                if answering {
                    let (workspaces, name, author) =
                        (workspaces.clone(), name.clone(), frame.author);
                    let body = body.clone();
                    tokio::task::spawn_blocking(move || {
                        workspaces.content_for_host(&name, author, &body);
                    });
                    continue;
                }
                if !asking {
                    continue;
                }
                for answer in workspaces.content_answer(&name, &frame.author, &body) {
                    let Ok(message) = tilo_failure::ignored!(
                        "a content answer this device made itself does not decode",
                        postcard::from_bytes::<NeighbourMsg>(&answer)
                    ) else {
                        continue;
                    };
                    if let Err(error) = link.send(message) {
                        tracing::debug!(%name, %error, "a member's content answer did not reach the serving device");
                    }
                }
            }
        });
    }

    /// Take `message`, `author`'s frame the serving device sent this member
    /// copy, into
    /// it: a resource write into the copy's documents and the apps following
    /// it, group governance into this device's key service.
    async fn keep_member_write(
        &self,
        workspace: &str,
        author: [u8; 32],
        message: &NeighbourMsg,
        body: &[u8],
    ) -> Result<(), String> {
        if message.governance_workspace().is_some() {
            if let Some(sink) = self
                .inner
                .host
                .channels
                .as_ref()
                .and_then(|channels| channels.sink.clone())
            {
                sink.take(vec![message.clone()]);
            }
            return Ok(());
        }
        let NeighbourMsg::AppDocument {
            app_id,
            document_id,
            ..
        } = message
        else {
            return Ok(());
        };
        let Some((route, retention)) = neighbour_wire::retention(message, false) else {
            return Ok(());
        };
        let state = self.workspace(workspace).await.map_err(|e| e.to_string())?;
        let governed = state.lock().await;
        let followed = governed.follow(author, message.clone());
        if let Some(reason) = followed.refused {
            tracing::debug!(%workspace, %reason, "a member copy refused a write the serving device carried");
        }
        self.tell_followers(&governed, (app_id.clone(), document_id.clone()), message)
            .await;
        let workspace_id = governed.workspace_id();
        drop(governed);
        self.inner
            .host
            .records
            .carry(
                workspace_id,
                author,
                HostCommit {
                    body: body.to_vec(),
                    channel: route,
                    checkpoint: retention == neighbour_wire::Retention::Checkpoint,
                },
            )
            .await?
            .durable()
            .await?;
        Ok(())
    }
}

#[cfg(test)]
#[path = "member_tests.rs"]
mod tests;
