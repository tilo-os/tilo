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

use collab_types::SignedOp;
use collab_types::signer::DeviceSigner;
use neighbour_wire::NeighbourMsg;
use tilo_delivery_protocol::workspace_join::WorkspaceDeviceHintV1;
use tilo_peer_state::MissingPrerequisite;
use tilo_peer_state::StatePeer;
use tilo_peer_state::TransportOrigin;
use tilo_peer_state::direct::{Answered, DirectSync, Kept};
use tilo_peer_state::join::{
    JoinBootstrap, JoinChallengeRequest, JoinCompleteRequest, JoinTransportRequest,
    JoinTransportResponse, Role, TRANSPORT_IROH_WORKSPACE_HOST_ENDPOINT_ADDR_V1, TransportHint,
};
use tilo_peer_state::join_transport::WorkspaceJoinClient;
use tilo_peer_state::owner::WriteRefusal;
use tilo_peer_state::owner::session::hosted_by;
use tilo_server_protocol::WorkspaceId;
use tilo_server_protocol::refusal::Refusal;
use tilo_user_identity::DeviceCert;

use super::actions::NAME;
use super::{GovernedWorkspace, Held, Workspaces, appended, peer_parts};

/// How long naming the person in a workspace they just joined may take.
const NAMING_WITHIN: std::time::Duration = std::time::Duration::from_secs(15);
use crate::workspace_documents::HostCommit;

/// The member copies a device holds: the links of the workspaces it joined,
/// by workspace while each is up, and the device they act as.
pub(super) struct Members {
    /// The link of each member copy, while it is up.
    links: std::sync::Mutex<std::collections::HashMap<WorkspaceId, DirectSync>>,
    /// What syncs each member copy now. A copy held again is synced by its new
    /// enrollment alone.
    syncing: std::sync::Mutex<std::collections::HashMap<WorkspaceId, MemberSyncing>>,
    /// Held while a copy is taken in: a workspace joined while the session holds what it took
    /// part in before is kept once.
    holding: tokio::sync::Mutex<()>,
    /// The device the copies act as, once the embedder hands it to the
    /// service ([`Workspaces::set_member_device`]).
    device: std::sync::OnceLock<Arc<MemberDevice>>,
    /// Whether each member copy's link is up, by workspace: what a write
    /// waits for before it is published, since a write published before the
    /// serving device listens is in no store of its own.
    up: std::sync::Mutex<std::collections::HashMap<WorkspaceId, tokio::sync::watch::Sender<bool>>>,
    /// This device's writes made while the serving device was away, by workspace, in the order
    /// they were made: sent the moment a link is up, before anything written after them.
    away: std::sync::Mutex<std::collections::HashMap<WorkspaceId, Vec<NeighbourMsg>>>,
    /// What runs admitted to each member copy are pinned to, by workspace: the
    /// serving device's address and how often the copy was enrolled, read
    /// without waiting by the check every admitted call passes.
    materials: std::sync::Mutex<std::collections::HashMap<WorkspaceId, (TransportHint, u64)>>,
    /// Told each time a workspace is held here as a member copy.
    held: Arc<tokio::sync::Notify>,
    /// Each write of another device a workspace held here took in, governed
    /// or a member copy: the embedder hands it to the apps following the
    /// workspace's records ([`Workspaces::admitted_writes`]).
    pub(super) writes: tokio::sync::broadcast::Sender<AdmittedWrite>,
    /// Each write this device made to a member copy that the serving device
    /// has not answered yet, by workspace: what [`Workspaces::served`] waits
    /// on, and what a link that caught up asks again.
    served: std::sync::Mutex<std::collections::HashMap<WorkspaceId, ServedWrites>>,
    /// Told whenever a served write is noted, answered or refused: a `served`
    /// that waits subscribes before it reads, and reads again after each tick.
    served_changed: tokio::sync::watch::Sender<u64>,
}

impl Default for Members {
    fn default() -> Self {
        Self {
            links: std::sync::Mutex::default(),
            syncing: std::sync::Mutex::default(),
            holding: tokio::sync::Mutex::default(),
            device: std::sync::OnceLock::default(),
            up: std::sync::Mutex::default(),
            away: std::sync::Mutex::default(),
            materials: std::sync::Mutex::default(),
            held: Arc::default(),
            writes: tokio::sync::broadcast::channel(WRITES_HELD).0,
            served: std::sync::Mutex::default(),
            served_changed: tokio::sync::watch::channel(0).0,
        }
    }
}

/// How many admitted writes an embedder that has not read them yet is told of
/// before the oldest is dropped.
const WRITES_HELD: usize = 1024;

/// Another device's write a workspace held here took in, as the workspace
/// took it: what this device's apps are shown of it.
#[derive(Clone, Debug)]
pub struct AdmittedWrite {
    /// The workspace the write is of.
    pub workspace: WorkspaceId,
    /// The device that wrote it.
    pub author: [u8; 32],
    /// The write, as the workspace decoded and admitted it.
    pub message: Arc<NeighbourMsg>,
    /// The author's access in the workspace, as its governance gave it when
    /// the write was taken in.
    pub access: tilo_action_policy::MemberAccess,
    /// Whether the write is history the workspace already held, which this
    /// device catches up on (written before it arrived, or while it was
    /// away), rather than a write made just now: history is old by nature,
    /// so only a live write is held to the clock window.
    pub history: bool,
}

/// The tasks that sync a member copy.
struct MemberSyncing {
    /// Its link's dialling.
    dialling: tokio::task::AbortHandle,
    /// The reader of what the serving device sends.
    following: tokio::task::AbortHandle,
    /// The wait for the group's keys, which takes in what the copy kept before it held them.
    keying: Option<tokio::task::AbortHandle>,
    /// What the serving device answers of this device's writes there
    /// ([`Workspaces::answer_member_writes`]).
    answering: tokio::task::AbortHandle,
}

impl MemberSyncing {
    fn abort(&self) {
        self.dialling.abort();
        self.following.abort();
        if let Some(keying) = &self.keying {
            keying.abort();
        }
        self.answering.abort();
    }
}

impl Members {
    /// Take `device` as the device the member copies act as.
    pub(super) fn set_device(&self, device: Arc<MemberDevice>) {
        if self.device.set(device).is_err() {
            tracing::warn!("the service's member device was already set; it is kept");
        }
    }

    /// A receiver told whenever a served write is noted, answered or refused.
    pub(super) fn answers_changed(&self) -> tokio::sync::watch::Receiver<u64> {
        self.served_changed.subscribe()
    }

    /// Note the write `message`, named by the hash `write` of its frame, as one of this device's
    /// to `app`'s `document` of `workspace` that the device serving it has not answered yet.
    fn note_served(
        &self,
        workspace: WorkspaceId,
        write: [u8; 32],
        (app, document): (&str, &str),
        message: &NeighbourMsg,
    ) {
        let Ok(mut served) = self.served.lock() else {
            return;
        };
        served.entry(workspace).or_default().waiting.insert(
            write,
            WaitingWrite {
                app: app.to_owned(),
                document: document.to_owned(),
                message: message.clone(),
            },
        );
        drop(served);
        self.served_changed
            .send_modify(|tick| *tick = tick.wrapping_add(1));
    }

    /// What the device serving `workspace` answered of one of this device's writes there.
    fn answered_served(&self, workspace: WorkspaceId, answered: Answered) {
        let Ok(mut served) = self.served.lock() else {
            return;
        };
        let Some(writes) = served.get_mut(&workspace) else {
            return;
        };
        let Some(answered_write) = writes.waiting.remove(&answered.write) else {
            return;
        };
        if matches!(answered.kept, Kept::Refused { .. }) {
            // A write the workspace refused for good is no write here either: the next `served`
            // over its document says so, once, and the caller decides what to do about it.
            writes.refused.insert(
                answered.write,
                (answered_write.app, answered_write.document),
            );
        }
        drop(served);
        self.served_changed
            .send_modify(|tick| *tick = tick.wrapping_add(1));
    }

    /// The writes this device made to `workspace` that no serving device has answered yet.
    fn served_waiting(&self, workspace: WorkspaceId) -> Vec<NeighbourMsg> {
        let Ok(served) = self.served.lock() else {
            return Vec::new();
        };
        served
            .get(&workspace)
            .map(|writes| {
                writes
                    .waiting
                    .values()
                    .map(|waiting| waiting.message.clone())
                    .collect()
            })
            .unwrap_or_default()
    }
}

/// A write this device made to a member copy, before the device serving the workspace answers it.
#[derive(Clone)]
struct WaitingWrite {
    app: String,
    document: String,
    /// The frame this device sent, kept to be sent once more after a link catches up: an answer
    /// can be lost with the link it was written on.
    message: NeighbourMsg,
}

/// The writes of this device a member copy's serving device has not answered yet, and those it
/// refused.
#[derive(Default)]
struct ServedWrites {
    /// By the hash of the write's frame.
    waiting: std::collections::BTreeMap<[u8; 32], WaitingWrite>,
    /// The writes it refused for good, by their frame's hash: each is said once, to the next
    /// [`Workspaces::served`] over its document.
    refused: std::collections::BTreeMap<[u8; 32], (String, String)>,
}

