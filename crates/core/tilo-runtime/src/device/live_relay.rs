//! The live session of the workspaces this device serves, on the device's one endpoint.
//!
//! A workspace's live session runs through one relay, the device the workspace's state names as
//! serving it. The daemon holds the device's endpoint, so it is the daemon that relays: it serves
//! [`tilo_live::relay::Relay`] on [`tilo_live::native::ALPN`] beside the device's other
//! protocols, and admits a connection for a workspace it governs only when the certificate the
//! grant presents is the roster's own and the handshake proved the key ([`bind`]). It cuts a
//! session when the workspace's membership, keys or calls move.
//!
//! The person's desktop session captures, encodes and draws, and takes part like any other
//! device: a `moq-lite` client, over a stream of the daemon's socket
//! ([`tilo_live::link`]). The session names where the daemon takes the stream: into this relay,
//! for a workspace this device serves, or on to the relay of the device serving it, which the
//! daemon dials from its endpoint and splices to the stream ([`Live::serve_link`]). Either way the
//! relay judges the session's grant as it judges every device's.
//!
//! A relay tells a device it turns away nothing but that, so what it turned away and why is kept
//! here, bounded, and the journal says it when it is news ([`TurnedAway`]). It is this device's
//! own resource too ([`link::TURNED_AWAY`], [`TurnedAwayReads`]): the person's desktop session
//! follows it and shows each workspace's beside its live session.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use collab_types::scope::Scope;
use tilo_actions::remote::{
    ActionCallError, Caller, Pending, RemoteActionMessage, ResourceAddress, ResourceCopy,
    ResourceReadRequest, ResourceReads, ResourceWake,
};
use tilo_desktop_model::WorkspaceId;
use tilo_live::grant;
use tilo_live::link;
use tilo_live::live::{self, Namespace};
use tilo_live::native;
use tilo_live::relay::{
    Admission, Denied, Direction, Peer, Relay, RelayAuthority, SessionAuthorization,
};
use tilo_lock::MutexRecover as _;
use tilo_session_client::workspace_keys::{Device, admitted_devices};
use tilo_user_identity::{DeviceCert, EndpointDelegation};
use tilo_workspace_service::workspaces::Held;

use super::workspaces::ServiceWorkspaces;

/// How often the workspaces' membership, keys and calls are read again when nothing announced a
/// change: a removal the key service did not announce still cuts its sessions within this.
const REREAD: Duration = Duration::from_secs(30);

/// The scope one workspace's live session carries.
fn scope_of(workspace: WorkspaceId) -> Scope {
    Scope::workspace(workspace.to_string())
}

/// Now, in Unix nanoseconds.
fn now_ns() -> u64 {
    tilo_clock::unix_now_ns()
}

/// Why the relay turned a device away from a workspace's live session. The device is told only
/// that it was not let in: the journal and [`Live::turned_away`] say why.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Refusal {
    /// The grant names no workspace.
    Unparsed,
    /// Who is in the workspace cannot be read on this relay.
    Unread,
    /// The grant asks for a scope this relay carries no live session for.
    Scope,
    /// This device does not relay the workspace: it does not serve it.
    NotRelayedHere,
    /// The certificate the grant carries does not read as one.
    CertificateUnread,
    /// The certificate the grant carries ran out.
    CertificateExpired,
    /// The workspace holds no device of the certificate's key.
    NotAMember,
    /// The workspace holds another certificate for the device than the one it presented.
    CertificateDiffers,
    /// The connection's endpoint is not the device, and the device did not sign for it, or that
    /// signature ran out.
    Endpoint,
    /// The device seals under keys this relay already moved past: it presented `presented`, and
    /// the workspace's media channel is at `newest` here.
    KeysBehind { presented: u64, newest: u64 },
}

