//! Holding a workspace that was made on another device.
//!
//! A holder of the workspace hands it over with
//! [`HOST_WORKSPACE`](tilo_server_protocol::workspaces::HOST_WORKSPACE): the
//! workspace as a joining member gets it (its signed governance, roster and
//! revocations) and the handing holder's own signed address. This device then
//! keeps a writable copy of its own ([`GovernedWorkspace::adopt`]), as the device
//! the workspace was made on does: it writes what it changes under its own
//! device's key, takes in what every other holder and member writes, admits
//! whom the workspace's governance admits, and keeps a session to every
//! holder its governance names ([`Workspaces::dial_holders`]). Losing any one
//! holder loses nothing the others hold, and leaves the workspace writable at
//! each of them.
//!
//! Which device owns the workspace is its own fact, kept with it as it was
//! handed over; this device is only the one writing its changes, and
//! the workspace's compacted base stays the owning device's to publish.
//! A server of another person holds the workspace without being a member: of
//! who is in it, it writes only the redemptions of invitations it answers
//! ([`tilo_peer_state::workspace::SenderAccess::Holder`]).
//!
//! Each holder says in the workspace's state where its copy stands
//! ([`tilo_peer_state::HeldCopy`]): which holders it still takes what it
//! lacks from, and which it cannot reach and since when, as its sessions to
//! them stand ([`Links`]). A holder that comes back after a while dials the
//! others, takes in what they hold and it lacks, and then says it is current;
//! the others, reaching it again, say so too. Every device reads from those
//! rows, against how many copies the workspace wants, which holder is short
//! of what ([`Workspaces::copies`]).
//!
//! The device lets the workspace go ([`Workspaces::release`]) when its owners
//! ask ([`UNHOST_WORKSPACE`](tilo_server_protocol::workspaces::UNHOST_WORKSPACE))
//! or when the workspace's governance no longer names it. The files stay; a
//! later hand-over serves the workspace from them again.

use std::collections::HashMap;
use std::sync::{Arc, Weak};

use neighbour_wire::NeighbourMsg;
use tilo_delivery_protocol::workspace_join::WorkspaceDeviceHintV1;
use tilo_peer_state::join::{JoinBootstrap, TransportHint};
use tilo_peer_state::{CopyRole, StatePeer, TransportOrigin};
use tilo_server_protocol::{Whose, WorkSetting, WorkspaceId};
use tilo_user_identity::DeviceCert;
use tokio::sync::Mutex as AsyncMutex;
use web_time::SystemTime;

use super::links::{Link, Links};
use super::{GovernedWorkspace, Held, Workspaces, appended, peer_parts};

/// What a holder hands the device it asks to hold one of its workspaces.
#[derive(Clone, Debug)]
pub struct HostWorkspaceRequest {
    /// The workspace's UUID: its id spelled out, and nothing else
    /// ([`HostWorkspaceRequest::named`]).
    pub workspace: String,
    /// The workspace's transport shard.
    pub workspace_id: WorkspaceId,
    /// The handing holder's own signed address for the workspace.
    pub holder_hint: TransportHint,
    /// The workspace as a joining member gets it, this device
    /// admitted.
    pub bootstrap: JoinBootstrap,
    /// How this device is to hold it: with its keys, or as a store of its
    /// ciphertext. The address it answers says so.
    pub role: CopyRole,
}

impl HostWorkspaceRequest {
    /// The workspace's name, checked to be its own id in the form the id
    /// spells itself: what is kept of a workspace here is found by that id,
    /// so no text a caller wrote names a file or a held workspace.
    ///
    /// # Errors
    ///
    /// The request names its workspace by anything but its own id.
    fn named(&self) -> Result<String, String> {
        match WorkspaceId::parse(&self.workspace) {
            Some(id) if id == self.workspace_id && id.to_string() == self.workspace => {
                Ok(id.to_string())
            }
            _ => Err("a workspace is handed over under its own id and no other name".into()),
        }
    }
}

