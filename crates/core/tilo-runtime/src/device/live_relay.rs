//! The live session of the workspaces this device serves, on the device's one endpoint.
//!
//! A workspace's live session runs through one relay, the device the workspace's state names as
//! serving it. The daemon holds the device's endpoint, so it is the daemon that relays: it serves
//! [`tilo_live::relay::Relay`] on [`tilo_live::native::ALPN`] beside the device's other
//! protocols, and admits a connection for a workspace it hosts only when the certificate the
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
//! The daemon relays one more scope of its own: this device's ([`Scope::Device`]), in its person's
//! own workspace, which their other devices dial directly to drive its keyboard and pointer. It
//! admits by the same grant, bound the same way ([`bind`]); who it admits is the person's devices
//! their record keeps, less the ones they took back ([`members`]), and only this device reads it.
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
    ActionCallError, Caller, ReadsAnswer, ResourceAddress, ResourceCopy, ResourceReads,
    ResourceWake, reads_answer,
};
use tilo_delivery_protocol::workspace_join::WorkspaceDeviceHintV1;
use tilo_desktop_model::WorkspaceId;
use tilo_live::grant;
use tilo_live::link;
use tilo_live::live::{self, Namespace};
use tilo_live::native;
use tilo_live::relay::{
    Admission, Denied, Direction, Peer, Relay, RelayAuthority, SessionAuthorization,
};
use tilo_live::roster::Roster;
use tilo_lock::MutexRecover as _;
use tilo_session_client::workspace_keys::{Device, admitted_devices};
use tilo_user_identity::{DeviceCert, EndpointDelegation};
use tilo_workspace_service::workspaces::person_workspace::devices::{DEVICES, Devices};
use tilo_workspace_service::workspaces::{Held, PersonResources};

use super::workspaces::ServiceWorkspaces;

/// How often the workspaces' membership, keys and calls are read again when nothing announced a
/// change: a removal the key service did not announce still cuts its sessions within this.
const REREAD: Duration = Duration::from_secs(30);

/// How long a workspace whose relay did not take this device in waits before it is asked again:
/// [`RETRY_FIRST`] after the first miss, twice as long after each further one, and never longer
/// than [`RETRY_LONGEST`]. A relay that is away and comes back says nothing, so it is asked again
/// soon and then ever less often, and never given up on.
const RETRY_FIRST: Duration = Duration::from_secs(1);
const RETRY_LONGEST: Duration = Duration::from_secs(15);

/// The wait the schedule gives before a workspace's relay is asked again after `missed` attempts
/// in a row it took this device in on none of.
fn retry_schedule(missed: u32) -> Duration {
    let doublings = missed.saturating_sub(1).min(8);
    RETRY_FIRST
        .saturating_mul(1_u32 << doublings)
        .min(RETRY_LONGEST)
}