impl Refusal {
    /// The word the journal says it by: what a tool branches on.
    pub(crate) const fn word(&self) -> &'static str {
        match self {
            Self::Unparsed => "grant-unparsed",
            Self::Unread => "membership-unread",
            Self::Scope => "scope-not-carried",
            Self::NotRelayedHere => "not-relayed-here",
            Self::CertificateUnread => "certificate-unread",
            Self::CertificateExpired => "certificate-expired",
            Self::NotAMember => "not-a-member",
            Self::CertificateDiffers => "certificate-differs",
            Self::Endpoint => "endpoint-not-the-device",
            Self::KeysBehind { .. } => "keys-behind",
        }
    }
}

/// One endpoint the relay turned away from a workspace's live session, and has not let in since.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TurnedAway {
    /// The endpoint the handshake proved: a device's key, or the key a page dials under.
    pub(crate) peer: [u8; 32],
    /// What its person called it, when it is a device the workspace holds and they named it.
    pub(crate) name: String,
    /// Why it was turned away the last time it asked.
    pub(crate) why: Refusal,
    /// How often it asked and was turned away since it was last let in.
    pub(crate) times: u32,
}

impl TurnedAway {
    /// How the reading says it: the endpoint's key, what its person called it, why it was turned
    /// away (with the epochs, for keys behind) and how often.
    fn row(&self) -> serde_json::Value {
        let (presented, newest) = match self.why {
            Refusal::KeysBehind { presented, newest } => (Some(presented), Some(newest)),
            _ => (None, None),
        };
        serde_json::json!({
            "device": hex::encode(self.peer),
            "name": self.name,
            "why": self.why.word(),
            "presented": presented,
            "newest": newest,
            "times": self.times,
        })
    }
}

/// How many endpoints the relay keeps as turned away per workspace: it answers whoever dials it,
/// so what it remembers of them is bounded.
const TURNED_AWAY_KEPT: usize = 8;

/// Note in `turned` that `peer` was turned away for `why`. Answers whether that is news: the
/// endpoint was not turned away before, or for another reason.
fn note_turned_away(
    turned: &mut BTreeMap<[u8; 32], TurnedAway>,
    peer: [u8; 32],
    name: String,
    why: Refusal,
) -> bool {
    if let Some(known) = turned.get_mut(&peer) {
        let news = known.why.word() != why.word();
        known.why = why;
        known.name = name;
        known.times = known.times.saturating_add(1);
        return news;
    }
    if turned.len() >= TURNED_AWAY_KEPT {
        turned.pop_first();
    }
    turned.insert(
        peer,
        TurnedAway {
            peer,
            name,
            why,
            times: 1,
        },
    );
    true
}

/// Whether a device that presented the keys of `presented` seals under keys a relay that holds
/// `newest` still opens: a device behind would publish nothing anyone opens.
const fn keys_current(presented: u64, newest: u64) -> Result<(), Refusal> {
    if presented < newest {
        return Err(Refusal::KeysBehind { presented, newest });
    }
    Ok(())
}

/// What its person called `device`, by the certificate `devices` holds of it.
fn name_of(devices: &[Device], device: [u8; 32]) -> String {
    devices
        .iter()
        .find(|held| held.device == device)
        .and_then(|held| tilo_user_identity::device_name(&held.cert.device_label))
        .unwrap_or_default()
        .to_owned()
}

/// What one granted preface asked for, once its certificate is bound to the handshake and to the
/// workspace's roster.
struct Request {
    workspace: WorkspaceId,
    device: [u8; 32],
}