/// `text`, postcard in hex, decoded.
pub(crate) fn decode_hex<T: serde::de::DeserializeOwned>(text: &str) -> Result<T, String> {
    let bytes = hex::decode(text).map_err(|error| error.to_string())?;
    postcard::from_bytes(&bytes).map_err(|error| error.to_string())
}

/// The extension of the file beside a held workspace's own naming the
/// holder that handed it over ([`handed_by`]).
const HANDED: &str = "handed";

/// The holder that handed over the workspace kept at `path`, as the file
/// beside it names it: none for a workspace made here, or one whose file
/// does not hold up.
pub(super) fn handed_by(path: &std::path::Path) -> Option<WorkspaceDeviceHintV1> {
    let file = path.with_extension(HANDED);
    let bytes = match std::fs::read(&file) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return None,
        Err(error) => {
            tracing::warn!(path = %file.display(), %error, "the holder that handed a workspace over was not read");
            return None;
        }
    };
    let address = postcard::from_bytes::<TransportHint>(&bytes)
        .map_err(|error| error.to_string())
        .and_then(|hint| {
            WorkspaceDeviceHintV1::from_transport_hint(&hint).map_err(|error| error.to_string())
        });
    match address {
        Ok(address) => Some(address),
        Err(error) => {
            tracing::warn!(path = %file.display(), %error, "the holder that handed a workspace over is not an address");
            None
        }
    }
}