impl ServedWrites {
    /// The writes of `documents` the serving device has not answered yet.
    fn waiting_of(&self, documents: &[(String, String)]) -> std::collections::BTreeSet<[u8; 32]> {
        self.waiting
            .iter()
            .filter(|(_, waiting)| {
                documents
                    .iter()
                    .any(|(app, document)| app == &waiting.app && document == &waiting.document)
            })
            .map(|(write, _)| *write)
            .collect()
    }

    /// Why the serving device refused a write of `documents`, said once: the refusal is no longer
    /// this copy's to say after.
    fn take_refusal(&mut self, documents: &[(String, String)]) -> Option<String> {
        let write = self.refused.iter().find_map(|(write, (app, document))| {
            documents
                .iter()
                .any(|(wanted_app, wanted_document)| {
                    app == wanted_app && document == wanted_document
                })
                .then_some(*write)
        })?;
        let (app, document) = self.refused.remove(&write)?;
        Some(format!(
            "the device serving the workspace refused this device's write to {app}/{document}"
        ))
    }
}

/// The extension of the file beside a member copy's workspace naming the
/// device it syncs with ([`member_of`]).
pub(super) const MEMBER: &str = "member";

/// How long after the link ends this device dials that device again.
/// How long one workspace's member copy is waited for as the device takes it in
/// ([`Workspaces::hold_as_members`]).
const HOLD_WITHIN: std::time::Duration = std::time::Duration::from_secs(30);

const REDIAL: std::time::Duration = std::time::Duration::from_secs(5);
/// How long a write waits for the member copy's link before it is refused:
/// the write is undone, as one the owner could not take is.
#[cfg(any(feature = "apps", feature = "agents", test))]
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

/// A link to the device serving a member copy, at `addr` from `endpoint`, on which this device
/// proved itself as `device` and that device as `host`, its pinned certificate: the first thing a
/// serving device's workspace link asks, before the dial names the workspace.
async fn proved_link(
    endpoint: &iroh::Endpoint,
    addr: &iroh_base::EndpointAddr,
    device: &MemberDevice,
    host: &DeviceCert,
) -> Result<Arc<dyn tilo_peer_state::transport::Transport>, Refusal> {
    use tilo_server_client::link::{Dialer, Purpose};
    let dialer = Dialer::from_endpoint(
        tilo_server_client::host::ServerAddress::at(addr.clone()),
        endpoint.clone(),
    );
    let link = dialer
        .link(Purpose::Records)
        .await
        .map_err(|error| Refusal::Other(error.to_string()))?;
    link.prove_member(&device.cert, device.signer.as_ref(), host)
        .await
        .map_err(|error| match error {
            // The host says, under its own signature, that it admits nobody of this person: no
            // workspace it serves has them, this one among them.
            tilo_server_client::LinkError::Refused(code)
                if code == tilo_server_protocol::auth::HelloRefusal::NotAdmitted.code() =>
            {
                Refusal::NotAMember(code)
            }
            error => Refusal::Other(error.to_string()),
        })?;
    Ok(Arc::new(link))
}

/// The device a member copy syncs with: the address its enrollment pinned,
/// signed by that device itself.
#[derive(Clone, Debug)]
pub(super) struct Member {
    /// That device's signed address, where it is reached.
    addr: iroh_base::EndpointAddr,
    /// The hint the address came from, which a run admitted here is pinned
    /// to.
    hint: TransportHint,
    /// How often this copy was enrolled: every admission of this device,
    /// counted, so a run admitted under one enrollment ends when another
    /// replaces it.
    continuity: u64,
}

/// The material a run admitted to a member copy is pinned to: the device the
/// copy acts as, the device serving it, and the enrollment that admitted this
/// device there.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MemberMaterial {
    /// The admitted device certificate.
    pub cert: DeviceCert,
    /// The serving device's signed address.
    pub hint: TransportHint,
    /// How often the copy was enrolled.
    pub continuity: u64,
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
                hint,
                continuity: 0,
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
            NeighbourMsg::decode(bytes).map_err(|error| error.to_string())?;
        if matches!(message, NeighbourMsg::AppDocument { .. }) {
            continue;
        }
        peer.receive_from(origin, message, now)
            .map_err(|error| error.to_string())?;
    }
    // The workspace's governance came with the answer this copy is taken in from: what this
    // device decides for others reads it, and whoever follows that is woken for it as the device
    // keeping the workspace wakes them for its own writes. Whoever follows it (the key pump,
    // the apps' brokers) comes with the device's keys.
    #[cfg(feature = "device-keys")]
    tilo_identity::policy_change::announce();
    Ok(())
}