/// Bind a grant to the transport and the roster:
///
/// * the certificate is byte for byte the row the workspace's governance holds for its device —
///   the membership every device already syncs, so a device presenting another device's
///   certificate, or one the workspace removed, is refused;
/// * the connection's endpoint speaks for that device: it is the device's own key, or one the
///   device signed an [`EndpointDelegation`] for, live now, which is how a page whose device key
///   cannot leave `WebCrypto` dials;
/// * the certificate has not expired, and the workspace is one this device relays now
///   (`relays`): one it serves, while it is a member.
fn bind(
    peer: Peer,
    grant: &grant::Native,
    devices: &[Device],
    relays: bool,
    now_ns: u64,
) -> Result<Request, Refusal> {
    let Peer(endpoint) = peer;
    let workspace =
        WorkspaceId::parse(&live::workspace_id(grant.workspace)).ok_or(Refusal::Unparsed)?;
    // One scope per workspace today: the workspace itself.
    if grant.scope != scope_of(workspace).id() {
        return Err(Refusal::Scope);
    }
    if !relays {
        return Err(Refusal::NotRelayedHere);
    }
    let certificate: DeviceCert =
        serde_json::from_slice(&grant.certificate).map_err(|_| Refusal::CertificateUnread)?;
    if certificate.is_expired_at(now_ns) {
        return Err(Refusal::CertificateExpired);
    }
    let device = certificate.device_public_key;
    let admitted = devices
        .iter()
        .find(|admitted| admitted.device == device)
        .ok_or(Refusal::NotAMember)?;
    if certificate != admitted.cert {
        return Err(Refusal::CertificateDiffers);
    }
    if endpoint != device {
        let delegation: EndpointDelegation =
            serde_json::from_slice(&grant.delegation).map_err(|_| Refusal::Endpoint)?;
        if delegation.device_public_key != device {
            return Err(Refusal::Endpoint);
        }
        delegation
            .verify(&endpoint, now_ns)
            .map_err(|_| Refusal::Endpoint)?;
    }
    Ok(Request { workspace, device })
}

/// Whether `own` relays a workspace that `held` says how this device holds, whose members are
/// `devices`: it serves the workspace, and is a member whose certificate is in force.
fn relays_here(held: Option<Held>, devices: &[Device], own: [u8; 32], now_ns: u64) -> bool {
    matches!(held, Some(Held::Governs))
        && devices
            .iter()
            .any(|device| device.device == own && !device.cert.is_expired_at(now_ns))
}

/// What each call of each workspace this relay carries allows, by the tag its tracks name
/// ([`collab_types::calls::tag`]).
type CallsByTag =
    Arc<Mutex<BTreeMap<WorkspaceId, BTreeMap<String, collab_types::calls::CallRule>>>>;

/// One admitted session: its publication, the scope it hears, and the watch it is cut by.
struct Permit {
    workspaces: Arc<ServiceWorkspaces>,
    changed: tokio::sync::watch::Receiver<u64>,
    /// What the workspace's calls allow: the call tracks it carries for this session.
    calls: CallsByTag,
    /// The person the session's device is (their root as hex).
    person: String,
    device: [u8; 32],
    workspace: WorkspaceId,
    workspace_name: String,
    scope: String,
    publication: String,
    prefix: String,
}

impl SessionAuthorization for Permit {
    fn publication(&self) -> Option<&str> {
        Some(&self.publication)
    }

    fn subscriptions(&self) -> &[String] {
        // Every namespace under the scope is offered, and each is judged as it is offered: a device
        // that joins after this session opened is heard without a new session.
        &[]
    }

    fn subscription_prefix(&self) -> Option<&str> {
        Some(&self.prefix)
    }

    fn current(&self, direction: Direction, namespace: &str) -> bool {
        let Some(name) = Namespace::parse(namespace) else {
            return false;
        };
        if name.workspace_id() != self.workspace_name || name.scope_id() != self.scope {
            return false;
        }
        // The workspace's membership as it is now: a device it removed, or one whose certificate
        // ran out, is not admitted any more.
        let Some(Ok(devices)) = devices_now(&self.workspaces, self.workspace) else {
            return false;
        };
        if !devices.iter().any(|device| device.device == name.device()) {
            return false;
        }
        match direction {
            Direction::Publish => name.device() == self.device,
            Direction::Subscribe => true,
        }
    }

    fn changed(&self) -> Option<tokio::sync::watch::Receiver<u64>> {
        Some(self.changed.clone())
    }

    fn call(&self, direction: Direction, call: &str) -> bool {
        let may = self
            .calls
            .lock_recover()
            .get(&self.workspace)
            .and_then(|calls| calls.get(call))
            .map_or(collab_types::calls::Directions::BOTH, |rule| {
                rule.may(Some(&self.person))
            });
        match direction {
            Direction::Publish => may.publish,
            Direction::Subscribe => may.subscribe,
        }
    }
}