/// Aborts its task when dropped.
pub(super) struct AbortOnDrop(tokio::task::JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// The replica `bootstrap` hands over: its governance taken in from the
/// bootstrap's signed messages, its changes written under this device's own
/// actor.
fn replica(bootstrap: &JoinBootstrap, now: SystemTime) -> Result<StatePeer, String> {
    let mut peer = StatePeer::from_join(
        &bootstrap.workspace_id.to_string(),
        bootstrap.host.clone(),
        bootstrap.certified_roster.clone(),
        &bootstrap.revocations,
        now,
    )?;
    super::select_exact_admission(&mut peer)?;
    take_governance(&mut peer, bootstrap, now)?;
    if !peer.has_governance() {
        return Err("the workspace was handed over without its governance".into());
    }
    Ok(peer)
}

/// Take `bootstrap`'s governance messages into `peer`.
fn take_governance(
    peer: &mut StatePeer,
    bootstrap: &JoinBootstrap,
    now: SystemTime,
) -> Result<(), String> {
    let origin = TransportOrigin::RetainedServerDelta(bootstrap.host.device_public_key);
    for bytes in &bootstrap.governance_messages {
        let message: NeighbourMsg =
            NeighbourMsg::decode(bytes).map_err(|error| error.to_string())?;
        if matches!(message, NeighbourMsg::AppDocument { .. }) {
            continue;
        }
        peer.receive_from(origin, message, now)
            .map_err(|error| error.to_string())?;
    }
    Ok(())
}

/// The owner of the workspace `request` hands over, by their user public
/// key, as its governance says.
///
/// # Errors
///
/// The workspace's governance does not hold up.
pub(crate) fn owner(request: &HostWorkspaceRequest) -> Result<[u8; 32], String> {
    Ok(replica(&request.bootstrap, tilo_clock::wall())?.owner())
}

/// Whether `work` takes the workspace `request` hands over from `caller`,
/// for this device `own`, whoever its owners are.
pub(crate) fn work_takes(
    work: &WorkSetting,
    caller: &DeviceCert,
    own: &DeviceCert,
    request: &HostWorkspaceRequest,
) -> bool {
    // A workspace belongs to the person who governs it, not to the device it
    // was made on: every device of that person holds it, whatever work the
    // device takes from others, so it outlasts any one of them.
    if replica(&request.bootstrap, tilo_clock::wall())
        .is_ok_and(|peer| peer.owner() == own.user_public_key)
    {
        return true;
    }
    let mut whose = Whose::of_roots(caller.user_public_key, own.user_public_key);
    whose.workspace = Some(request.workspace_id);
    work.from.admits(&whose)
}

/// Why a hand-over of a workspace kept here is refused to a device its copy
/// does not name among its holders.
/// Whether the copy `held` kept here is a Servers workspace an earlier build
/// founded alike on each of its person's devices, naming the person's root as
/// its founder, which the hand-over `request` from `caller` founds again under
/// one device. It does when `caller` is a device of that person and the
/// founder the hand-over names is one of theirs the copy here names among its
/// holders. Such a copy holds only what that device carried into the new
/// founding, so the hand-over replaces it.
///
/// # Errors
///
/// The hand-over's governance does not hold up.
fn refounds(
    held: &StatePeer,
    caller: &DeviceCert,
    request: &HostWorkspaceRequest,
    now: SystemTime,
) -> Result<bool, String> {
    let owner = held.owner();
    if held.governance().founder() != Some(owner) || caller.user_public_key != owner {
        return Ok(false);
    }
    let Some(founder) = replica(&request.bootstrap, now)?.governance().founder() else {
        return Ok(false);
    };
    Ok(founder != owner
        && held.governance().keeping_devices().iter().any(|hint| {
            hint.host.device_public_key == founder && hint.host.user_public_key == owner
        }))
}

const NOT_A_HOLDER: &str =
    "the workspace kept here is another's: it is handed over only by one of its holders";

/// Why a hand-over that does not name this server among the workspace's holders
/// is refused: whom a workspace is held by is its owner's decision, signed into
/// the workspace it hands over.
const NOT_NAMED: &str = "a workspace is handed to a server only with its owner's change naming the \
     server among its holders";

/// Whether `peer`'s governance names `caller`'s device among the
/// workspace's holders, by the address it signed.
fn names_holder(peer: &StatePeer, caller: &DeviceCert) -> bool {
    peer.governance()
        .keeping_devices()
        .iter()
        .any(|hint| hint.host.device_public_key == caller.device_public_key)
}

/// That `peer`'s governance names `caller` among the workspace's holders,
/// which write it as the device it was made on does: the very certificate it
/// keeps for that holder, so another certificate of the same device key (one
/// its person never signed) writes nothing.
///
/// # Errors
///
/// It names no such holder.
pub(super) fn require_holder(
    peer: &StatePeer,
    caller: &DeviceCert,
) -> Result<(), super::CollaborationError> {
    if peer
        .governance()
        .keeping_devices()
        .iter()
        .any(|hint| hint.host == *caller)
    {
        Ok(())
    } else {
        Err(super::CollaborationError::new(
            "only a device that holds the workspace writes it",
        ))
    }
}

impl GovernedWorkspace {
    /// A writable copy of the workspace `request` hands over, kept under
    /// `dir`: its owner as the bootstrap names it, nothing of its own
    /// published yet. `peer` is the bootstrap admitted, its governance naming
    /// this server among its holders.
    fn adopt(
        path: std::path::PathBuf,
        workspace: String,
        request: &HostWorkspaceRequest,
        peer: StatePeer,
        device: Option<&super::DeviceKeys>,
    ) -> Result<Self, String> {
        let mut parts = peer_parts::PeerParts::open(&path)?;
        let entry = super::WorkspaceDocumentEntry {
            version: super::DOCUMENT_VERSION,
            workspace,
            workspace_id: request.workspace_id,
            owner: request.bootstrap.host.clone(),
            peer: parts.stage(&peer)?,
            generation: request.bootstrap.generation.max(1),
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
    /// Hold the workspace `request` hands over, `caller`'s request: a
    /// writable copy of it here, current with its governance. Answers this
    /// host's own signed address for it, saying the role it was asked to hold
    /// it in. Asked again, it takes in what the hand-over carries, and its
    /// address says the role asked now.
    ///
    /// # Errors
    ///
    /// The request does not hold up, the caller is not the holder whose
    /// address it carries, or the workspace could not be kept here.
    pub(crate) async fn hold(
        &self,
        caller: &DeviceCert,
        request: &HostWorkspaceRequest,
    ) -> Result<TransportHint, String> {
        // Refused before any path is built from it.
        let workspace = request.named()?;
        let holder = WorkspaceDeviceHintV1::from_transport_hint(&request.holder_hint)
            .map_err(|error| error.to_string())?;
        holder
            .validate_bootstrap(&request.bootstrap)
            .map_err(|error| error.to_string())?;
        if holder.workspace_id != request.workspace_id
            || request.bootstrap.workspace_id != request.workspace_id
        {
            return Err("the holder's address names another workspace".into());
        }
        if holder.device_certificate.device_public_key != caller.device_public_key {
            return Err("only a holder of the workspace hands it over".into());
        }
        let now = tilo_clock::wall();
        let admitted = self.admitted_hand_over(request, now)?;
        let mut held = self.inner.workspaces.lock().await.get(&workspace).cloned();
        if let Some(copy) = held.clone() {
            let copy = copy.lock().await;
            if refounds(&copy.peer(), caller, request, now)? {
                let workspace_id = copy.workspace_id();
                drop(copy);
                self.discard(&workspace, workspace_id).await?;
                held = None;
            }
        }
        let state = if let Some(held) = held {
            if !names_holder(&held.lock().await.peer(), caller) {
                return Err(NOT_A_HOLDER.into());
            }
            held
        } else {
            self.take_in(&workspace, caller, request, admitted, &holder, now)
                .await?
        };
        let mut governed = state.lock().await;
        self.settle(&mut governed).await?;
        // A copy another host hands over starts here without the workspace's group keys: the
        // holder that founded it elsewhere holds them. This device takes part in the group the
        // way a member copy does, asking its key holders to admit it; once they are here, the
        // sealed records it took in before open, and what this host retains is replayed.
        let _keying = self.take_part_in_keys(&workspace, Held::Governs);
        tracing::info!(%workspace, workspace_id = %request.workspace_id, role = ?request.role, "holding a workspace another host handed over");
        let hint = self
            .transport_hint_as(&governed, request.role)
            .map_err(|error| error.to_string())?;
        let holds_keys = governed
            .peer()
            .governance()
            .holder_role(&self.inner.host.cert.device_public_key)
            != Some(CopyRole::Store);
        drop(governed);
        self.list_for_person_at_hand_over(&workspace, holds_keys)
            .await;
        Ok(hint)
    }

    /// The hand-over's governance, admitted: [`replica`] of the bootstrap
    /// `request` carries, which names this server among the workspace's holders.
    ///
    /// # Errors
    ///
    /// The governance does not admit, or it names no such holder: whom a
    /// workspace is held by is its owner's decision, signed into the governance
    /// the hand-over carries, and a server holds what its owner's device gave it,
    /// never what it found.
    fn admitted_hand_over(
        &self,
        request: &HostWorkspaceRequest,
        now: SystemTime,
    ) -> Result<StatePeer, String> {
        let admitted = replica(&request.bootstrap, now)?;
        if admitted
            .governance()
            .keeping_devices()
            .iter()
            .any(|hint| hint.host == self.inner.host.cert)
        {
            Ok(admitted)
        } else {
            Err(NOT_NAMED.into())
        }
    }

    /// Take in `workspace` for the first time here: the copy `request` hands
    /// over, `admitted`, kept and served from this host's directory. Its files
    /// may be those of a copy held here before and let go.
    async fn take_in(
        &self,
        workspace: &str,
        caller: &DeviceCert,
        request: &HostWorkspaceRequest,
        admitted: StatePeer,
        holder: &WorkspaceDeviceHintV1,
        now: SystemTime,
    ) -> Result<Arc<AsyncMutex<GovernedWorkspace>>, String> {
        let path = self.inner.dir.join(format!("{workspace}.workspace"));
        let device = self.device_keys();
        let released = if path.exists() {
            let governed = GovernedWorkspace::open(path.clone(), device.as_ref())?;
            if refounds(&governed.peer(), caller, request, now)? {
                drop(governed);
                super::delete::remove_files(&self.inner.dir, workspace);
                None
            } else {
                Some(governed)
            }
        } else {
            None
        };
        let mut governed = if let Some(governed) = released {
            // Held here before and let go: its files are where it
            // stands, and the hand-over brings what it missed, from one
            // of the holders those files name.
            if !names_holder(&governed.peer(), caller) {
                return Err(NOT_A_HOLDER.into());
            }
            let _ = std::fs::remove_file(path.with_extension(super::RELEASED));
            let mut peer = governed.peer().clone();
            take_governance(&mut peer, &request.bootstrap, now)?;
            *governed.peer.lock() = peer;
            governed
        } else {
            GovernedWorkspace::adopt(
                path,
                workspace.to_owned(),
                request,
                admitted,
                device.as_ref(),
            )?
        };
        self.bind(&governed).await?;
        // Kept with the workspace: the holder that handed it over is one this
        // server syncs from after a restart, before its sessions to the other
        // holders its governance names are up.
        tilo_files::native::atomic::write(
            &governed.path.with_extension(HANDED),
            &postcard::to_stdvec(&request.holder_hint).map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
        governed.handed_by = Some(holder.clone());
        let reading = governed.reading();
        let state = Arc::new(AsyncMutex::new(governed));
        self.inner.readings.hold(workspace, &state, reading);
        self.inner
            .workspaces
            .lock()
            .await
            .insert(workspace.to_owned(), Arc::clone(&state));
        let governed = state.lock().await;
        self.serve_direct(&governed)?;
        drop(governed);
        Ok(state)
    }

    /// Write `person`'s list of workspaces where `workspace`, just taken in, is
    /// their own: the list a login reads from the server they log in at. The
    /// device that signs the hand-over also names the server among its holders,
    /// so the server writes what it knows of theirs as the holder it is, from
    /// the hand-over and not after a later sync. A store holds the workspace's
    /// ciphertext alone and writes nothing of it.
    async fn list_for_person_at_hand_over(&self, workspace: &str, holds_keys: bool) {
        let person = self.inner.host.cert.user_public_key;
        if !holds_keys || workspace != tilo_server_protocol::servers::person_workspace_id(&person) {
            return;
        }
        // The records are sealed: the group's keys come from the other holders
        // over the workspace's link. They are waited for here, before the write
        // takes the workspace's lock, because the key service publishes what it
        // settles through that same lock: a write waiting for the keys while it
        // holds the lock waits for itself.
        let Some(device) = self.device_keys() else {
            // This host keeps no group keys: what it writes is in the clear.
            self.list_known_for(&person).await;
            return;
        };
        let keys_here = tokio::time::timeout(
            crate::own_device::OPEN_WAIT,
            super::sealed_documents::wait_for_group_keys(&device, workspace),
        )
        .await
        .unwrap_or(false);
        if keys_here {
            self.list_known_for(&person).await;
        } else {
            tracing::warn!(%workspace, "the person's list of workspaces was not written: the workspace's group has not admitted this server");
        }
    }

    /// Let `workspace` go, `caller`'s request: another device of this one's own
    /// person or of the workspace's owner. Whether it was held here.
    ///
    /// # Errors
    ///
    /// The caller may not ask, or the release could not be recorded.
    pub(crate) async fn unhold(
        &self,
        caller: &DeviceCert,
        workspace: &str,
    ) -> Result<bool, String> {
        // Named by its id, as it was handed over: no other text is looked up
        // among the files here.
        WorkspaceId::parse(workspace).ok_or_else(|| "a workspace is named by its id".to_owned())?;
        // A workspace being let go as its governance stopped naming this device
        // is gone here once this answers.
        drop(self.inner.releasing.lock().await);
        let Ok(state) = self.workspace(workspace).await else {
            return Ok(false);
        };
        let owner = state.lock().await.peer().owner();
        if caller.user_public_key != self.inner.host.cert.user_public_key
            && caller.user_public_key != owner
        {
            return Err(
                "only its own person or the workspace's owner asks a host to let it go".into(),
            );
        }
        match self.release(workspace).await {
            Ok(()) => Ok(true),
            // Let go meanwhile, as its governance stopped naming this device.
            Err(_) if self.workspace(workspace).await.is_err() => Ok(true),
            Err(error) => Err(error),
        }
    }

    /// Keep a link to the other holders `governed`'s governance names, and
    /// to the holder that handed it over; let the workspace go once its
    /// governance named this device and names it no more.
    ///
    /// Every holder links to the others over the device link, exactly once
    /// per pair: the holder with the greatest device key among them is
    /// dialled by the others and dials none, so each keeps one link into the
    /// workspace's history beside the ones it answers.
    pub(super) fn dial_holders(&self, governed: &mut GovernedWorkspace) {
        let host = &self.inner.host;
        let own = host.cert.device_public_key;
        let mut named: Vec<WorkspaceDeviceHintV1> = governed
            .peer()
            .governance()
            .keeping_devices()
            .iter()
            .filter_map(|hint| WorkspaceDeviceHintV1::from_transport_hint(hint).ok())
            .collect();
        let names_this = named
            .iter()
            .any(|holder| holder.device_certificate.device_public_key == own);
        if governed.holders.named && !names_this {
            governed.holders = Holders::default();
            let workspace = governed.workspace().to_owned();
            let this = self.clone();
            tokio::spawn(async move {
                tracing::info!(%workspace, "the workspace's governance no longer names this host");
                if let Err(error) = this.release(&workspace).await {
                    tracing::warn!(%workspace, %error, "a workspace no longer held here was not let go");
                }
            });
            return;
        }
        governed.holders.named |= names_this;
        named.extend(governed.handed_by.clone());
        let links = governed.holders.links.clone();
        governed.holders.sessions.retain(|device, _| {
            let kept = named
                .iter()
                .any(|holder| holder.device_certificate.device_public_key == *device);
            if !kept {
                links.forget(device);
            }
            kept
        });
        if governed.holders.reporting.is_none() {
            governed.holders.reporting = Some(AbortOnDrop(tokio::spawn(report_copy(
                Arc::downgrade(&self.inner),
                governed.workspace().to_owned(),
                links.clone(),
            ))));
        }
        let workspace_id = governed.workspace_id();
        // The one holder this device dials: the greatest device key among the
        // others, or none when it is its own.
        let Some(dialled) = named
            .iter()
            .map(|holder| holder.device_certificate.device_public_key)
            .filter(|device| *device != own)
            .max()
        else {
            return;
        };
        if named
            .iter()
            .map(|holder| holder.device_certificate.device_public_key)
            .chain(std::iter::once(own))
            .max()
            != Some(own)
            && !governed.holders.sessions.contains_key(&dialled)
        {
            let Some(holder) = named
                .iter()
                .find(|holder| holder.device_certificate.device_public_key == dialled)
            else {
                return;
            };
            let Some(link) = self.inner.direct.link(workspace_id) else {
                return;
            };
            let endpoint = host.endpoint.clone();
            let addr = holder.endpoint_addr.clone();
            let name = governed.workspace().to_owned();
            let dialing = tokio::spawn(dial_holder(
                link,
                endpoint,
                addr,
                dialled,
                links,
                name,
                Arc::downgrade(&self.inner),
            ));
            governed
                .holders
                .sessions
                .insert(dialled, AbortOnDrop(dialing));
        }
    }
}

/// Dial `device` at `addr` over the workspace's link until the workspace is
/// let go here, taking in what it holds and answering it: what this holder's
/// copy lacks of the other, and what it publishes, go over that one link.
async fn dial_holder(
    link: tilo_peer_state::direct::DirectSync,
    endpoint: iroh::Endpoint,
    addr: iroh_base::EndpointAddr,
    device: [u8; 32],
    links: Links,
    name: String,
    workspaces: Weak<super::WorkspacesInner>,
) {
    const FIRST: std::time::Duration = std::time::Duration::from_secs(1);
    const MOST: std::time::Duration = std::time::Duration::from_secs(60);
    let mut backoff = FIRST;
    loop {
        links.set(device, Link::Reaching);
        match link.dial(&endpoint, addr.clone(), device).await {
            Ok(dialed) => {
                backoff = FIRST;
                links.set(device, Link::CatchingUp);
                let mut caught_up = link.caught_up();
                let run = dialed.run(&link);
                tokio::pin!(run);
                loop {
                    tokio::select! {
                        ended = &mut run => {
                            if let Err(refusal) = ended {
                                tracing::debug!(%name, ?refusal, "a holder's link ended");
                            }
                            break;
                        }
                        changed = caught_up.changed() => {
                            if changed.is_err() {
                                continue;
                            }
                            let current = if *caught_up.borrow() {
                                Link::Current
                            } else {
                                Link::CatchingUp
                            };
                            links.set(device, current);
                        }
                    }
                }
                links.set(
                    device,
                    Link::Unreachable {
                        since_ns: tilo_clock::unix_now_ns(),
                    },
                );
            }
            Err(refusal) => {
                tracing::debug!(%name, ?refusal, "a holder is unreachable");
                // A key its person's root retired is told typed, with the entry the root
                // signed: this device keeps it only once the entry verifies, as an announced
                // revocation is, so every workspace here refuses the key and a retired server
                // lets go of what it holds.
                if let Some(inner) = workspaces.upgrade() {
                    (Workspaces { inner }).take_refusal(refusal).await;
                }
                links.set(
                    device,
                    Link::Unreachable {
                        since_ns: tilo_clock::unix_now_ns(),
                    },
                );
            }
        }
        tokio::time::sleep(backoff).await;
        backoff = backoff.saturating_mul(2).min(MOST);
    }
}

/// Say in `workspace`'s state where this device's copy stands, each time a
/// session to another holder changes how it stands, until the workspace is
/// let go here.
async fn report_copy(workspaces: Weak<super::WorkspacesInner>, workspace: String, links: Links) {
    // Said once at the start, also by a holder no other holder is named
    // beside.
    links.touch();
    let mut before = std::collections::BTreeMap::new();
    loop {
        links.changed().await;
        let Some(inner) = workspaces.upgrade() else {
            return;
        };
        let this = Workspaces { inner };
        // A holder this copy turned current with holds what this device
        // asked the workspace for and nobody answered: it is asked again now.
        let now = links.now();
        if now
            .iter()
            .any(|(device, link)| *link == Link::Current && before.get(device) != Some(link))
        {
            drop(this.inner.reached.send(workspace.clone()));
        }
        before = now;
        let held = held_copy(&links);
        let cert = this.device_cert().clone();
        let reported = this
            .author_own_workspace_state(&workspace, &cert, |peer| {
                peer.workspace_mut()
                    .report_copy(&cert, &held, tilo_clock::wall())
            })
            .await;
        if let Err(error) = reported {
            // A holder that is not a member of the workspace writes nothing
            // in it: the holders that reach it say where it stands.
            tracing::debug!(%workspace, %error, "this holder's copy was not said");
        }
    }
}

/// Where this device's copy stands, as its sessions to the other holders
/// do, with the holders it holds changes back from for lack of history and
/// the content it lacks.
fn held_copy(links: &Links) -> tilo_peer_state::HeldCopy {
    let mut held = tilo_peer_state::HeldCopy {
        catching_up_from: links.held_from(),
        content_missing: links.content_missing(),
        ..tilo_peer_state::HeldCopy::default()
    };
    for (device, link) in &links.now() {
        match link {
            Link::Reaching | Link::CatchingUp => {
                held.catching_up_from.insert(*device);
            }
            Link::Unreachable { since_ns } => {
                held.unreachable.insert(*device, *since_ns);
            }
            Link::Current => {}
        }
    }
    held
}

impl Workspaces {
    /// Each workspace whose copy here turned current with another of its
    /// holders: what it lacks of the content its files name, the holder
    /// may keep. A slow receiver may lag; it then asks for all it lacks.
    #[must_use]
    pub fn holders_reached(&self) -> tokio::sync::broadcast::Receiver<String> {
        self.inner.reached.subscribe()
    }

    /// `workspace`'s copy here lacks `blobs` of the content its files
    /// name: every holder reads it in the workspace's copy status.
    pub async fn lacks_content(&self, workspace: &str, blobs: u32) {
        if let Ok(governed) = self.workspace(workspace).await {
            governed.lock().await.holders.links().lacks_content(blobs);
        }
    }

    /// Where `workspace`'s copies stand: what it wants, each holder, and
    /// which of them is short of what.
    ///
    /// # Errors
    ///
    /// The workspace is not held here.
    pub async fn copies(
        &self,
        workspace: &str,
    ) -> Result<tilo_peer_state::CopyStatus, super::CollaborationError> {
        let governed = self.workspace(workspace).await?;
        let status = governed.lock().await.peer().workspace().copies();
        Ok(status)
    }

    /// Have `workspace` want `policy`, or the default again for `None`, as its
    /// owners chose.
    ///
    /// # Errors
    ///
    /// The workspace is not held here, or the choice could not be kept.
    pub async fn choose_copies(
        &self,
        workspace: &str,
        policy: Option<tilo_peer_state::CopyPolicy>,
    ) -> Result<(), String> {
        self.name_host(workspace, |governance, key, now| {
            governance.author_copy_policy(policy, key, now)
        })
        .await
    }
}

/// This device's sessions to a workspace's other holders.
#[derive(Default)]
pub(super) struct Holders {
    /// A session to each other holder, by device.
    sessions: HashMap<[u8; 32], AbortOnDrop>,
    /// Where each of those sessions stands.
    links: Links,
    /// What says in the workspace where this device's copy stands.
    reporting: Option<AbortOnDrop>,
    /// Whether the workspace's governance has named this device among its
    /// holders: once it has, naming it no more lets the workspace go here.
    named: bool,
    /// Changes of the workspace another holder sent that build on history
    /// this device lacks, by that holder: taken in once the history is.
    held: Vec<([u8; 32], NeighbourMsg)>,
}

/// The most changes held back for history at once: the oldest goes first,
/// and the next catch-up from its holder brings it again.
const MOST_HELD: usize = 256;

impl Holders {
    /// Hold `message` of the holder `from` back until the history it builds
    /// on arrives.
    pub(super) fn hold(&mut self, from: [u8; 32], message: NeighbourMsg) {
        if self.held.len() >= MOST_HELD {
            self.held.remove(0);
        }
        self.held.push((from, message));
        self.say_held();
    }

    /// The changes held back, taken to be tried again.
    pub(super) fn take_held(&mut self) -> Vec<([u8; 32], NeighbourMsg)> {
        std::mem::take(&mut self.held)
    }

    /// Say in the copy's links which holders changes are held back from.
    pub(super) fn say_held(&self) {
        self.links
            .hold_from(self.held.iter().map(|(from, _)| *from).collect());
    }

    /// Where this device's copy stands against the other holders.
    pub(super) fn links(&self) -> &Links {
        &self.links
    }
}