impl GovernedWorkspace {
    /// Take `message`, a document write the device serving this member copy sent, into the copy:
    /// a document it holds is changed, and one it holds nothing of is founded by the whole
    /// document its serving device writes. Answers whether the copy changed.
    fn take_served(&self, author: [u8; 32], message: &NeighbourMsg) -> bool {
        if !matches!(message, NeighbourMsg::AppDocument { .. }) {
            return false;
        }
        let mut peer = self.peer();
        match peer.receive_from(
            TransportOrigin::RetainedServerDelta(author),
            message.clone(),
            tilo_clock::wall(),
        ) {
            Ok(seen) => seen.changed || seen.metadata_changed,
            Err(error) => {
                tracing::debug!(
                    %error,
                    "a member copy did not take in what its serving device wrote"
                );
                false
            }
        }
    }

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
        self.inner.member.set_device(device);
    }

    /// Save `change` to the shortcuts of `workspace` as this device's person: a shortcut it
    /// saves is signed by the device this service signs its own writes as
    /// ([`Self::signing_device`]), so its triggers fire for that person as one they saved.
    ///
    /// # Errors
    /// The shortcut could not be signed, or the workspace did not take the change.
    pub async fn change_own_shortcuts(
        &self,
        workspace: &str,
        mut change: tilo_automation::ShortcutChange,
    ) -> Result<(), String> {
        if let tilo_automation::ShortcutChange::Save(shortcut) = &mut change {
            let device = self.signing_device();
            tilo_peer_state::workspace::sign_saved_shortcut(
                workspace,
                shortcut,
                &device.cert,
                device.signer.as_ref(),
            )?;
        }
        self.commit_own_membership(
            workspace,
            tilo_peer_state::workspace_membership::WorkspaceMembershipCommand::Settings(
                tilo_peer_state::workspace_membership::SettingsChange::Shortcuts(change),
            ),
            None,
        )
        .await
        .map(drop)
        .map_err(|error| error.to_string())
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

    /// The device this service signs its own roles' writes as: the device its member copies
    /// act as where its embedder keeps one, else this device itself, which governs the
    /// workspaces it serves.
    pub(crate) fn signing_device(&self) -> Arc<MemberDevice> {
        if let Some(device) = self.inner.member.device.get().cloned() {
            return device;
        }
        Arc::new(MemberDevice {
            cert: self.inner.host.cert.clone(),
            signer: Arc::new(self.inner.host.key.clone()),
        })
    }

    /// Redeem `invitation` with the device it names and hold the workspace as a
    /// member copy.
    ///
    /// # Errors
    ///
    /// The invitation is not for this device, that device refuses or cannot be
    /// reached, or the copy could not be kept.
    pub async fn join_member(&self, invitation: &str) -> Result<WorkspaceId, String> {
        self.join_member_as(invitation, None).await
    }

    /// [`Self::join_member`], and then name the person `name` among the workspace's people
    /// (else the name the invitation gave them): what its member list and everyone's lists of
    /// people show for them, rather than this device's label. Asked of the device that let them
    /// in as the workspace's governance operation, over the device action service, signed as
    /// this device; a workspace that does not answer keeps the name it admitted them with.
    ///
    /// # Errors
    ///
    /// As [`Self::join_member`].
    pub async fn join_member_as(
        &self,
        invitation: &str,
        name: Option<&str>,
    ) -> Result<WorkspaceId, String> {
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
        // A device that holds the workspace's keys, where the invitation names one: only such a
        // device lets a new member in.
        let routes: Vec<_> = join
            .grant()
            .transport
            .iter()
            .filter(|hint| hint.kind == TRANSPORT_IROH_WORKSPACE_HOST_ENDPOINT_ADDR_V1)
            .filter_map(|hint| {
                let (address, _) = WorkspaceDeviceHintV1::authenticate_transport_hint(hint).ok()?;
                Some((hint.clone(), address))
            })
            .collect();
        let (hint, address) = routes
            .iter()
            .find(|(_, address)| address.holds_keys)
            .or_else(|| routes.first())
            .cloned()
            .ok_or("invitation has no authenticated native Iroh route")?;
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
        // The person's own computer asked for this join when it redeemed the invitation: theirs
        // to write into their list.
        let joined = self.hold_member(&device, &bootstrap, &hint, true).await?;
        let name = name
            .or_else(|| join.grant().invitee_name.as_deref())
            .map(str::trim)
            .filter(|name| !name.is_empty());
        if let Some(name) = name
            && let Err(why) = self.name_at(&address.endpoint_addr, joined, name).await
        {
            tracing::debug!(%why, "the person is not named in the workspace they joined");
        }
        Ok(joined)
    }

    /// Ask the device at `addr` to name this device's person `name` in `workspace`.
    async fn name_at(
        &self,
        addr: &iroh::EndpointAddr,
        workspace: WorkspaceId,
        name: &str,
    ) -> Result<(), String> {
        use tilo_actions::remote::{InvokeArgument, InvokeRequest, RemoteActionTarget};
        let named = workspace.to_string();
        let remote = Arc::new(self.action_service().remote(
            &self.inner.host.endpoint,
            addr,
            &named,
        ));
        let mut arguments = serde_json::Map::new();
        arguments.insert(
            "name".to_owned(),
            serde_json::Value::String(name.to_owned()),
        );
        let request = InvokeRequest {
            invocation: tilo_actions::InvocationId::new(),
            target: RemoteActionTarget {
                provider: tilo_actions::remote::WORKSPACE_GOVERNANCE_PROVIDER.to_owned(),
                address: tilo_actions::remote::workspace_governance_address(&named, NAME),
            },
            argument: InvokeArgument::Json(arguments),
            context: None,
            confirmation: None,
            revision: None,
            on_behalf_of: None,
            wait_ms: None,
            appearance: None,
            signed: None,
        };
        match remote.invoke_until_settled(request, NAMING_WITHIN).await {
            Some(Ok(tilo_actions::InvocationUpdate::Succeeded { .. })) => Ok(()),
            Some(Ok(tilo_actions::InvocationUpdate::Failed { error, .. })) => Err(error.message),
            Some(Ok(_)) => Err("no answer in time".to_owned()),
            Some(Err(error)) => Err(error.to_string()),
            None => Err("the device is not reached".to_owned()),
        }
    }

    /// Hold the workspace `bootstrap` answers as a member copy, synced with the device `hint`
    /// names: what this device's session hands its service once the serving device admitted this
    /// device, by an invitation, an entry or as a further device of the person. Answers the
    /// workspace.
    ///
    /// # Errors
    ///
    /// This service acts as no device, the address `hint` carries is not the workspace's, this
    /// device's own service serves the workspace, or the copy could not be kept.
    pub async fn join_admitted(
        &self,
        bootstrap: &JoinBootstrap,
        hint: &TransportHint,
    ) -> Result<WorkspaceId, String> {
        let device = self.member_identity()?;
        self.hold_member(&device, bootstrap, hint, false).await
    }

    /// Hold `workspace`, which this device is a member of, as a member copy synced with the
    /// device `hint` names: this device proves its membership there and takes the workspace as
    /// that device answers it. What a device's session asks for each workspace it takes part in
    /// that its service does not hold yet, such as one it joined before its service held the
    /// workspaces it joins.
    ///
    /// # Errors
    ///
    /// As [`Self::join_admitted`], or the device `hint` names refuses this device's membership or
    /// cannot be reached.
    pub async fn hold_as_member(
        &self,
        workspace: WorkspaceId,
        hint: &TransportHint,
    ) -> Result<WorkspaceId, String> {
        let device = self.member_identity()?;
        let (address, _) =
            WorkspaceDeviceHintV1::authenticate_transport_hint(hint).map_err(|e| e.to_string())?;
        let client = WorkspaceJoinClient::new(self.inner.host.endpoint.clone());
        let bootstrap = tilo_peer_state::owner::enroll::prove_membership(
            &client,
            &address.endpoint_addr,
            workspace,
            &device.cert,
            device.signer.as_ref(),
        )
        .await?;
        self.hold_member(&device, &bootstrap, hint, false).await
    }

    /// Hold each of `held`, a workspace and where the device hosting it serves it, as a member
    /// copy synced with that device: the workspaces this device takes part in that its service
    /// does not hold yet. One that is not taken in is said and left for the next call.
    pub async fn hold_as_members(
        &self,
        held: impl IntoIterator<Item = (WorkspaceId, TransportHint)>,
    ) {
        for (workspace, hint) in held {
            if self
                .is_member(&workspace.to_string())
                .await
                .unwrap_or(false)
            {
                continue;
            }
            match tokio::time::timeout(HOLD_WITHIN, self.hold_as_member(workspace, &hint)).await {
                Ok(Ok(_)) => {}
                Ok(Err(error)) => {
                    tracing::warn!(%error, %workspace, "a workspace this device takes part in is not held on its service");
                }
                Err(_) => {
                    tracing::warn!(%workspace, "a workspace's member copy did not arrive on this device's service in time");
                }
            }
        }
    }

    /// Hold `bootstrap` as a member copy of the workspace it answers, synced
    /// with the device `hint` names, as `device` certifies. Asked again, the
    /// copy takes in what the answer carries.
    ///
    /// # Errors
    ///
    /// The answer or the address does not hold up, or the copy could not be
    /// kept here.
    #[expect(
        clippy::too_many_lines,
        reason = "one hold: the copy a workstation's disk kept, and the one taken in here"
    )]
    pub(super) async fn hold_member(
        &self,
        device: &MemberDevice,
        bootstrap: &JoinBootstrap,
        hint: &TransportHint,
        asked: bool,
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
        let _holding = self.inner.member.holding.lock().await;
        let held = self.inner.workspaces.lock().await.get(&name).cloned();
        let mut enrolled = 1;
        // Whether this device is taken into the workspace now, never having held it before.
        let mut taken_in = false;
        if let Some(held) = held {
            let mut governed = held.lock().await;
            // A disk copied from the machine that serves this workspace kept the workspace as
            // that machine does, and served it here while no link to the machine reached this
            // one. The machine serving it answered this device, so this machine holds the
            // workspace as a member copy of it from now on: what it reads and writes reaches
            // the workspace, and the copy it kept answers until then.
            let copied = governed.member.is_none();
            // The device serving the workspace, as the copy holds it now: a copy held again by
            // the same device keeps its link.
            let serving_changed = governed
                .member
                .as_ref()
                .is_none_or(|member| member.addr != address.endpoint_addr);
            let mut peer = governed.peer().clone();
            take_governance(&mut peer, bootstrap, now)?;
            *governed.peer.lock() = peer;
            if let Some(member) = governed.member.as_mut() {
                member.addr = address.endpoint_addr.clone();
                member.hint = hint.clone();
                member.continuity = member.continuity.saturating_add(1);
                enrolled = member.continuity;
            } else {
                governed.member = Some(Member {
                    addr: address.endpoint_addr.clone(),
                    hint: hint.clone(),
                    continuity: enrolled,
                });
            }
            if copied {
                governed.persist()?;
                self.bind_member(&governed).await?;
                self.inner.direct.release(workspace);
                self.inner.readings.hold(&name, &held, governed.reading());
            }
            if let Err(error) = keep_member(&governed.path, hint) {
                tracing::warn!(%name, %error, "a member copy's device address was not kept");
            }
            drop(governed);
            if copied {
                tracing::info!(
                    %name,
                    "this machine was copied from the device that serves the workspace, and \
                     holds it as a member copy of it from now on"
                );
            }
            // A copy held again takes in what the new answer carries, and is synced again only
            // when the device serving it changed or its link is down: a live link to the same
            // device is kept, and the frames it is carrying are not thrown away under it.
            if serving_changed || !self.member_link_up(workspace) {
                self.serve_member(&held, device).await?;
            }
        } else {
            taken_in = true;
            let path = self.inner.dir.join(format!("{name}.workspace"));
            let keys = self.device_keys();
            let mut governed = if path.exists() {
                let governed = GovernedWorkspace::open(path.clone(), keys.as_ref())?;
                // A copy let go of here is kept again: it opens with the service once more.
                let _ = std::fs::remove_file(path.with_extension(super::RELEASED));
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
                hint: hint.clone(),
                continuity: enrolled,
            });
            governed.persist()?;
            if let Err(error) = keep_member(&governed.path, hint) {
                tracing::warn!(%name, %error, "a member copy's device address was not kept");
            }
            self.bind_member(&governed).await?;
            let reading = governed.reading();
            let state = Arc::new(tokio::sync::Mutex::new(governed));
            self.inner.readings.hold(&name, &state, reading);
            self.inner
                .workspaces
                .lock()
                .await
                .insert(name.clone(), Arc::clone(&state));
            // Synced once it is held: what the serving device sends from the start is kept in it.
            self.serve_member(&state, device).await?;
        }
        if let Ok(mut materials) = self.inner.member.materials.lock() {
            materials.insert(workspace, (hint.clone(), enrolled));
        }
        // A run admitted under another enrollment of this copy is over.
        self.inner.executions.fence(workspace);
        self.inner.member.held.notify_waiters();
        tracing::info!(%name, workspace_id = %workspace, "holding a workspace this device joined as a member");
        // Their list names what they asked to be in and what is theirs: a join their own device
        // redeemed an invitation for (`asked`), or a workspace a site made for them and handed
        // to them as its owner. Any other join — one another person's agent or API made for them,
        // or a workspace their session entered without their asking — is not listed. The record
        // is written where their own workspace is governed; a site that made a workspace writes
        // nothing of theirs.
        if taken_in
            && (asked || bootstrap.role == Role::Owner)
            && let Ok(summary) = self.summary(&name).await
        {
            self.list_for(
                &device.cert.user_public_key,
                &summary.workspace,
                &summary.name,
            )
            .await;
        }
        // What this device's native apps kept of it in files goes into it now that it is held.
        #[cfg(all(feature = "apps", not(target_arch = "wasm32")))]
        self.import_app_files_soon();
        Ok(workspace)
    }

    /// Stop syncing the member copy `workspace`, which this device let go of: its link, its wait
    /// for the group's keys and its answers end with it.
    pub(super) fn let_go_member(&self, workspace: WorkspaceId) {
        if let Ok(mut all) = self.inner.member.syncing.lock()
            && let Some(syncing) = all.remove(&workspace)
        {
            syncing.abort();
        }
        if let Ok(mut links) = self.inner.member.links.lock() {
            links.remove(&workspace);
        }
        if let Ok(mut ups) = self.inner.member.up.lock() {
            ups.remove(&workspace);
        }
        if let Ok(mut materials) = self.inner.member.materials.lock() {
            materials.remove(&workspace);
        }
    }

    /// The material a run admitted to `workspace`, a member copy, is pinned
    /// to: the device the copy acts as, the device serving it, and how often
    /// the copy was enrolled. `None` for a workspace this device does not
    /// hold as a member copy.
    #[must_use]
    pub fn member_material(&self, workspace: WorkspaceId) -> Option<MemberMaterial> {
        let device = self.inner.member.device.get()?;
        let (hint, continuity) = {
            let materials = self.inner.member.materials.lock().ok()?;
            materials.get(&workspace)?.clone()
        };
        Some(MemberMaterial {
            cert: device.cert.clone(),
            hint,
            continuity,
        })
    }

    /// Told each time a workspace is held here as a member copy.
    #[must_use]
    pub fn members_held(&self) -> Arc<tokio::sync::Notify> {
        Arc::clone(&self.inner.member.held)
    }

    /// Each write of another device that a workspace held here took in, in
    /// the order it was taken: a member's write to a workspace this service
    /// governs, and every write the device serving a member copy sends. This
    /// device's own writes are not told: its apps have them already. A slow
    /// receiver lags, and then reads again what it follows.
    #[must_use]
    pub fn admitted_writes(&self) -> tokio::sync::broadcast::Receiver<AdmittedWrite> {
        self.inner.member.writes.subscribe()
    }

    /// The device this service's member copies act as, once it is handed one:
    /// the device whose own writes [`Self::admitted_writes`] does not tell.
    pub(super) fn member_device_key(&self) -> Option<[u8; 32]> {
        self.inner.member.device.get().map(|device| device.device())
    }

    /// Every workspace held here as a member copy. Read without any
    /// workspace's lock ([`super::readings`]): a workspace busy taking in a
    /// flood of changes is still held here, and a read of it never waits
    /// behind them.
    pub async fn member_ids(&self) -> Vec<WorkspaceId> {
        let kept: Vec<WorkspaceId> = self
            .inner
            .workspaces
            .lock()
            .await
            .keys()
            .filter_map(|workspace| WorkspaceId::parse(workspace))
            .collect();
        let mut ids = Vec::new();
        for workspace in kept {
            if self
                .reading(workspace)
                .await
                .is_some_and(|reading| reading.member)
            {
                ids.push(workspace);
            }
        }
        ids
    }

    /// Every workspace kept here, a member copy or one this device governs,
    /// as its governance describes it ([`describe`]).
    pub async fn described(&self) -> Vec<tilo_desktop_model::Workspace> {
        let kept: Vec<WorkspaceId> = self
            .inner
            .workspaces
            .lock()
            .await
            .keys()
            .filter_map(|workspace| WorkspaceId::parse(workspace))
            .collect();
        let mut described = Vec::new();
        for workspace in kept {
            if let Some(reading) = self.reading(workspace).await {
                let (name, peer) = named_copy(&reading);
                described.extend(describe(workspace, &name, &peer));
            }
        }
        described
    }

    /// Whether the history the device serving `workspace` keeps is in hand on
    /// this copy's link, and a watch of it: true once the link caught up, false
    /// again while a link being made catches up.
    ///
    /// Only once it is true does this copy's [`crate::workspace_documents::DocumentRead::Absent`]
    /// mean the workspace holds nothing, rather than that what it holds has not
    /// arrived: a folder synced from this copy reads what is absent here as
    /// absent there.
    #[must_use]
    pub fn member_caught_up(
        &self,
        workspace: WorkspaceId,
    ) -> Option<tokio::sync::watch::Receiver<bool>> {
        Some(self.member_link(workspace)?.caught_up())
    }

    /// `workspace`'s member copy as this device holds it: the name its
    /// governance gives it, and the replica it syncs. `None` for a workspace
    /// this service does not hold as a member copy.
    pub async fn member_copy(&self, workspace: WorkspaceId) -> Option<(String, StatePeer)> {
        let reading = self.reading(workspace).await?;
        reading.member.then(|| named_copy(&reading))
    }

    /// Where `workspace`'s copies stand, as this device's member copy reads its holders' word:
    /// what tells a folder whether the workspace's servers keep what it holds. `None` for a
    /// workspace this service does not hold as a member copy.
    pub async fn member_copies(
        &self,
        workspace: WorkspaceId,
    ) -> Option<tilo_peer_state::CopyStatus> {
        let reading = self.reading(workspace).await?;
        reading
            .member
            .then(|| reading.replica.read(|peer| peer.workspace().copies()))
    }

    /// `workspace`'s copy as this device holds it, when this device governs
    /// it here rather than as a member: the name its governance gives it and
    /// the replica it keeps. `None` for a member copy or a workspace not held
    /// here.
    pub async fn governed_copy(&self, workspace: WorkspaceId) -> Option<(String, StatePeer)> {
        let reading = self.reading(workspace).await?;
        (!reading.member).then(|| named_copy(&reading))
    }

    /// Send `message` on `workspace`'s member link, while it is up: whether
    /// it was sent. A message handed to a link whose connection is not up
    /// reaches no store of its own — group governance is kept nowhere — so
    /// the caller is told it was not sent, and keeps it for a later pass.
    #[must_use]
    pub fn send_member(&self, workspace: WorkspaceId, message: NeighbourMsg) -> bool {
        if !self.member_up_now(workspace) {
            return false;
        }
        self.member_link(workspace)
            .is_some_and(|link| link.send(message).is_ok())
    }

    /// Whether `workspace`'s member copy is linked to the device serving it
    /// now.
    fn member_up_now(&self, workspace: WorkspaceId) -> bool {
        self.inner
            .member
            .up
            .lock()
            .ok()
            .and_then(|ups| ups.get(&workspace).map(|up| *up.borrow()))
            .unwrap_or(false)
    }

    /// The person the device this member copy acts as acts for in
    /// `workspace`, as its governance admits it now.
    pub async fn member_actor(&self, workspace: WorkspaceId) -> Option<String> {
        let device = self.member_identity().ok()?;
        let (_, state) = self.member_copy(workspace).await?;
        let member = state
            .permissions(device.cert.device_public_key, tilo_clock::wall())
            .ok()?;
        member.person_id().map(str::to_owned)
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

    /// Keep `commit`, a write this device makes to `workspace`, which another
    /// device serves: admitted into this device's documents of it in this
    /// device's name, and sent to the serving device, which fans it out to the
    /// workspace's other members. A write made while that device is away is
    /// kept all the same, never refused for it: it waits among the writes the
    /// serving device has not answered, which go to it once a link is back and
    /// caught up ([`Self::answer_member_writes`]). Answers the order the write
    /// was kept at.
    ///
    /// # Errors
    ///
    /// The workspace is not held here as one another device serves, or the
    /// write could not be kept.
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
        let body = commit.body.clone();
        let order = self
            .inner
            .records
            .carry(workspace_id, device.device(), &commit)?;
        let message: NeighbourMsg =
            NeighbourMsg::decode(&body).map_err(|error| error.to_string())?;
        // This device's own write is this copy's from the moment it is made, as the engine's was:
        // the device serving the workspace carries it to the workspace's other members, and what
        // it sends back may reach this copy later.
        {
            let state = self
                .workspace(workspace)
                .await
                .map_err(|error| error.to_string())?;
            let mut governed =
                crate::timing::said_if_long("the member copy's lock", state.lock()).await;
            self.follow_member_document(&mut governed, device.device(), &message);
        }
        if let NeighbourMsg::AppDocument {
            app_id,
            document_id,
            ..
        } = &message
        {
            // A member copy's own write tells whoever follows its records, as a served write does.
            self.wrote(workspace, app_id, document_id);
            // The frame's hash is what the serving device names its answer by; the write is
            // noted before it goes out, so an answer that comes back at once finds it.
            self.inner.member.note_served(
                workspace_id,
                *blake3::hash(&body).as_bytes(),
                (app_id, document_id),
                &message,
            );
        }
        self.forward_member_document(workspace_id, message);
        Ok(order)
    }

    /// Wait until the device serving `workspace`, a member copy, has answered every write this
    /// device made to `documents` — each an app and one of its documents — before this call: `Ok`
    /// once it holds each of them, `Err` with why once it refused one.
    ///
    /// `None` for a governed workspace or one this device does not hold as a member copy: its own
    /// store kept the write, and no other device answers for it. A write is sent to the serving
    /// device as it is made
    /// ([`Self::commit_member_document`]); an answer can be lost with the link it came on, so a
    /// write still waiting is sent again once a link catches up ([`Self::answer_member_writes`]).
    #[must_use]
    pub fn served(
        &self,
        workspace: &str,
        documents: &[(&str, &str)],
    ) -> Option<crate::workspace_documents::DocumentServing> {
        let workspace = WorkspaceId::parse(workspace)?;
        // A workspace this device governs, or one it holds nothing of, is answered here and by no
        // other device: its own store kept the write.
        self.member_link(workspace)?;
        let documents: Vec<(String, String)> = documents
            .iter()
            .map(|(app, document)| ((*app).to_owned(), (*document).to_owned()))
            .collect();
        let workspaces = self.clone();
        Some(Box::pin(async move {
            workspaces.wait_served(workspace, &documents).await
        }))
    }

    /// Lose the next `count` answers the device serving `workspace` gives this device's writes,
    /// as a link that ended after the write was kept and before its answer came: what a test
    /// makes a write wait for the next link's catch-up.
    #[cfg(test)]
    pub(super) fn lose_member_answers(&self, workspace: WorkspaceId, count: usize) {
        if let Some(link) = self.member_link(workspace) {
            link.lose_answers(count);
        }
    }

    /// [`Self::served`], over `workspace`'s id and documents already owned.
    async fn wait_served(
        &self,
        workspace: WorkspaceId,
        documents: &[(String, String)],
    ) -> Result<(), String> {
        // Subscribed before the first read: a write answered between the two ticks a receiver
        // that subscribed after it.
        let mut changed = self.inner.member.served_changed.subscribe();
        let mut waiting = std::collections::BTreeSet::new();
        let mut first = true;
        loop {
            {
                let Ok(mut served) = self.inner.member.served.lock() else {
                    return Err("this device's member copies closed".into());
                };
                let Some(writes) = served.get_mut(&workspace) else {
                    return Ok(());
                };
                if let Some(refused) = writes.take_refusal(documents) {
                    return Err(refused);
                }
                let of_documents = writes.waiting_of(documents);
                waiting = if first {
                    first = false;
                    of_documents
                } else {
                    // A write made while this waits is not one it asked after: the caller named
                    // the writes before it.
                    waiting.intersection(&of_documents).copied().collect()
                };
                if waiting.is_empty() {
                    return Ok(());
                }
            }
            changed.changed().await.map_err(|_| {
                "this device's workspace service ended before the write was answered".to_owned()
            })?;
        }
    }

    /// Take what the device serving `workspace` answers of this device's writes there, and ask
    /// again the writes it has not answered once the link catches up: an answer lost with the
    /// link it was written on is asked for again on the next one.
    fn answer_member_writes(
        &self,
        workspace: WorkspaceId,
        link: &DirectSync,
    ) -> tokio::task::AbortHandle {
        let mut answers = link.answers();
        let mut caught_up = link.caught_up();
        let workspaces = self.downgrade();
        tokio::spawn(async move {
            loop {
                let answered = tokio::select! {
                    answered = answers.recv() => Some(answered),
                    changed = caught_up.changed() => {
                        if changed.is_err() {
                            return;
                        }
                        None
                    }
                };
                let Some(workspaces) = workspaces.upgrade() else {
                    return;
                };
                match answered {
                    Some(Ok(answered)) => workspaces.inner.member.answered_served(workspace, answered),
                    Some(Err(tokio::sync::broadcast::error::RecvError::Lagged(missed))) => {
                        tracing::debug!(
                            %workspace,
                            missed,
                            "a member copy's answers fell behind; its waiting writes are asked again"
                        );
                        workspaces.reask_member_writes(workspace);
                    }
                    Some(Err(tokio::sync::broadcast::error::RecvError::Closed)) => return,
                    None if *caught_up.borrow() => workspaces.reask_member_writes(workspace),
                    None => {}
                }
            }
        })
        .abort_handle()
    }

    /// Send this device's writes to `workspace` the serving device has not answered yet once
    /// more: what a write whose answer was lost with its link waits for.
    fn reask_member_writes(&self, workspace: WorkspaceId) {
        let waiting = self.inner.member.served_waiting(workspace);
        if waiting.is_empty() {
            return;
        }
        let Some(link) = self.member_link(workspace) else {
            return;
        };
        for message in waiting {
            if let Err(error) = link.send(message) {
                tracing::debug!(%workspace, %error, "a member copy's waiting write did not go again");
            }
        }
    }

    /// Say `workspace`'s link to the device serving it is up, and send that device first what this
    /// device wrote while it was away, in the order it was written.
    fn member_link_reached(&self, workspace: WorkspaceId) {
        let Some(link) = self.member_link(workspace) else {
            return;
        };
        let Ok(mut away) = self.inner.member.away.lock() else {
            return;
        };
        if let Ok(ups) = self.inner.member.up.lock()
            && let Some(up) = ups.get(&workspace)
        {
            up.send_replace(true);
        }
        for message in away.remove(&workspace).unwrap_or_default() {
            if let Err(error) = link.send(message) {
                tracing::debug!(%workspace, %error, "a write made while the serving device was away did not go");
            }
        }
    }

    /// Say `workspace`'s link to the device serving it is down, as when that device cannot be
    /// reached, and up again when `up`: what a test makes a write wait out.
    #[cfg(test)]
    pub(super) fn set_member_link(&self, workspace: WorkspaceId, up: bool) {
        if up {
            self.member_link_reached(workspace);
        } else if let Ok(ups) = self.inner.member.up.lock()
            && let Some(link) = ups.get(&workspace)
        {
            link.send_replace(false);
        }
    }

    /// Waits until the member copy's link is up, for a write that must reach the device serving
    /// it: the workspaces that write resources ask it, and so do the tests of the link itself.
    #[cfg(any(feature = "apps", feature = "agents", test))]
    pub(super) async fn wait_member_link(&self, workspace_id: WorkspaceId) -> Result<(), String> {
        let mut up = self.member_up(workspace_id)?;
        tokio::time::timeout(REACHED_WITHIN, up.wait_for(|up| *up))
            .await
            .map_err(|_| "the device serving the workspace was not reached in time")?
            .map_err(|_| "the member copy's link closed")?;
        Ok(())
    }

    pub(super) fn follow_member_document(
        &self,
        governed: &mut GovernedWorkspace,
        author: [u8; 32],
        message: &NeighbourMsg,
    ) {
        if governed.take_served(author, message) {
            self.keep_soon(governed);
        }
    }

    /// Send `message`, this device's write, to the device serving `workspace_id`: now while the
    /// link is up, else the moment it is, after the writes made before it.
    pub(super) fn forward_member_document(&self, workspace_id: WorkspaceId, message: NeighbourMsg) {
        let Some(link) = self.member_link(workspace_id) else {
            return;
        };
        // Under the queue's lock, as the link drains it when it comes up: no write slips between.
        let Ok(mut away) = self.inner.member.away.lock() else {
            return;
        };
        if !self.member_up_now(workspace_id) {
            away.entry(workspace_id).or_default().push(message);
            return;
        }
        drop(away);
        if let Err(error) = link.send(message) {
            tracing::debug!(%workspace_id, %error, "a member write did not reach the serving device");
        }
    }

    /// Keep `request`, a terminal effect receipt this device served in
    /// `workspace`, a member copy: recorded in this device's slice of the
    /// workspace and sent to the device serving it, answered with its
    /// receipt.
    ///
    /// # Errors
    ///
    /// The workspace is not a member copy held here, the request is not an
    /// effect receipt, or the receipt could not be kept.
    pub async fn commit_member_effect_receipt(
        &self,
        workspace: &str,
        redacted: &[u8],
    ) -> Result<(), String> {
        if !self.is_member(workspace).await? {
            return Err("this workspace is not held here as a member copy".into());
        }
        let receipt: tilo_desktop_model::WorkspaceEffectReceipt =
            serde_json::from_slice(redacted).map_err(|error| error.to_string())?;
        let device = self.member_identity()?;
        let state = self
            .workspace(workspace)
            .await
            .map_err(|error| error.to_string())?;
        let messages = {
            let governed = state.lock().await;
            let now = tilo_clock::wall();
            let mut candidate = governed.peer().clone();
            candidate
                .workspace_mut()
                .apply_own_effect_receipt(&device.cert, &receipt, now)?;
            let messages = vec![candidate.workspace_message()];
            *governed.peer.lock() = candidate;
            drop(governed);
            messages
        };
        let commit = messages
            .into_iter()
            .next()
            .map(|message| {
                neighbour_wire::retention(&message, false)
                    .ok_or_else(|| "the receipt carries no retention route".to_owned())?;
                let body = postcard::to_stdvec(&message).map_err(|error| error.to_string())?;
                Ok::<_, String>(HostCommit { body })
            })
            .transpose()?;
        // A receipt whose change produced nothing is one the workspace already
        // holds.
        if let Some(commit) = commit {
            let _ = self.commit_member_document(workspace, commit).await?;
        }
        Ok(())
    }

    /// Commit `request`, a document write this device makes to `workspace`: admitted into the
    /// workspace's own persistence when this service governs it, else taken into the member copy
    /// and carried to the device serving the workspace.
    ///
    /// # Errors
    ///
    /// [`WriteRefusal::Waiting`] while this device is still taking the workspace in (joining it,
    /// or opening it as it starts): the same write is kept once it holds it. Otherwise the write
    /// is not this device's, or it could not be kept.
    pub async fn commit_local_document(
        &self,
        workspace: &str,
        app: &str,
        document: &str,
        readers: Option<&str>,
        envelope: SignedOp,
    ) -> Result<(), WriteRefusal> {
        let workspace_id = WorkspaceId::parse(workspace)
            .ok_or_else(|| WriteRefusal::Refused(format!("{workspace} is not a workspace id")))?;
        let device = self.signing_device();
        if envelope.author.as_bytes() != &device.device() {
            return Err(WriteRefusal::Refused(
                "the document's signer differs from this device".into(),
            ));
        }
        let member = self.is_member(workspace).await?;
        if !member && self.governed_copy(workspace_id).await.is_none() {
            return Err(WriteRefusal::Waiting(MissingPrerequisite::Workspace));
        }
        let message = NeighbourMsg::AppDocument {
            peer: neighbour_wire::peer_handle(&device.device()),
            app_id: app.to_owned(),
            cluster_id: workspace_id.to_string(),
            document_id: document.to_owned(),
            envelope: Box::new(envelope.clone()),
            readers: readers.map(str::to_owned),
        };
        if !member {
            return self
                .commit_own_document(workspace, app, document, readers, &envelope)
                .await
                .map_err(WriteRefusal::from);
        }
        if neighbour_wire::retention(&message, false).is_none() {
            return Err(WriteRefusal::Refused(
                "the write carries no retention route".into(),
            ));
        }
        let body = postcard::to_stdvec(&message).map_err(|error| error.to_string())?;
        self.commit_member_document(workspace, HostCommit { body })
            .await?;
        Ok(())
    }

    /// Admit a write this device makes to `workspace`, the profile's own workspace this service
    /// holds and governs: into its ordered persistence, carried to the workspace's other
    /// devices.
    ///
    /// # Errors
    ///
    /// The workspace is not governed here, the write is not this device's, or it could not be
    /// kept.
    pub async fn commit_own_document(
        &self,
        workspace: &str,
        app: &str,
        document: &str,
        readers: Option<&str>,
        envelope: &SignedOp,
    ) -> Result<(), String> {
        let workspace_id = WorkspaceId::parse(workspace).ok_or("not a workspace id")?;
        let device = self.signing_device();
        if self.is_member(workspace).await? {
            return Err("this workspace is held here as a member copy".into());
        }
        if self.governed_copy(workspace_id).await.is_none() {
            return Err("this workspace is not governed here".into());
        }
        let message = Self::local_document_message(
            workspace_id,
            device.device(),
            app,
            document,
            readers,
            envelope,
        )?;
        let body = postcard::to_stdvec(&message).map_err(|error| error.to_string())?;
        self.absorb(workspace, device.device(), &body).await?;
        // The workspace took the write: the devices it is linked to take it in as they would
        // a member's, live, and one that links later from the store.
        self.inner
            .records
            .carry(workspace_id, device.device(), &HostCommit { body })?;
        Ok(())
    }

    /// Keep a locally drafted document, using its authenticated writer, and
    /// carry it on the workspace's link.
    #[cfg(any(feature = "apps", feature = "agents"))]
    pub(super) fn retain_local_document(
        &self,
        workspace_id: WorkspaceId,
        author: [u8; 32],
        app: &str,
        document: &str,
        readers: Option<&str>,
        envelope: &SignedOp,
    ) -> Result<NeighbourMsg, String> {
        let message =
            Self::local_document_message(workspace_id, author, app, document, readers, envelope)?;
        let body = postcard::to_stdvec(&message).map_err(|error| error.to_string())?;
        self.inner
            .records
            .carry(workspace_id, author, &HostCommit { body })?;
        Ok(message)
    }

    /// The frame a write of `author`'s goes out as.
    fn local_document_message(
        workspace_id: WorkspaceId,
        author: [u8; 32],
        app: &str,
        document: &str,
        readers: Option<&str>,
        envelope: &SignedOp,
    ) -> Result<NeighbourMsg, String> {
        if envelope.author.as_bytes() != &author {
            return Err("the document's signer differs from this device".into());
        }
        Ok(NeighbourMsg::AppDocument {
            peer: neighbour_wire::peer_handle(&author),
            app_id: app.to_owned(),
            cluster_id: workspace_id.to_string(),
            document_id: document.to_owned(),
            envelope: Box::new(envelope.clone()),
            readers: readers.map(str::to_owned),
        })
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
        let mut candidate = governed.peer().clone();
        candidate
            .workspace_mut()
            .apply_workspace_membership_command(
                &device.cert,
                &command,
                device.signer.as_ref(),
                now,
            )?;
        let message = candidate.workspace_message();
        *governed.peer.lock() = candidate;
        neighbour_wire::retention(&message, false).ok_or("the write carries no retention route")?;
        let body = postcard::to_stdvec(&message).map_err(|error| error.to_string())?;
        drop(governed);
        Ok(HostCommit { body })
    }

    /// Write `command`, a change this device makes as its own person's (a grant, a setting, a
    /// member's place), into `workspace`: its member copy's, sent to the device serving it, or the
    /// workspace governed here, with `receipt`, what its history says of it, in the same change.
    /// The workspace's admission decides it against this device's grants, as any member's.
    ///
    /// # Errors
    /// The workspace is not held here, this device may not make the change, or it could not be
    /// committed.
    pub async fn commit_own_change(
        &self,
        workspace: &str,
        command: tilo_peer_state::workspace_membership::WorkspaceMembershipCommand,
        receipt: Option<&tilo_desktop_model::WorkspaceEffectReceipt>,
    ) -> Result<(), String> {
        if self.is_member(workspace).await? {
            let commit = self.member_membership_write(workspace, command).await?;
            self.commit_member_document(workspace, commit).await?;
        } else {
            self.commit_own_membership(workspace, command, receipt)
                .await
                .map_err(|error| error.to_string())?;
        }
        Ok(())
    }

    /// Ticks true when `workspace`'s member copy is linked to the device
    /// serving it, and false when that link ends.
    #[cfg(any(feature = "apps", feature = "agents", test))]
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

    /// The link of `workspace`'s member copy, while it is up: what a session of this device
    /// reads and writes the workspace through instead of dialling a link of its own.
    #[must_use]
    pub fn member_link(&self, workspace: WorkspaceId) -> Option<DirectSync> {
        self.inner
            .member
            .links
            .lock()
            .ok()
            .and_then(|links| links.get(&workspace).cloned())
    }

    /// The links of every member copy that is up.
    pub(crate) fn member_links(&self) -> Vec<DirectSync> {
        self.inner
            .member
            .links
            .lock()
            .map(|links| links.values().cloned().collect())
            .unwrap_or_default()
    }

    /// Whether a link to the device serving `workspace` is up here: the copy's dialling task is
    /// still running, so a copy held again keeps it rather than replacing it.
    fn member_link_up(&self, workspace: WorkspaceId) -> bool {
        self.inner
            .member
            .syncing
            .lock()
            .ok()
            .and_then(|all| {
                all.get(&workspace)
                    .map(|syncing| !syncing.dialling.is_finished())
            })
            .unwrap_or(false)
    }

    /// What syncs `workspace`'s member copy, as the ids of its dialling and following tasks: a
    /// test tells a link it kept from one it dialled again by them.
    #[cfg(test)]
    pub(super) fn member_syncing(&self, workspace: WorkspaceId) -> Option<[tokio::task::Id; 2]> {
        self.inner
            .member
            .syncing
            .lock()
            .ok()?
            .get(&workspace)
            .map(|tasks| [tasks.dialling.id(), tasks.following.id()])
    }

    /// The address of the device serving `workspace`, as its member copy here holds it: what a
    /// test holding the copy again presents.
    #[cfg(test)]
    pub(super) async fn member_hint(&self, workspace: WorkspaceId) -> Option<TransportHint> {
        let state = self
            .inner
            .workspaces
            .lock()
            .await
            .get(&workspace.to_string())
            .cloned()?;
        let governed = state.lock().await;
        governed.member.as_ref().map(|member| member.hint.clone())
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
        // A workspace is opened from its own files, with no other's: a start with many
        // opens a few side by side, so it does not wait for each one's history in turn.
        let mut paths = Vec::new();
        let entries = std::fs::read_dir(dir).map_err(|error| error.to_string())?;
        for entry in entries {
            let path = entry.map_err(|error| error.to_string())?.path();
            // A workspace this service released stays on disk, unserved.
            if super::delete::passed_over(dir, &self.inner.direct, &path)
                || path.with_extension(super::RELEASED).exists()
            {
                continue;
            }
            paths.push(path);
        }
        let opened = open_side_by_side(paths, device.as_ref(), dir).await?;
        let mut workspaces = std::collections::BTreeMap::new();
        for workspace in opened {
            let name = workspace.workspace().to_owned();
            let reading = workspace.reading();
            let state = if let Some(member) = workspace.member.clone() {
                if let Ok(mut materials) = self.inner.member.materials.lock() {
                    materials.insert(workspace.workspace_id(), (member.hint, member.continuity));
                }
                Box::pin(self.kept_member(workspace)).await?
            } else {
                self.serve_direct(&workspace)?;
                Arc::new(tokio::sync::Mutex::new(workspace))
            };
            self.inner.readings.hold(&name, &state, reading);
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
        let mut kept = Vec::new();
        for state in members {
            if state.lock().await.member.is_some() {
                kept.push(state);
            }
        }
        // A service that keeps no member copy, a server's, has none to sync.
        if kept.is_empty() {
            return;
        }
        let device = match self.member_identity() {
            Ok(device) => device,
            Err(error) => {
                tracing::warn!(%error, copies = kept.len(), "member copies are kept here but not synced");
                return;
            }
        };
        for state in kept {
            if let Err(error) = self.serve_member(&state, &device).await {
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
        crate::workspace_io::get_or_initialize(
            &services.workspaces,
            services.access.state_root().to_path_buf(),
            workspace_id,
            crate::registry::MAX_HELD_WORKSPACES,
        )
        .await
        .map_err(|error| error.to_string())?;
        Ok(())
    }

    /// Sync the member copy `state` holds with the device serving it, as `device`:
    /// take in what that device sends, and answer it as a member answers. Asked
    /// again for a copy already synced, the copy's new enrollment alone syncs it.
    pub(super) async fn serve_member(
        &self,
        state: &Arc<tokio::sync::Mutex<GovernedWorkspace>>,
        device: &MemberDevice,
    ) -> Result<(), String> {
        let mut governed = state.lock().await;
        let member = governed
            .member
            .clone()
            .ok_or("the workspace is not a member copy")?;
        // The serving device answers on its service's key, which the hint's signature binds to
        // that device: what its history names no author for is that device's.
        let (address, _) = WorkspaceDeviceHintV1::authenticate_transport_hint(&member.hint)
            .map_err(|e| e.to_string())?;
        let serving = address.device_certificate.device_public_key;
        let host = address.device_certificate.clone();
        let workspace_id = governed.workspace_id();
        let name = governed.workspace().to_owned();
        let link = DirectSync::over(governed.peer.clone(), Arc::clone(&device.signer))
            .with_store(Arc::clone(&self.inner.store));
        self.recall_member_resources(&mut governed, Some(&link))
            .await?;
        drop(governed);
        if let Ok(mut links) = self.inner.member.links.lock() {
            links.insert(workspace_id, link.clone());
        }
        let keying = self.take_part_in_keys(&name, Held::MemberCopy);
        let following = self.follow_member(&name, &link);
        let (up, waiting) = tokio::sync::watch::channel(false);
        drop(waiting);
        if let Ok(mut ups) = self.inner.member.up.lock() {
            ups.insert(workspace_id, up.clone());
        }
        let endpoint = self.inner.host.endpoint.clone();
        let answering = self.answer_member_writes(workspace_id, &link);
        let workspaces = self.downgrade();
        let device = device.clone();
        let dialing = tokio::spawn(async move {
            loop {
                let dialed = match proved_link(&endpoint, &member.addr, &device, &host).await {
                    Ok(carrier) => link.dial_over(carrier, serving).await,
                    Err(refusal) => Err(refusal),
                };
                match dialed {
                    Ok(dialed) => {
                        let Some(workspaces) = workspaces.upgrade() else {
                            return;
                        };
                        workspaces.member_link_reached(workspace_id);
                        drop(workspaces);
                        if let Err(refusal) = dialed.run(&link).await {
                            tracing::debug!(%name, ?refusal, "a member copy's link ended");
                        }
                        up.send_replace(false);
                    }
                    Err(refusal @ Refusal::Revoked(_)) => {
                        tracing::debug!(%name, ?refusal, "a member copy's device says the key is retired");
                        // The serving device says the person's root retired this device's key,
                        // with the entry the root signed: kept only once the entry verifies.
                        if let Some(workspaces) = workspaces.upgrade() {
                            workspaces.take_refusal(refusal).await;
                        }
                    }
                    Err(Refusal::NotAMember(said))
                        if said == tilo_peer_state::direct::REMOVED
                            || said
                                == tilo_server_protocol::auth::HelloRefusal::NotAdmitted.code() =>
                    {
                        // The device serving the workspace answers that it removed this device's
                        // person, or that none of its workspaces has them any more: the copy is
                        // let go rather than kept and dialled again, so what the Files mounts
                        // enforce for it goes with it. A device it does not know yet, or whose
                        // certificate lapsed, is not told this, and dials again. Off this task,
                        // which letting the copy go ends along with its other syncs.
                        let refusal = Refusal::NotAMember(said);
                        tracing::info!(%name, ?refusal, "this device is no longer a member of the workspace");
                        let Some(released) = workspaces.upgrade() else {
                            return;
                        };
                        let name = name.clone();
                        tokio::spawn(async move {
                            if let Err(error) = released.release(&name).await {
                                tracing::warn!(%name, %error, "a workspace this device is no longer a member of was not let go");
                            }
                        });
                        return;
                    }
                    Err(error) => {
                        tracing::debug!(%name, %error, "a member copy's device is unavailable");
                    }
                }
                tokio::time::sleep(REDIAL).await;
            }
        });
        let syncing = MemberSyncing {
            dialling: dialing.abort_handle(),
            following,
            keying,
            answering,
        };
        if let Ok(mut all) = self.inner.member.syncing.lock()
            && let Some(before) = all.insert(workspace_id, syncing)
        {
            before.abort();
        }
        Ok(())
    }

    /// Have this device take part in the group of `workspace`, as the copy is held: its key
    /// service asks the group's key holders to admit it, and keeps asking until one does. The
    /// group admits only a device that asks, and reading a sealed record never asks (it waits
    /// for keys the device is due, [`crate::documents::Sealing::Due`]): a device that joined a
    /// workspace and only reads what was written there before it arrived would otherwise hold
    /// every sealed record unopened, however long it waited.
    ///
    /// When the device comes to hold the group's keys, the member copy takes in once what it
    /// kept but could not open before ([`Self::take_in_what_keys_open`]), and a governed copy —
    /// one another host handed over — replays what this host retains ([`Self::replay`]). Answers
    /// the task that waits for them, `None` when the device has no key service.
    pub(super) fn take_part_in_keys(
        &self,
        workspace: &str,
        held: Held,
    ) -> Option<tokio::task::AbortHandle> {
        let keys = self.device_keys()?;
        let workspace = workspace.to_owned();
        let workspaces = self.downgrade();
        let task = tokio::spawn(async move {
            if !super::sealed_documents::wait_for_group_keys(&keys, &workspace).await {
                return;
            }
            let Some(workspaces) = workspaces.upgrade() else {
                return;
            };
            match held {
                Held::MemberCopy => workspaces.take_in_what_keys_open(&workspace).await,
                Held::Governs => workspaces.replay(&workspace).await,
            }
        });
        Some(task.abort_handle())
    }

    /// Take into the member copy `workspace` the writes the device serving it sent that the copy
    /// kept but could not take in: a sealed record's writes that arrived before this device held
    /// the keys that open them. The copy keeps every write it is sent
    /// ([`Self::keep_member_write`]) and nothing sends those writes again, so without this the
    /// copy would hold the record without them for good, and so would everything reading it
    /// here. What it takes in reaches whoever follows the record as the history it is.
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the copy stays held until every kept write is in, so a write that comes meanwhile is judged after them"
    )]
    async fn take_in_what_keys_open(&self, workspace: &str) {
        let Ok(state) = self.workspace(workspace).await else {
            return;
        };
        let mut governed = state.lock().await;
        let retained = match self.inner.records.retained(governed.workspace_id()).await {
            Ok(retained) => retained,
            Err(error) => {
                tracing::warn!(%workspace, %error, "a member copy did not read the writes it kept");
                return;
            }
        };
        let mut took = false;
        for frame in retained {
            // A copy keeps every write it was sent, and taking each in again is work for this
            // thread alone, none of it waiting: a copy of a large workspace takes minutes. The
            // device's other work runs between two of them, its endpoint among it, so its server
            // still hears from it and a dial it makes is answered rather than timing out.
            tokio::task::yield_now().await;
            let Ok(message) = NeighbourMsg::decode(&frame.body) else {
                continue;
            };
            let NeighbourMsg::AppDocument {
                app_id,
                document_id,
                ..
            } = &message
            else {
                continue;
            };
            // The keys may have made a kept record readable: whoever follows it is told.
            self.wrote(workspace, app_id, document_id);
            // A resource's document is the store's, which takes it in as it does at the copy's
            // start ([`Self::recall_member_resources`]).
            if collab_types::is_store_document(document_id) {
                self.follow_member_document(&mut governed, frame.author, &message);
                let admitted = self
                    .replay_resource_write(&mut governed, frame.author, &message)
                    .await;
                took |= admitted;
                continue;
            }
            let taken = governed.take_served(frame.author, &message)
                || governed
                    .follow_kept(frame.author, message.clone())
                    .await
                    .changed;
            if taken {
                self.tell_followers(&governed, frame.author, &message, true)
                    .await;
                took = true;
            }
        }
        if took {
            self.keep_soon(&mut governed);
        }
    }

    /// Project this copy's retained resource frames before starting its link.
    pub(super) async fn recall_member_resources(
        &self,
        governed: &mut GovernedWorkspace,
        link: Option<&DirectSync>,
    ) -> Result<(), String> {
        let retained = self.inner.records.retained(governed.workspace_id()).await?;
        for frame in retained {
            let Ok(message) = NeighbourMsg::decode(&frame.body) else {
                continue;
            };
            if matches!(&message, NeighbourMsg::AppDocument { document_id, .. }
                if collab_types::is_store_document(document_id))
            {
                self.follow_member_document(governed, frame.author, &message);
                let admitted = self
                    .replay_resource_write(governed, frame.author, &message)
                    .await;
                if admitted
                    && self.member_device_key() == Some(frame.author)
                    && let Some(link) = link
                {
                    // Fill the link's catch-up history if this copy ended after retention.
                    link.send(message)?;
                }
            }
        }
        Ok(())
    }

    /// Read what the serving device sends a member copy: every resource write it
    /// carries is taken into the copy's documents in its author's name and
    /// handed to the apps following it; group governance goes to this device's
    /// key service; content pieces and asks are handled as the workspace's link
    /// handles them.
    fn follow_member(&self, workspace: &str, link: &DirectSync) -> tokio::task::AbortHandle {
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
                        | NeighbourMsg::DocumentContentRefused { .. }
                );
                let asking = matches!(frame.message, NeighbourMsg::DocumentContentWant { .. });
                let Ok(body) = postcard::to_allocvec(&frame.message) else {
                    continue;
                };
                // A group is settled between the workspace's key holders,
                // never in its state: a group frame is no workspace write,
                // keeps no admission report, and is taken in every time it
                // comes, so a member copy hears the group it was let into as
                // it is founded and after.
                let governance = frame.message.governance_workspace().is_some();
                if (governance || (frame.fresh && frame.report.is_ok()))
                    && let Err(error) = workspaces
                        .keep_member_write(
                            &name,
                            frame.author,
                            &frame.message,
                            &body,
                            frame.retained,
                            frame.landed,
                        )
                        .await
                {
                    tracing::warn!(%name, %error, "a member copy did not keep what the serving device sent");
                }
                // A write caught up, not one written now: the link admitted it
                // into this copy's replica before the reader heard it, so
                // neither taking it in nor landing it changes the copy, and
                // nothing above told a follower. Whoever follows the record
                // hears of it as of a write made now, from where the copy
                // keeps it.
                if frame.retained
                    && frame.report.is_ok()
                    && let NeighbourMsg::AppDocument {
                        app_id,
                        document_id,
                        ..
                    } = &frame.message
                {
                    workspaces.wrote(&name, app_id, document_id);
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
                        NeighbourMsg::decode(&answer)
                    ) else {
                        continue;
                    };
                    // Waits while the link to the serving device is full: the
                    // asks behind this one wait with it.
                    if let Err(error) = link.send_content(message).await {
                        tracing::debug!(%name, %error, "a member's content answer did not reach the serving device");
                    }
                }
            }
        })
        .abort_handle()
    }

    /// Take `message`, `author`'s frame the serving device sent this member
    /// copy, into
    /// it: a resource write into the copy's documents and the apps following
    /// it, group governance into this device's key service. `history` says
    /// the frame came from the serving device's retained history, and `landed`
    /// that the link took it into the copy's replica before its reader heard
    /// it.
    async fn keep_member_write(
        &self,
        workspace: &str,
        author: [u8; 32],
        message: &NeighbourMsg,
        body: &[u8],
        history: bool,
        landed: bool,
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
        if neighbour_wire::retention(message, false).is_none() {
            return Ok(());
        }
        let state = self.workspace(workspace).await.map_err(|e| e.to_string())?;
        let mut governed = state.lock().await;
        // A write the copy's own receive refuses may still be a compaction the
        // copy is behind; that is taken as the serving device kept it.
        let taken = governed.take_served(author, message)
            || governed.follow_kept(author, message.clone()).await.changed;
        // The link admits a frame into the copy's replica before its reader
        // hears it, so a write it landed changed the copy without `taken`
        // seeing it. Whoever follows the copy's records is told either way:
        // otherwise a folder's sync hears of the serving device's edits only
        // when it next reads every record.
        if taken || landed {
            self.tell_followers(&governed, author, message, history)
                .await;
            self.wrote(workspace, app_id, document_id);
            if taken {
                self.keep_soon(&mut governed);
            }
        } else {
            // A write the copy cannot take in yet (a sealed record whose keys have not reached the
            // service) is still the apps' to show, which may hold its keys.
            self.hand_on(&governed, author, message, history);
        }
        // A write of this device's own the serving device sent is one this
        // device made before it lost what it held, an empty disk after a move:
        // its apps never had it in this life, and take it in as anyone's.
        if self.member_device_key() == Some(author) {
            self.hand_on_any(&governed, author, message, history);
        }
        // Kept before the copy is let go, so the copy taking in what it kept once its keys
        // arrive ([`Self::take_in_what_keys_open`]) reads this write too, if this copy could not
        // take it in.
        self.inner.records.carry(
            governed.workspace_id(),
            author,
            &HostCommit {
                body: body.to_vec(),
            },
        )?;
        drop(governed);
        Ok(())
    }
}