/// How this device holds `workspace` now and the devices it admits; `None` for a workspace it
/// does not hold.
fn read(
    workspaces: &ServiceWorkspaces,
    workspace: WorkspaceId,
) -> Option<(Held, Result<Vec<Device>, String>)> {
    workspaces
        .copies()
        .read_now(&workspace.to_string(), admitted_devices)
}

/// The devices `workspace` admits now, as this device holds it.
fn devices_now(
    workspaces: &ServiceWorkspaces,
    workspace: WorkspaceId,
) -> Option<Result<Vec<Device>, String>> {
    read(workspaces, workspace).map(|(_, devices)| devices)
}

/// What the relay's judgement shares with the sessions it admitted: the device's workspaces, the
/// epochs, rosters and calls it last read, and the endpoints it turned away.
struct Shared {
    workspaces: Arc<ServiceWorkspaces>,
    device: [u8; 32],
    /// The newest content-key epoch this device holds of each workspace: a grant that predates a
    /// rotation this device already applied is refused.
    epochs: Mutex<BTreeMap<WorkspaceId, u64>>,
    /// The devices each workspace admitted when this device last read it: a member the
    /// workspace removed is a change the open sessions re-observe, so its session is cut on the
    /// removal and not on its next request.
    rosters: Mutex<BTreeMap<WorkspaceId, BTreeSet<[u8; 32]>>>,
    /// What each workspace's calls allowed when this device last read it.
    calls: CallsByTag,
    /// Told when a workspace's membership, keys or calls moved.
    changed: tokio::sync::watch::Sender<u64>,
    /// The endpoints the relay turned away from each workspace's live session, and why.
    turned: Mutex<BTreeMap<WorkspaceId, BTreeMap<[u8; 32], TurnedAway>>>,
    /// Moves with every change to [`Self::turned`]: the revision its reading stands at.
    turned_revision: AtomicU64,
    /// The reads of [`Self::turned`] waiting for it to move.
    turned_waiting: Mutex<Vec<ResourceWake>>,
}

impl RelayAuthority for Shared {
    fn admit<'a>(&'a self, peer: Peer, proof: &'a [u8]) -> Admission<'a> {
        Box::pin(std::future::ready(self.decide(peer, proof)))
    }

    /// An endpoint none of the workspaces this relay read names as a device. A relay that has
    /// read none yet calls nobody a stranger; this device's own session never is one.
    fn stranger(&self, peer: Peer) -> bool {
        let Peer(endpoint) = peer;
        if endpoint == self.device {
            return false;
        }
        let rosters = self.rosters.lock_recover();
        !rosters.is_empty() && !rosters.values().any(|devices| devices.contains(&endpoint))
    }
}

impl Shared {
    /// Whether `peer` may take part in the live session its grant (`proof`) names, from what this
    /// device holds of the workspace now.
    fn decide(&self, peer: Peer, proof: &[u8]) -> Result<Arc<dyn SessionAuthorization>, Denied> {
        let Peer(endpoint) = peer;
        let named = grant::Native::parse(proof).and_then(|grant| {
            let workspace = WorkspaceId::parse(&live::workspace_id(grant.workspace))?;
            Some((grant, workspace))
        });
        let Some((grant, workspace)) = named else {
            tracing::debug!(peer = %hex::encode(endpoint), why = Refusal::Unparsed.word(),
                "a device was turned away from the live session");
            return Err(Denied);
        };
        let read = read(&self.workspaces, workspace);
        let held = read.as_ref().map(|(held, _)| *held);
        let devices = read.map(|(_, devices)| devices);
        let judged = match &devices {
            Some(Ok(devices)) => self.judge(peer, &grant, workspace, held, devices),
            Some(Err(_)) => Err(Refusal::Unread),
            None => Err(Refusal::NotRelayedHere),
        };
        match judged {
            Ok(permit) => {
                self.let_in(workspace, endpoint);
                Ok(permit)
            }
            Err(why) => {
                let name = match &devices {
                    Some(Ok(devices)) => name_of(devices, endpoint),
                    _ => String::new(),
                };
                self.turned_away(workspace, endpoint, name, why);
                Err(Denied)
            }
        }
    }