/// The wait before `device` asks `workspace`'s relay again after `missed` misses in a row: the
/// schedule's ([`retry_schedule`]), less up to a quarter the device takes off by its own key
/// ([`tilo_live::live_session::spread`]). The devices of a workspace miss its relay together, when
/// it went away or had no room; on the schedule alone they would all ask it again in one moment
/// each time.
fn retry_wait(device: [u8; 32], workspace: WorkspaceId, missed: u32) -> Duration {
    use std::hash::{Hash as _, Hasher as _};
    let mut turn = std::hash::DefaultHasher::new();
    workspace.as_uuid().as_bytes().hash(&mut turn);
    missed.hash(&mut turn);
    tilo_live::live_session::spread(retry_schedule(missed), device, turn.finish())
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
/// scope's roster.
struct Request {
    workspace: WorkspaceId,
    scope: Scope,
    device: [u8; 32],
}

/// Bind a grant for `scope` to the transport and the scope's roster ([`members`]):
///
/// * the certificate is byte for byte the row the roster holds for its device — the membership
///   every device already syncs, so a device presenting another device's certificate, or one the
///   workspace removed or its person took back, is refused;
/// * the connection's endpoint speaks for that device: it is the device's own key, or one the
///   device signed an [`EndpointDelegation`] for, live now, which is how a page whose device key
///   cannot leave `WebCrypto` dials;
/// * the certificate has not expired, and the workspace is one this device relays now
///   (`relays`): one it serves, while it is a member, or this device's own.
fn bind(
    peer: Peer,
    grant: &grant::Native,
    scope: &Scope,
    devices: &[Device],
    relays: bool,
    now_ns: u64,
) -> Result<Request, Refusal> {
    let Peer(endpoint) = peer;
    let workspace =
        WorkspaceId::parse(&live::workspace_id(grant.workspace)).ok_or(Refusal::Unparsed)?;
    if grant.scope != scope.id() || scope.workspace_id() != workspace.to_string() {
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
    Ok(Request {
        workspace,
        scope: scope.clone(),
        device,
    })
}

/// Who `scope` admits as this device holds it now, and whether this device relays it; `None` for
/// a scope of a workspace it does not hold.
///
/// * A workspace's scopes admit the workspace's members, and are relayed here while this device
///   serves the workspace ([`relays_here`]).
/// * This device's own scope admits its person's devices as the record of them keeps them, less
///   the ones they took back and the ones whose certificate ran out, and only in their own
///   workspace. `person` is that record, `own` this device's certificate.
///
/// The keys a workspace's live session seals under here, or why it holds none.
enum SessionKeys {
    Ready(tilo_live::frame::LiveKeys),
    Waiting,
    Keyless,
}

/// What this device's key service says of `workspace`'s media channel now.
fn session_keys(workspaces: &ServiceWorkspaces, workspace: WorkspaceId) -> SessionKeys {
    match workspaces.keys().get().map(|keys| keys.access(workspace).0) {
        Some(tilo_workspace_keys::KeyAccess::Ready(keys)) => {
            SessionKeys::Ready(tilo_live::frame::LiveKeys::from(keys))
        }
        Some(
            tilo_workspace_keys::KeyAccess::Waiting | tilo_workspace_keys::KeyAccess::Unavailable,
        ) => SessionKeys::Waiting,
        _ => SessionKeys::Keyless,
    }
}

/// The epoch `workspace`'s media channel is at here, for a refusal that names it.
fn keys_epoch_held(workspaces: &ServiceWorkspaces, workspace: WorkspaceId) -> u64 {
    match workspaces.keys().get().map(|keys| keys.access(workspace).0) {
        Some(tilo_workspace_keys::KeyAccess::Ready(keys)) => {
            tilo_live::frame::LiveKeys::from(keys).epoch()
        }
        _ => 0,
    }
}

fn members(
    workspaces: &ServiceWorkspaces,
    person: Option<&PersonResources>,
    own: &DeviceCert,
    scope: &Scope,
    now_ns: u64,
) -> Option<(bool, Result<Vec<Device>, String>)> {
    let workspace = WorkspaceId::parse(scope.workspace_id())?;
    let Some(device) = scope.device_of() else {
        let (held, devices) = read(workspaces, workspace)?;
        let relays = devices
            .as_ref()
            .is_ok_and(|devices| relays_here(Some(held), devices, own.device_public_key, now_ns));
        return Some((relays, devices));
    };
    let root = own.user_public_key;
    if workspace != person_workspace(&root) {
        return None;
    }
    let Some(record) = person.and_then(|person| person.read::<Devices>(DEVICES)) else {
        return Some((
            false,
            Err("the record of the person's devices is not here yet".to_owned()),
        ));
    };
    let devices: Vec<Device> = record
        .listed(&root)
        .into_iter()
        .filter(|listed| !listed.device.revoked(&listed.cert) && !listed.cert.is_expired_at(now_ns))
        .map(|listed| Device {
            device: listed.cert.device_public_key,
            cert: listed.cert,
        })
        .collect();
    let relays =
        device == own.device_public_key && devices.iter().any(|listed| listed.device == device);
    Some((relays, Ok(devices)))
}

/// `person`'s own workspace.
fn person_workspace(person: &[u8; 32]) -> WorkspaceId {
    WorkspaceId::from(collab_types::workspace_id::person_workspace(person))
}

/// Whether `own` relays a workspace that `held` says how this device holds, whose members are
/// `devices`: it serves the workspace, and is a member whose certificate is in force.
fn relays_here(held: Option<Held>, devices: &[Device], own: [u8; 32], now_ns: u64) -> bool {
    matches!(held, Some(Held::Hosts))
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
    /// The record of this device's person's devices: who this device's own scope admits.
    record: Option<PersonResources>,
    /// This device's certificate.
    own: DeviceCert,
    /// The scope the session was admitted to.
    scoped: Scope,
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
        // The scope's membership as it is now: a device the workspace removed or its person took
        // back, or one whose certificate ran out, is not admitted any more.
        let Some((_, Ok(devices))) = members(
            &self.workspaces,
            self.record.as_ref(),
            &self.own,
            &self.scoped,
            now_ns(),
        ) else {
            return false;
        };
        if !devices.iter().any(|device| device.device == name.device())
            || !devices.iter().any(|device| device.device == self.device)
        {
            return false;
        }
        match direction {
            Direction::Publish => name.device() == self.device,
            // What is sent to one device is read by that device alone.
            Direction::Subscribe => self
                .scoped
                .device_of()
                .is_none_or(|device| device == self.device),
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

/// How far this device's place in one workspace's live session came, as its daemon publishes it
/// ([`link::SESSIONS`]): what the desktop session shows beside the workspace, and what decides
/// whether a call, a shared window or presence carries there.
#[derive(Clone, Debug, PartialEq, Eq)]
enum SessionState {
    /// In the live session, through `relay`, sealing under `epoch`.
    Open {
        relay: [u8; 32],
        name: String,
        epoch: u64,
    },
    /// The workspace's media keys are not here yet.
    WaitingForKeys,
    /// This device holds no keys of the workspace and none are coming.
    Keyless,
    /// Who is in the workspace cannot be read here.
    Unread,
    /// The workspace names no device that relays it.
    NoRelay,
    /// The relay was not reached.
    Unreached { relay: [u8; 32], name: String },
    /// The relay was admitting as many devices as it takes at once.
    Busy { relay: [u8; 32], name: String },
    /// The relay answered and did not admit this device, which presented `epoch`'s keys.
    Refused {
        relay: [u8; 32],
        name: String,
        epoch: u64,
    },
}

impl SessionState {
    /// This state as the reading says it, with `own` this device.
    fn row(&self, own: [u8; 32]) -> serde_json::Value {
        let word = match self {
            Self::Open { .. } => "open",
            Self::WaitingForKeys => "waiting-for-keys",
            Self::Keyless => "no-keys",
            Self::Unread => "membership-unread",
            Self::NoRelay => "no-relay",
            Self::Unreached { .. } => "relay-unreached",
            Self::Busy { .. } => "relay-busy",
            Self::Refused { .. } => "refused",
        };
        let (relay, name, epoch) = match self {
            Self::Open { relay, name, epoch } | Self::Refused { relay, name, epoch } => {
                (Some(*relay), name.clone(), Some(*epoch))
            }
            Self::Unreached { relay, name } | Self::Busy { relay, name } => {
                (Some(*relay), name.clone(), None)
            }
            _ => (None, String::new(), None),
        };
        serde_json::json!({
            "state": word,
            "relay": relay.map(hex::encode).unwrap_or_default(),
            "relay_name": name,
            "relay_here": relay.is_some_and(|relay| relay == own),
            "epoch": epoch,
            "lent": false,
        })
    }
}

/// What the relay's judgement shares with the sessions it admitted: the device's workspaces, the
/// epochs, rosters and calls it last read, and the endpoints it turned away.
struct Shared {
    workspaces: Arc<ServiceWorkspaces>,
    device: [u8; 32],
    /// This device's certificate: whose devices its own scope admits.
    cert: DeviceCert,
    /// The record of this device's person's devices, once the daemon holds it
    /// ([`Live::follow_person`]).
    person: std::sync::OnceLock<PersonResources>,
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
    /// How far this device's place in each workspace's live session came, as the reading says it.
    sessions: Mutex<BTreeMap<WorkspaceId, SessionState>>,
    /// Moves with every change to [`Self::sessions`].
    sessions_revision: AtomicU64,
    /// The reads of [`Self::sessions`] waiting for it to move.
    sessions_waiting: Mutex<Vec<ResourceWake>>,
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
        if rosters.is_empty() || rosters.values().any(|devices| devices.contains(&endpoint)) {
            return false;
        }
        drop(rosters);
        // One of the person's own devices, driving this one, is no stranger either.
        let own = Scope::device(
            person_workspace(&self.cert.user_public_key).to_string(),
            self.device,
        );
        !members(
            &self.workspaces,
            self.person.get(),
            &self.cert,
            &own,
            now_ns(),
        )
        .and_then(|(_, devices)| devices.ok())
        .is_some_and(|devices| devices.iter().any(|device| device.device == endpoint))
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
        let Some(scope) = Scope::parse(workspace.to_string(), &grant.scope) else {
            self.turned_away(workspace, endpoint, String::new(), Refusal::Scope);
            return Err(Denied);
        };
        let now = now_ns();
        let read = members(&self.workspaces, self.person.get(), &self.cert, &scope, now);
        let relays = read.as_ref().is_some_and(|(relays, _)| *relays);
        let devices = read.map(|(_, devices)| devices);
        let judged = match &devices {
            Some(Ok(devices)) => self.judge(peer, &grant, &scope, relays, devices, now),
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

    /// [`Self::decide`] for a grant that names `scope`, which this device relays when `relays`
    /// and whose devices are `devices`: the session it is admitted as, or why it is turned away.
    fn judge(
        &self,
        peer: Peer,
        grant: &grant::Native,
        scope: &Scope,
        relays: bool,
        devices: &[Device],
        now: u64,
    ) -> Result<Arc<dyn SessionAuthorization>, Refusal> {
        let request = bind(peer, grant, scope, devices, relays, now)?;
        let newest = self
            .epochs
            .lock_recover()
            .get(&request.workspace)
            .copied()
            .unwrap_or(0);
        keys_current(grant.epoch, newest)?;
        let scope = request.scope;
        let person = devices
            .iter()
            .find(|admitted| admitted.device == request.device)
            .map(|admitted| hex::encode(admitted.cert.user_public_key))
            .unwrap_or_default();
        Ok(Arc::new(Permit {
            workspaces: Arc::clone(&self.workspaces),
            record: self.person.get().cloned(),
            own: self.cert.clone(),
            scoped: scope.clone(),
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
        if !matches!(held, Held::Hosts) {
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
    /// This device's own place in each workspace's live session, as this daemon holds it: what
    /// an app's call lane joins through, and what the person's desktop drives over its link.
    calls: Mutex<BTreeMap<WorkspaceId, Arc<super::live_call::CallSession>>>,
    /// What the device's apps and the person's shell say in each workspace, kept to say again in
    /// a live session that opens later, and the apps that hear the others' words.
    pub(super) speaking: super::live_speaking::Speaking,
    /// This live session itself, for what answers on its behalf later.
    this: std::sync::Weak<Live>,
    /// Told when a workspace's live session ended, so it is settled again at once.
    settle: Arc<tokio::sync::Notify>,
}

impl Live {
    /// The live session of `workspaces`' device, on `relay` when its keeping already runs one (a
    /// server's), and on a relay of its own otherwise. Must be called inside the daemon's runtime.
    pub(crate) fn new(
        workspaces: &Arc<ServiceWorkspaces>,
        relay: Option<Relay>,
        fanout: Arc<tilo_app_host::collab_broker::FanoutState>,
    ) -> Arc<Self> {
        let (changed, _) = tokio::sync::watch::channel(0);
        let shared = Arc::new(Shared {
            workspaces: Arc::clone(workspaces),
            device: workspaces.device_cert().device_public_key,
            cert: workspaces.device_cert(),
            person: std::sync::OnceLock::new(),
            epochs: Mutex::default(),
            rosters: Mutex::default(),
            calls: Arc::default(),
            changed,
            turned: Mutex::default(),
            turned_revision: AtomicU64::new(0),
            turned_waiting: Mutex::default(),
            sessions: Mutex::default(),
            sessions_revision: AtomicU64::new(0),
            sessions_waiting: Mutex::default(),
        });
        // Whom the relay turned away, and why: what the person's desktop session shows. It is
        // this computer's relay alone that turned them away.
        workspaces.copies().publish_own(
            link::TURNED_AWAY,
            tilo_workspace_service::workspaces::OwnReaders::ThisDevice,
            Arc::new(TurnedAwayReads(Arc::clone(&shared))),
        );
        // How far this device's place in each workspace's live session came: what the desktop
        // shows beside the workspace, and what its link tells it to carry.
        workspaces.copies().publish_own(
            link::SESSIONS,
            tilo_workspace_service::workspaces::OwnReaders::ThisDevice,
            Arc::new(SessionReads(Arc::clone(&shared))),
        );
        let serves = relay.is_none();
        let relay =
            relay.unwrap_or_else(|| Relay::new(Arc::clone(&shared) as Arc<dyn RelayAuthority>));
        let live = Arc::new_cyclic(|this| Self {
            shared,
            relay,
            endpoint: workspaces.endpoint(),
            serves,
            calls: Mutex::default(),
            speaking: super::live_speaking::Speaking::new(fanout),
            this: this.clone(),
            settle: Arc::new(tokio::sync::Notify::new()),
        });
        live.speaking.hear_apps(&live);
        // An app joins and leaves a call through the device's one dispatch, as any action.
        workspaces.copies().providers().provide(
            super::live_speaking::PROVIDER,
            super::live_speaking::PROVIDER,
            tilo_workspace_service::workspaces::providers::Reach::ThisDevice,
            Arc::new(super::live_speaking::LiveCalls(Arc::downgrade(&live))),
        );
        live.follow();
        live
    }

    /// This live session, while it runs.
    pub(super) fn weak(&self) -> std::sync::Weak<Self> {
        self.this.clone()
    }

    /// The person this device belongs to, by their root key.
    pub(super) fn person(&self) -> [u8; 32] {
        self.shared.workspaces.device_cert().user_public_key
    }

    /// The workspaces this device holds.
    pub(super) fn workspaces(&self) -> &Arc<ServiceWorkspaces> {
        &self.shared.workspaces
    }

    /// Admit this device's person's devices to its own scope by `person`, the record of them, from
    /// now on: a device they took back is cut as the record says so. Only the first call counts.
    pub(crate) fn follow_person(&self, person: PersonResources) {
        let mut moved = person.changes();
        if self.shared.person.set(person).is_err() {
            return;
        }
        let shared = Arc::downgrade(&self.shared);
        tilo_rt::app().spawn(async move {
            while moved.changed().await.is_ok() {
                let Some(shared) = shared.upgrade() else {
                    return;
                };
                shared.bump();
            }
        });
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
    /// moved, a live session of one ended, and at least every [`REREAD`]; the relay's judgement
    /// follows them, and so does this device's own place in each workspace's live session. A
    /// workspace whose relay did not take this device in is asked again after [`retry_wait`].
    fn follow(self: &Arc<Self>) {
        let live = Arc::downgrade(self);
        let keys = self.shared.workspaces.keys();
        let settle = Arc::clone(&self.settle);
        tilo_rt::app().spawn(async move {
            let mut after = keys.generation();
            let mut missed: BTreeMap<WorkspaceId, u32> = BTreeMap::new();
            loop {
                let Some(live) = live.upgrade() else {
                    return;
                };
                let held = live.shared.workspaces.copies().ids().await;
                live.speaking.follow_workspaces(&held);
                if live.serves {
                    let observing = Arc::clone(&live.shared);
                    let workspaces = held.clone();
                    // The reads wait on the workspaces' own locks: off the runtime's workers.
                    let observed = tokio::task::spawn_blocking(move || {
                        for workspace in workspaces {
                            observing.observe(workspace);
                        }
                    })
                    .await;
                    if observed.is_err() {
                        return;
                    }
                }
                // This device's own place in each workspace's live session: a server's daemon
                // holds it as a desktop's session does, so a call carries with no session.
                let mut wait = REREAD;
                for workspace in &held {
                    live.reconcile_call(*workspace).await;
                    if live.shared.awaits_relay(*workspace) {
                        let in_a_row = missed.entry(*workspace).or_default();
                        *in_a_row = in_a_row.saturating_add(1);
                        wait = wait.min(retry_wait(live.shared.device, *workspace, *in_a_row));
                    } else {
                        missed.remove(workspace);
                    }
                }
                missed.retain(|workspace, _| held.contains(workspace));
                drop(live);
                tokio::select! {
                    changed = keys.changed(after) => after = changed,
                    () = settle.notified() => {}
                    () = tokio::time::sleep(wait) => {}
                }
            }
        });
    }

    /// This device's place in `workspace`'s live session as its state is now: joined while this
    /// device serves the workspace and holds its keys, kept and moved with it, and closed when
    /// either is no longer so. A workspace another device serves is left to the person's desktop
    /// until this daemon is the one that carries its devices.
    async fn reconcile_call(self: &Arc<Self>, workspace: WorkspaceId) {
        let own = self.shared.device;
        let Some((held, devices)) = read(&self.shared.workspaces, workspace) else {
            self.drop_call(workspace);
            self.shared.note_session(workspace, SessionState::Unread);
            return;
        };
        let Ok(devices) = devices else {
            self.drop_call(workspace);
            self.shared.note_session(workspace, SessionState::Unread);
            return;
        };
        let keys = session_keys(&self.shared.workspaces, workspace);
        // Where this device takes part: its own relay when it serves the workspace, the relay of
        // the device that holds it otherwise, whose signed address its governance names.
        let toward = match held {
            Held::Hosts => Some((own, None)),
            Held::MemberCopy => self
                .host_relay(workspace)
                .await
                .map(|(device, addr)| (device, Some(addr))),
        };
        let Some((serving, away)) = toward else {
            self.drop_call(workspace);
            self.shared.note_session(workspace, SessionState::NoRelay);
            return;
        };
        let relay_name = name_of(&devices, serving);
        let kept = {
            let calls = self.calls.lock_recover();
            calls
                .get(&workspace)
                .filter(|call| !call.ended() && call.relay() == serving)
                .cloned()
        };
        if let Some(call) = kept {
            self.refresh_call(workspace, &call, keys, &devices, serving, relay_name)
                .await;
            return;
        }
        self.drop_call(workspace);
        self.open_call(workspace, held, &devices, keys, serving, away, relay_name)
            .await;
    }

    /// The call this device already holds for `workspace`, taken up as its keys, the workspace's
    /// devices and its relay are now.
    async fn refresh_call(
        &self,
        workspace: WorkspaceId,
        call: &Arc<super::live_call::CallSession>,
        keys: SessionKeys,
        devices: &[Device],
        serving: [u8; 32],
        relay_name: String,
    ) {
        match keys {
            SessionKeys::Ready(keys) => {
                let gate = self.call_gate(workspace);
                call.refresh(keys, devices, gate);
                call.hand_call_keys(self.shared.workspaces.copies()).await;
                self.shared.note_session(
                    workspace,
                    SessionState::Open {
                        relay: serving,
                        name: relay_name,
                        epoch: call.epoch(),
                    },
                );
            }
            SessionKeys::Waiting => self
                .shared
                .note_session(workspace, SessionState::WaitingForKeys),
            SessionKeys::Keyless => self.shared.note_session(workspace, SessionState::Keyless),
        }
    }

    /// This device's call for `workspace`, joined where it holds none: its own relay serves it, or
    /// the device that holds the workspace does.
    async fn open_call(
        self: &Arc<Self>,
        workspace: WorkspaceId,
        held: Held,
        devices: &[Device],
        keys: SessionKeys,
        serving: [u8; 32],
        away: Option<iroh::EndpointAddr>,
        relay_name: String,
    ) {
        let SessionKeys::Ready(keys) = keys else {
            self.shared.note_session(
                workspace,
                match keys {
                    SessionKeys::Keyless => SessionState::Keyless,
                    _ => SessionState::WaitingForKeys,
                },
            );
            return;
        };
        let own = self.shared.device;
        let gate = self.call_gate(workspace);
        let roster = Roster::new(devices.iter().map(|device| device.device));
        let local = matches!(held, Held::Hosts).then_some(&self.relay);
        let certificate = self.shared.workspaces.device_cert().clone();
        match super::live_call::CallSession::join(
            local,
            away,
            super::live_call::Joining {
                endpoint: &self.endpoint,
                certificate: &certificate,
                relay: serving,
                device: own,
                workspace,
                keys,
                roster,
                base: self.base_word(devices),
            },
        )
        .await
        {
            Ok(call) => {
                let call = Arc::new(call);
                let (rules, persons) = gate;
                call.session().set_calls(rules, persons);
                {
                    let live = Arc::downgrade(self);
                    let relay = serving;
                    let name = relay_name.clone();
                    call.session().set_ended(Arc::new(move || {
                        if let Some(live) = live.upgrade() {
                            live.call_ended(workspace, relay, name.clone());
                        }
                    }));
                }
                self.speaking.speak_in(workspace, call.speaker());
                self.calls
                    .lock_recover()
                    .insert(workspace, Arc::clone(&call));
                call.hand_call_keys(self.shared.workspaces.copies()).await;
                self.shared.note_session(
                    workspace,
                    SessionState::Open {
                        relay: serving,
                        name: relay_name,
                        epoch: call.epoch(),
                    },
                );
                tracing::debug!(%workspace, relay = %hex::encode(serving),
                    "the device's live session is held by its daemon");
            }
            Err(error) => {
                let state = match error {
                    tilo_live::live_session::LiveError::Denied => SessionState::Refused {
                        relay: serving,
                        name: relay_name,
                        epoch: keys_epoch_held(&self.shared.workspaces, workspace),
                    },
                    tilo_live::live_session::LiveError::Busy => SessionState::Busy {
                        relay: serving,
                        name: relay_name,
                    },
                    _ => SessionState::Unreached {
                        relay: serving,
                        name: relay_name,
                    },
                };
                self.shared.note_session(workspace, state);
                tracing::debug!(%workspace, relay = %hex::encode(serving), %error,
                    "this device took no part in its workspace's live session");
            }
        }
    }

    /// What this device says of itself when one of its apps takes its person into a call with no
    /// shell of theirs running: its name, as its certificate gives it, and its person. Outside a
    /// call such a device says nothing.
    fn base_word(&self, devices: &[Device]) -> tilo_live::presence::Own {
        let cert = self.shared.workspaces.device_cert();
        let name = tilo_user_identity::device_name(&cert.device_label)
            .map_or_else(|| name_of(devices, self.shared.device), str::to_owned);
        tilo_live::presence::Own {
            label: name.clone(),
            device: name,
            user: Some(hex::encode(cert.user_public_key)),
            ..tilo_live::presence::Own::default()
        }
    }

    /// What the workspace's calls allow now, and the person each of its member devices is.
    pub(super) fn call_gate(&self, workspace: WorkspaceId) -> super::live_call::CallGate {
        self.shared
            .workspaces
            .copies()
            .read_now(&workspace.to_string(), |peer| {
                peer.governance().call_gate(tilo_clock::wall())
            })
            .and_then(|(_, gate)| gate.ok())
            .unwrap_or_default()
    }

    /// Where the device holding `workspace` is reached, from the signed address its governance
    /// names, and which device it is.
    async fn host_relay(&self, workspace: WorkspaceId) -> Option<([u8; 32], iroh::EndpointAddr)> {
        let peer = self
            .shared
            .workspaces
            .copies()
            .state(&workspace.to_string())
            .await?;
        let hints = peer.governance().keeping_devices();
        hints.into_iter().find_map(|hint| {
            let host = WorkspaceDeviceHintV1::from_transport_hint(&hint).ok()?;
            let device = host.device_certificate.device_public_key;
            Some((device, host.endpoint_addr))
        })
    }

    /// `workspace`'s live session ended: what this device shows waits for the relay again, and
    /// the workspace is settled afresh.
    fn call_ended(&self, workspace: WorkspaceId, relay: [u8; 32], name: String) {
        self.drop_call(workspace);
        self.shared
            .note_session(workspace, SessionState::Unreached { relay, name });
        self.settle.notify_one();
    }

    /// Stop taking part in `workspace`'s live session here: the session's readers and carrier
    /// end with the last handle to it.
    fn drop_call(&self, workspace: WorkspaceId) {
        self.calls.lock_recover().remove(&workspace);
    }

    /// This device's place in `workspace`'s live session, while its daemon holds one: what an
    /// app's call lane joins through.
    pub(crate) fn call_session(
        &self,
        workspace: WorkspaceId,
    ) -> Option<Arc<super::live_call::CallSession>> {
        self.calls.lock_recover().get(&workspace).cloned()
    }

    /// Carry one link of the person's desktop session, opened on the daemon's socket, until it
    /// ends: into this relay, as this device, or on to the relay at the address it names.
    pub(crate) async fn serve_link(&self, stream: tilo_socket::tokio::Stream) {
        let (reader, writer) = tokio::io::split(stream);
        if let Err(error) = link::carry(
            reader,
            writer,
            &self.relay,
            &self.endpoint,
            self.shared.device,
            Some(self),
        )
        .await
        {
            tracing::debug!(%error, "a live link of the desktop session ended unserved");
        }
    }
}

impl link::Calls for Live {
    fn carry<'a>(
        &'a self,
        workspace: String,
        session: link::Session,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'a>> {
        let call =
            WorkspaceId::parse(&workspace).and_then(|workspace| self.call_session(workspace));
        Box::pin(async move {
            let Some(call) = call else {
                session.close(0, "this device holds no live session for that workspace");
                return;
            };
            if tilo_live::link_call::serve(session, call.shared())
                .await
                .is_err()
            {
                tracing::debug!(%workspace,
                    "a desktop's link into the workspace's live session ended unserved");
            }
        })
    }
}

/// The endpoints the relay turned away from each workspace's live session and has not let in
/// since: this device's own resource [`link::TURNED_AWAY`] (`record` empty), read in the person's
/// own workspace by this device's own person alone, as a JSON object of the lists by workspace.
struct TurnedAwayReads(Arc<Shared>);

impl ResourceReads for TurnedAwayReads {
    fn read<'a>(
        &'a self,
        _caller: &'a Caller,
        address: &'a ResourceAddress,
    ) -> ReadsAnswer<'a, ResourceCopy> {
        reads_answer(async move {
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
        })
    }

    fn watch<'a>(
        &'a self,
        _caller: &'a Caller,
        _address: &'a ResourceAddress,
        wake: &'a ResourceWake,
    ) -> ReadsAnswer<'a, ()> {
        reads_answer(async move {
            let mut waiting = self.0.turned_waiting.lock_recover();
            waiting.retain(ResourceWake::waiting);
            waiting.push(wake.clone());
            drop(waiting);
            Ok(())
        })
    }
}

/// How far this device's place in each workspace's live session came: this device's own resource
/// [`link::SESSIONS`] (`record` empty), read in the person's own workspace by this device's own
/// person alone, as a JSON object by workspace.
struct SessionReads(Arc<Shared>);

impl SessionReads {
    fn rows(shared: &Shared) -> BTreeMap<String, serde_json::Value> {
        shared
            .sessions
            .lock_recover()
            .iter()
            .map(|(workspace, state)| (workspace.to_string(), state.row(shared.device)))
            .collect()
    }

    fn moved(shared: &Shared) {
        shared.sessions_revision.fetch_add(1, Ordering::Relaxed);
        let waiting = std::mem::take(&mut *shared.sessions_waiting.lock_recover());
        for wake in waiting {
            wake.wake();
        }
    }
}

impl ResourceReads for SessionReads {
    fn read<'a>(
        &'a self,
        _caller: &'a Caller,
        address: &'a ResourceAddress,
    ) -> ReadsAnswer<'a, ResourceCopy> {
        reads_answer(async move {
            if address.app != link::SESSIONS || !address.record.is_empty() {
                return Ok(ResourceCopy::Absent);
            }
            let revision = self.0.sessions_revision.load(Ordering::Relaxed).to_string();
            serde_json::to_value(Self::rows(&self.0))
                .map(|value| ResourceCopy::reading(value, Some(revision)))
                .map_err(|error| ActionCallError::Unavailable {
                    reason: error.to_string(),
                })
        })
    }

    fn watch<'a>(
        &'a self,
        _caller: &'a Caller,
        _address: &'a ResourceAddress,
        wake: &'a ResourceWake,
    ) -> ReadsAnswer<'a, ()> {
        reads_answer(async move {
            let mut waiting = self.0.sessions_waiting.lock_recover();
            waiting.retain(ResourceWake::waiting);
            waiting.push(wake.clone());
            drop(waiting);
            Ok(())
        })
    }
}

impl Shared {
    /// Note how far this device's place in `workspace`'s live session came, and tell the reading
    /// when it moved.
    /// Whether `workspace`'s relay is to be asked again: it was not reached, or had no room.
    fn awaits_relay(&self, workspace: WorkspaceId) -> bool {
        matches!(
            self.sessions.lock_recover().get(&workspace),
            Some(SessionState::Unreached { .. } | SessionState::Busy { .. })
        )
    }

    fn note_session(&self, workspace: WorkspaceId, state: SessionState) {
        let moved = {
            let mut sessions = self.sessions.lock_recover();
            sessions.get(&workspace) != Some(&state) && {
                sessions.insert(workspace, state);
                true
            }
        };
        if moved {
            SessionReads::moved(self);
        }
    }
}

#[cfg(test)]
#[path = "live_relay_tests.rs"]
mod tests;