#[cfg(test)]
#[path = "member_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "agent_turn_tests.rs"]
mod agent_turn_tests;

#[cfg(test)]
#[path = "person_devices_tests.rs"]
mod person_devices_tests;

#[cfg(all(test, feature = "agents"))]
#[path = "served_notices_tests.rs"]
mod served_notices_tests;

#[cfg(all(test, feature = "agents"))]
#[path = "connection_turn_tests.rs"]
mod connection_turn_tests;

#[cfg(all(test, feature = "agents"))]
#[path = "own_agent_tests.rs"]
mod own_agent_tests;

#[cfg(all(test, feature = "agents"))]
#[path = "hosted_own_agent_tests.rs"]
mod hosted_own_agent_tests;

/// The workspaces at `paths` opened, a few at a time: each reads its own files, and one at
/// a time made a start wait for every workspace's history in turn.
///
/// # Errors
///
/// A workspace could not be opened.
#[cfg(not(target_arch = "wasm32"))]
async fn open_side_by_side(
    paths: Vec<PathBuf>,
    device: Option<&super::DeviceKeys>,
    dir: &Path,
) -> Result<Vec<GovernedWorkspace>, String> {
    /// Workspaces opened at once.
    const AT_ONCE: usize = 4;
    let mut opening = tokio::task::JoinSet::new();
    let mut opened = Vec::with_capacity(paths.len());
    let mut paths = paths.into_iter();
    loop {
        while opening.len() < AT_ONCE {
            let Some(path) = paths.next() else { break };
            let device = device.cloned();
            let dir = dir.to_owned();
            opening.spawn_blocking(move || {
                GovernedWorkspace::open(path.clone(), device.as_ref())
                    .map_err(|error| super::document_format::unopened(&path, &dir, &error))
            });
        }
        let Some(joined) = opening.join_next().await else {
            break;
        };
        opened.push(joined.map_err(|error| error.to_string())??);
    }
    Ok(opened)
}