    /// [`Self::decide`] for a grant that names `workspace`, held here as `held`, whose devices
    /// are `devices`: the session it is admitted as, or why it is turned away.
    fn judge(
        &self,
        peer: Peer,
        grant: &grant::Native,
        workspace: WorkspaceId,
        held: Option<Held>,
        devices: &[Device],
    ) -> Result<Arc<dyn SessionAuthorization>, Refusal> {
        let now = now_ns();
        let relays = relays_here(held, devices, self.device, now);
        let request = bind(peer, grant, devices, relays, now)?;
        let newest = self
            .epochs
            .lock_recover()
            .get(&workspace)
            .copied()
            .unwrap_or(0);
        keys_current(grant.epoch, newest)?;
        let scope = scope_of(request.workspace);
        let person = devices
            .iter()
            .find(|admitted| admitted.device == request.device)
            .map(|admitted| hex::encode(admitted.cert.user_public_key))
            .unwrap_or_default();
        Ok(Arc::new(Permit {
            workspaces: Arc::clone(&self.workspaces),
            changed: self.changed.subscribe(),
            calls: Arc::clone(&self.calls),
            person,
            device: request.device,
            workspace: request.workspace,
            workspace_name: request.workspace.to_string(),
            publication: Namespace::new(&scope, request.device).path(),
            prefix: live::devices(&scope),
            scope: scope.id(),
        }))
    }

    /// `peer` was turned away from `workspace`'s live session for `why`: kept, and said in the
    /// journal when that is news.
    fn turned_away(&self, workspace: WorkspaceId, peer: [u8; 32], name: String, why: Refusal) {
        let (presented, newest) = match why {
            Refusal::KeysBehind { presented, newest } => (Some(presented), Some(newest)),
            _ => (None, None),
        };
        let news = {
            let mut turned = self.turned.lock_recover();
            note_turned_away(turned.entry(workspace).or_default(), peer, name, why)
        };
        self.turned_moved();
        if news {
            tracing::warn!(%workspace, peer = %hex::encode(peer), why = why.word(), ?presented,
                ?newest, "a device was turned away from this workspace's live session");
        } else {
            tracing::debug!(%workspace, peer = %hex::encode(peer), why = why.word(), ?presented,
                ?newest, "a device was turned away from this workspace's live session again");
        }
    }

    /// `peer` was let into `workspace`'s live session: it is no longer one the relay turned away.
    fn let_in(&self, workspace: WorkspaceId, peer: [u8; 32]) {
        let was = self
            .turned
            .lock_recover()
            .get_mut(&workspace)
            .and_then(|turned| turned.remove(&peer));
        if let Some(was) = was {
            self.turned_moved();
            tracing::info!(%workspace, peer = %hex::encode(peer), why = was.why.word(),
                times = was.times,
                "a device this relay turned away is in the workspace's live session now");
        }
    }

    /// Read `workspace`'s membership, keys and calls as they are now, and tell the sessions open
    /// on this relay when any moved.
    fn observe(&self, workspace: WorkspaceId) {
        let Some((held, Ok(devices))) = read(&self.workspaces, workspace) else {
            self.withdraw(workspace);
            return;
        };
        if !matches!(held, Held::Governs) {
            self.withdraw(workspace);
            return;
        }
        let mut moved = false;
        let roster: BTreeSet<[u8; 32]> = devices.iter().map(|device| device.device).collect();
        let before = self
            .rosters
            .lock_recover()
            .insert(workspace, roster.clone());
        if let Some(before) = before {
            moved |= before != roster;
        }
        let (rules, _) = self
            .workspaces
            .copies()
            .read_now(&workspace.to_string(), |peer| {
                peer.governance().call_gate(tilo_clock::wall())
            })
            .and_then(|(_, gate)| gate.ok())
            .unwrap_or_default();
        let by_tag: BTreeMap<String, collab_types::calls::CallRule> = rules
            .iter()
            .map(|(channel, rule)| (collab_types::calls::tag(channel), rule.clone()))
            .collect();
        moved |= self.calls.lock_recover().insert(workspace, by_tag.clone()) != Some(by_tag);
        if let Some(tilo_workspace_keys::KeyAccess::Ready(keys)) = self
            .workspaces
            .keys()
            .get()
            .map(|keys| keys.access(workspace).0)
        {
            let epoch = tilo_live::frame::LiveKeys::from(keys).epoch();
            let mut epochs = self.epochs.lock_recover();
            let newer = epochs.get(&workspace).is_none_or(|newest| *newest < epoch);
            if newer {
                epochs.insert(workspace, epoch);
            }
            drop(epochs);
            moved |= newer;
        }
        if moved {
            self.bump();
        }
    }

    /// Stop carrying `workspace`: its sessions are cut on the change.
    fn withdraw(&self, workspace: WorkspaceId) {
        let had = self.rosters.lock_recover().remove(&workspace).is_some();
        self.calls.lock_recover().remove(&workspace);
        if had {
            self.bump();
        }
    }

    /// [`Self::turned`] moved: its reading's revision moves past the one each waiting read had.
    fn turned_moved(&self) {
        self.turned_revision.fetch_add(1, Ordering::Relaxed);
        let waiting = std::mem::take(&mut *self.turned_waiting.lock_recover());
        for wake in waiting {
            wake.wake();
        }
    }

    /// The endpoints turned away from each workspace's live session and not let in since, by
    /// workspace, as the reading says them.
    fn turned_rows(&self) -> BTreeMap<String, Vec<serde_json::Value>> {
        self.turned
            .lock_recover()
            .iter()
            .filter(|(_, turned)| !turned.is_empty())
            .map(|(workspace, turned)| {
                (
                    workspace.to_string(),
                    turned.values().map(TurnedAway::row).collect(),
                )
            })
            .collect()
    }

    fn bump(&self) {
        self.changed
            .send_modify(|count| *count = count.wrapping_add(1));
    }
}

/// The device's live session: the relay of the workspaces it serves, and the links of the
/// person's desktop session into it or on to another device's.
pub(crate) struct Live {
    shared: Arc<Shared>,
    relay: Relay,
    endpoint: iroh::Endpoint,
    /// Whether this daemon serves the relay on its endpoint itself; a server's relay is the one
    /// its keeping serves.
    serves: bool,
}

impl Live {
    /// The live session of `workspaces`' device, on `relay` when its keeping already runs one (a
    /// server's), and on a relay of its own otherwise. Must be called inside the daemon's runtime.
    pub(crate) fn new(workspaces: &Arc<ServiceWorkspaces>, relay: Option<Relay>) -> Arc<Self> {
        let (changed, _) = tokio::sync::watch::channel(0);
        let shared = Arc::new(Shared {
            workspaces: Arc::clone(workspaces),
            device: workspaces.device_cert().device_public_key,
            epochs: Mutex::default(),
            rosters: Mutex::default(),
            calls: Arc::default(),
            changed,
            turned: Mutex::default(),
            turned_revision: AtomicU64::new(0),
            turned_waiting: Mutex::default(),
        });
        // Whom the relay turned away, and why: what the person's desktop session shows. It is
        // this computer's relay alone that turned them away.
        workspaces.copies().publish_own(
            link::TURNED_AWAY,
            tilo_workspace_service::workspaces::OwnReaders::ThisDevice,
            Arc::new(TurnedAwayReads(Arc::clone(&shared))),
        );
        let serves = relay.is_none();
        let relay =
            relay.unwrap_or_else(|| Relay::new(Arc::clone(&shared) as Arc<dyn RelayAuthority>));
        let live = Arc::new(Self {
            shared,
            relay,
            endpoint: workspaces.endpoint(),
            serves,
        });
        live.follow();
        live
    }