/// [`open_side_by_side`] in one thread, as a wasm build has no blocking threads to open on.
///
/// # Errors
///
/// A workspace could not be opened.
#[cfg(target_arch = "wasm32")]
async fn open_side_by_side(
    paths: Vec<PathBuf>,
    device: Option<&super::DeviceKeys>,
    dir: &Path,
) -> Result<Vec<GovernedWorkspace>, String> {
    paths
        .into_iter()
        .map(|path| {
            GovernedWorkspace::open(path.clone(), device)
                .map_err(|error| super::document_format::unopened(&path, dir, &error))
        })
        .collect()
}

/// The name a workspace's governance gives it, and its replica, as `reading`
/// reads them now.
/// `workspace`, named `name`, as the copy `peer` of it describes it: its
/// settings and shortcuts, its sandbox and the names it keeps for its people.
#[must_use]
pub fn describe(
    workspace: WorkspaceId,
    name: &str,
    peer: &StatePeer,
) -> Option<tilo_desktop_model::Workspace> {
    let document = peer.admitted_workspace()?;
    let mut described = tilo_desktop_model::Workspace::new(workspace, name.to_owned());
    tilo_peer_state::workspace::adopt_workspace(&mut described, document, &[]);
    described.sandbox = peer.governance().governed_sandbox().ok()?;
    // The names the workspace's governance keeps for its people: what every surface showing
    // someone who is not here reads, from the same list the agent worker's members come from.
    if let Ok(people) = peer.governance().governed_members(tilo_clock::wall()) {
        described.named_people(
            people
                .members
                .iter()
                .filter(|member| !member.removed)
                .filter_map(|member| Some((member.id.person_id()?, member.name.as_str()))),
        );
    }
    Some(described)
}

fn named_copy(reading: &super::readings::Reading) -> (String, StatePeer) {
    let peer = reading.replica.read(StatePeer::clone);
    let name = peer.governance().workspace_name().unwrap_or_default();
    (name, peer)
}