    /// What the device's endpoint answers for the live session: nothing where its keeping
    /// already answers the relay's protocol.
    pub(crate) fn protocols(
        &self,
    ) -> Vec<(&'static [u8], Box<dyn iroh::protocol::DynProtocolHandler>)> {
        if !self.serves {
            return Vec::new();
        }
        let relay = self.relay.clone();
        let handler = native::handler(move |session: native::Session| {
            let relay = relay.clone();
            async move { native::serve_on(&relay, session).await }
        });
        vec![(native::ALPN, Box::new(handler))]
    }

    /// Read every workspace's membership, keys and calls again whenever the key service says one
    /// moved, and at least every [`REREAD`].
    fn follow(self: &Arc<Self>) {
        if !self.serves {
            return;
        }
        let shared = Arc::downgrade(&self.shared);
        let keys = self.shared.workspaces.keys();
        tilo_rt::app().spawn(async move {
            let mut after = keys.generation();
            loop {
                let Some(shared) = shared.upgrade() else {
                    return;
                };
                let held = shared.workspaces.copies().ids().await;
                let observing = Arc::clone(&shared);
                // The reads wait on the workspaces' own locks: off the runtime's workers.
                let observed = tokio::task::spawn_blocking(move || {
                    for workspace in held {
                        observing.observe(workspace);
                    }
                })
                .await;
                drop(shared);
                if observed.is_err() {
                    return;
                }
                tokio::select! {
                    changed = keys.changed(after) => after = changed,
                    () = tokio::time::sleep(REREAD) => {}
                }
            }
        });
    }

    /// Carry one link of the person's desktop session, opened on the daemon's socket, until it
    /// ends: into this relay, as this device, or on to the relay at the address it names.
    pub(crate) async fn serve_link(&self, stream: tilo_socket::tokio::Stream) {
        let (reader, writer) = stream.into_split();
        if let Err(error) = link::carry(
            reader,
            writer,
            &self.relay,
            &self.endpoint,
            self.shared.device,
        )
        .await
        {
            tracing::debug!(%error, "a live link of the desktop session ended unserved");
        }
    }
}

/// The endpoints the relay turned away from each workspace's live session and has not let in
/// since: this device's own resource [`link::TURNED_AWAY`] (`record` empty), read in the person's
/// own workspace by this device's own person alone, as a JSON object of the lists by workspace.
struct TurnedAwayReads(Arc<Shared>);

impl ResourceReads for TurnedAwayReads {
    fn read(
        &self,
        _caller: &Caller,
        address: &ResourceAddress,
    ) -> Result<ResourceCopy, ActionCallError> {
        if address.app != link::TURNED_AWAY || !address.record.is_empty() {
            return Ok(ResourceCopy::Absent);
        }
        // The revision first: a refusal while the lists are read moves it past this one.
        let revision = self.0.turned_revision.load(Ordering::Relaxed).to_string();
        serde_json::to_value(self.0.turned_rows())
            .map(|value| ResourceCopy::reading(value, Some(revision)))
            .map_err(|error| ActionCallError::Unavailable {
                reason: error.to_string(),
            })
    }

    fn answer_read(
        self: Arc<Self>,
        caller: &Caller,
        correlation: u64,
        read: ResourceReadRequest,
    ) -> Pending<RemoteActionMessage> {
        tilo_actions::remote::answer_read(self, caller, correlation, read)
    }

    fn watch(
        &self,
        _caller: &Caller,
        _address: &ResourceAddress,
        wake: &ResourceWake,
    ) -> Result<(), ActionCallError> {
        let mut waiting = self.0.turned_waiting.lock_recover();
        waiting.retain(ResourceWake::waiting);
        waiting.push(wake.clone());
        drop(waiting);
        Ok(())
    }
}

#[cfg(test)]
#[path = "live_relay_tests.rs"]
mod tests;
