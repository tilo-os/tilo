//! The live plane between the devices of a workspace: one sealed `moq-lite` session per workspace,
//! carried over this session's own iroh endpoint, and the relay role one member of the workspace
//! serves for the others.
//! Which members relay is durable workspace state, not presence and not the plane itself, so every
//! device names the same ones without asking anyone ([`relays_of`]): every member whose certificate
//! says it may act as a host ([`DeviceCapabilities::can_act_as_host`]), by ascending device key; and
//! in a workspace with none of those, the device hosting it, which every member already reaches:
//! two people's own devices sharing a workspace one of them hosts have a live plane without a
//! server. Every session serves [`tilo_live::relay::Relay`] on its endpoint, and its ALPN handler
//! admits a connection only for a workspace the session relays now, as the workspace's state names
//! it: a device only when the certificate it presents is the roster's own and the handshake proved
//! the key. It cuts a session when the workspace's membership or keys move.
//! A device joins the first of those relays it reaches, so an elected relay that is offline costs a
//! member the one above it and not the workspace's live plane. While none of them takes it in — the
//! relay is away, or does not know this device yet — it asks them again, waiting longer each time,
//! since a relay that comes back says nothing to the devices that missed it. Whichever relay a
//! member reaches,
//! every relay bridges the other relays into its own: members split across two relays of one
//! workspace still hear each other.
//! Every other member dials that relay over iroh ([`tilo_live::native`]) with its device
//! certificate as its grant, and each participant seals its own tracks under the scope's channel
//! content keys ([`tilo_live::plane::Plane`]): the relay copies bytes it cannot open, and it is a
//! member that holds them already.
//!
//! [`DeviceCapabilities::can_act_as_host`]: tilo_user_identity::DeviceCapabilities::can_act_as_host

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use collab_types::scope::Scope;
use iroh::{Endpoint, EndpointAddr, EndpointId};
use tilo_desktop_model::WorkspaceId;
use tilo_live::frame::PlaneKeys;
use tilo_live::grant;
use tilo_live::live::{self, Namespace};
use tilo_live::native;
use tilo_live::plane::{Heard, Join, Plane, PlaneError};
use tilo_live::relay::{
    Admission, Denied, Direction, Peer, Relay, RelayAuthority, SessionAuthorization,
};
use tilo_live::roster::Roster;
use tilo_lock::MutexRecover as _;
use tilo_network_actor::local_host::Protocols;
use tilo_user_identity::{DeviceCert, EndpointDelegation};

use crate::channels::{self, Member};
use crate::engine::Engine;

/// The scope one workspace's live plane carries.
fn scope_of(workspace: WorkspaceId) -> Scope {
    Scope::workspace(workspace.to_string())
}

/// The workspace's relays, as its state names them: every member whose certificate says it takes
/// the work, by ascending device key, so every device tries them in the same order without another
/// round trip; and when none does, `host`, the device hosting the workspace, while it is a member.
/// Empty when neither is, which is a workspace with no live plane.
pub(crate) fn relays_of(members: &[Member], host: Option<[u8; 32]>, now_ns: u64) -> Vec<[u8; 32]> {
    let current = |member: &&Member| !member.cert.is_expired_at(now_ns);
    let mut relays: Vec<[u8; 32]> = members
        .iter()
        .filter(current)
        .filter(|member| member.cert.capabilities.can_act_as_host)
        .map(|member| member.device)
        .collect();
    if relays.is_empty() {
        relays.extend(
            members
                .iter()
                .filter(current)
                .map(|member| member.device)
                .filter(|device| Some(*device) == host),
        );
    }
    relays.sort_unstable();
    relays.dedup();
    relays
}

/// Join budget of a bridge: a relay this device cannot reach is not one to wait on, since the plane
/// itself does not go through it.
const BRIDGE_JOINT: Duration = Duration::from_secs(3);
/// How long one relay of a workspace has to carry the plane before the next one is tried.
const ATTEMPT: Duration = Duration::from_secs(4);
/// How long a workspace whose relays did not take this device in waits before it asks them again:
/// [`RETRY_FIRST`] after the first miss, twice as long after each further one, and never longer
/// than [`RETRY_LONGEST`]. A relay that is away costs one dial of [`ATTEMPT`] each time, so a
/// device whose workspace's host is off for a week asks four times a minute and no more.
const RETRY_FIRST: Duration = Duration::from_secs(1);
const RETRY_LONGEST: Duration = Duration::from_secs(15);

/// The wait before a workspace's relays are asked again after `missed` attempts in a row that none
/// of them answered with a plane.
fn retry_wait(missed: u32) -> Duration {
    let doublings = missed.saturating_sub(1).min(8);
    RETRY_FIRST
        .saturating_mul(1_u32 << doublings)
        .min(RETRY_LONGEST)
}

/// Now, in Unix nanoseconds.
fn now_ns() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| {
            u64::try_from(elapsed.as_nanos()).unwrap_or(u64::MAX)
        })
}

/// A device that relays a workspace's live plane, as a standing names it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Relayer {
    /// Its device key.
    pub device: [u8; 32],
    /// What its person called it, as the certificate the workspace holds of it says; empty for a
    /// device they never named.
    pub name: String,
    /// Whether it is this device.
    pub here: bool,
}

/// How far one workspace's live plane came on this device. A plane that is not open carries no
/// presence, no call and no picture of a shared window, and nothing else says so: what was asked
/// of it waits without a word. The session publishes this, so a person, a tool and a test read why.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Standing {
    /// This device is in the plane, through `relay`, sealing under the keys of `epoch`. `lent`
    /// says those are the keys of its own streams alone, lent by the workspace's host
    /// ([`MediaKeysLender`]): it publishes with them and opens nothing another device sends.
    Open {
        relay: Relayer,
        epoch: u64,
        lent: bool,
    },
    /// This device asked for the keys of the workspace's media channel, or asked the workspace's
    /// host to lend it the keys of its own streams, and does not hold them yet.
    WaitingForKeys,
    /// This device holds no keys of the workspace and none are coming: it keeps no secret store,
    /// or the workspace removed it from the channel.
    Keyless,
    /// Who is in the workspace cannot be read here, so nothing names who relays it.
    Unread,
    /// No member of the workspace relays its live plane.
    NoRelay,
    /// None of the workspace's relays was reached: `relay` is the first of them.
    Unreached { relay: Relayer },
    /// `relay` answered and did not admit this device.
    Refused { relay: Relayer },
}

impl Standing {
    /// Whether the plane carries what this device says and asks for now.
    #[must_use]
    pub const fn is_open(&self) -> bool {
        matches!(self, Self::Open { .. })
    }
}

/// `device` as a standing names it, by the certificate `members` holds of it.
fn relayer(members: &[Member], device: [u8; 32], own: [u8; 32]) -> Relayer {
    let name = members
        .iter()
        .find(|member| member.device == device)
        .and_then(|member| tilo_user_identity::device_name(&member.cert.device_label))
        .unwrap_or_default()
        .to_owned();
    Relayer {
        device,
        name,
        here: device == own,
    }
}

/// Why no relay of a workspace carried its plane: the relay that stands for the answer, and whether
/// it refused this device.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Missed {
    relay: [u8; 32],
    refused: bool,
}

/// What one granted preface asked for, once its certificate is bound to the handshake and to the
/// workspace's roster.
struct Request {
    workspace: WorkspaceId,
    device: [u8; 32],
}

/// Bind a grant to the transport and the roster:
///
/// * the certificate is byte for byte the row the workspace's governance
///   holds for its device — the membership every device already syncs, so a
///   device presenting another device's certificate, or one the workspace
///   removed, is refused;
/// * the connection's endpoint speaks for that device: it is the device's own
///   key, or one the device signed an [`EndpointDelegation`] for, live now,
///   which is how a page whose device key cannot leave `WebCrypto` dials;
/// * the certificate has not expired, and the workspace is one this device
///   relays now, by the same rule every member applies ([`relays_of`]).
fn bind(
    peer: Peer,
    grant: &grant::Native,
    members: &[Member],
    host: Option<[u8; 32]>,
    own: [u8; 32],
    now_ns: u64,
) -> Result<Request, Denied> {
    let Peer(endpoint) = peer;
    let workspace = WorkspaceId::parse(&live::workspace_id(grant.workspace)).ok_or(Denied)?;
    // One scope per workspace today: the workspace itself.
    if grant.scope != scope_of(workspace).id() {
        return Err(Denied);
    }
    // Every member that may relay serves one: it is reached when the ones above it are not, and
    // bridges the others while they are.
    if !relays_of(members, host, now_ns).contains(&own) {
        return Err(Denied);
    }
    let certificate: DeviceCert = serde_json::from_slice(&grant.certificate).map_err(|_| Denied)?;
    if certificate.is_expired_at(now_ns) {
        return Err(Denied);
    }
    let device = certificate.device_public_key;
    let member = members
        .iter()
        .find(|member| member.device == device)
        .ok_or(Denied)?;
    if certificate != member.cert {
        return Err(Denied);
    }
    if endpoint != device {
        let delegation: EndpointDelegation =
            serde_json::from_slice(&grant.delegation).map_err(|_| Denied)?;
        if delegation.device_public_key != device {
            return Err(Denied);
        }
        delegation.verify(&endpoint, now_ns).map_err(|_| Denied)?;
    }
    Ok(Request { workspace, device })
}

/// One admitted session: its publication, the scope it hears, and the watch it is cut by.
struct Permit {
    engine: Arc<Engine>,
    changed: tokio::sync::watch::Receiver<u64>,
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
        let Ok(members) = channels::members_of(&self.engine, self.workspace) else {
            return false;
        };
        if !members.iter().any(|member| member.device == name.device()) {
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
}

/// One joined plane: through which relay, and the session itself.
struct Joined {
    relay: [u8; 32],
    plane: Arc<Plane>,
}

/// Another relay of a workspace this device relays, kept in this device's relay as one subscriber
/// session: the member joined the other relay, and everything it serves crosses here.
struct Bridged {
    _copy: tilo_live::bridge::Bridge,
}

/// What the relay role and the planes share: this session's engine, its own device, the epochs it
/// saw, what it joined, and the change signal an open session re-observes on.
struct Shared {
    engine: Arc<Engine>,
    device: [u8; 32],
    /// The newest content-key epoch seen in each workspace: a grant that predates a rotation this
    /// device already applied is refused.
    epochs: Mutex<BTreeMap<WorkspaceId, u64>>,
    /// The devices each workspace admitted when this device last read it: a
    /// member the workspace removed is a change the open sessions re-observe,
    /// so its session is cut on the removal and not on its next request.
    rosters: Mutex<BTreeMap<WorkspaceId, BTreeSet<[u8; 32]>>>,
    /// Told when a workspace's membership or keys moved.
    changed: tokio::sync::watch::Sender<u64>,
    planes: Mutex<BTreeMap<WorkspaceId, Joined>>,
    /// How far each workspace's plane came here, as it was last settled ([`Standing`]).
    standings: Mutex<BTreeMap<WorkspaceId, Standing>>,
    /// Counts each move of a standing and each plane opened: what the session reads the standings
    /// and the planes again on, and not on every pass.
    moves: std::sync::atomic::AtomicU64,
    /// The other relays of each workspace this device relays, by device.
    bridged: Mutex<BTreeMap<(WorkspaceId, [u8; 32]), Bridged>>,
    /// What wakes each workspace's reconciler beside the workspace's own changes: a plane of it
    /// ended, so the workspace is settled again through whichever relay it reaches next, or this
    /// session was given a lender of media keys ([`Live::lend_media_keys`]).
    nudges: Mutex<BTreeMap<WorkspaceId, Arc<tokio::sync::Notify>>>,
    /// Told when the session's workspaces are to be read again for ones no reconciler runs for
    /// yet, beside the keys that announce a workspace joined later.
    readopt: tokio::sync::Notify,
    wake: Arc<dyn Fn() + Send + Sync>,
    /// The runtime the planes run on, once the plane's thread started it: what a plane's readers
    /// are started on, whichever thread asks the plane to show a track.
    runtime: std::sync::OnceLock<tokio::runtime::Handle>,
    /// Where a device that holds none of a workspace's media keys is lent the keys of its own
    /// streams ([`MediaKeysLender`]): a session in a workspace's VM, once it can ask its host.
    lender: std::sync::OnceLock<Arc<dyn MediaKeysLender>>,
}

/// One sender's media keys, lent: what a device confined to its own record publishes its presence,
/// windows and pages with on a workspace's live plane, holding nothing that opens anyone's stream
/// ([`tilo_live::plane::PlaneKeys::Sender`]).
pub(crate) trait MediaKeysLender: Send + Sync {
    /// This device's own media keys in `workspace` at the media channel's current epoch, as its
    /// host lends them; `None` when they could not be had now.
    fn lend(
        &self,
        workspace: WorkspaceId,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Option<channel_crypto::SenderMediaKeys>> + Send + '_>,
    >;
}

/// How long one ask for lent keys is waited for before the plane waits for the next event instead.
const LEND_PATIENCE: Duration = Duration::from_secs(20);

impl RelayAuthority for Shared {
    fn admit<'a>(&'a self, peer: Peer, proof: &'a [u8]) -> Admission<'a> {
        Box::pin(std::future::ready(self.decide(peer, proof)))
    }
}

impl Shared {
    /// Whether `peer` may take part in the plane its grant (`proof`) names, from what this
    /// device holds of the workspace now.
    fn decide(&self, peer: Peer, proof: &[u8]) -> Result<Arc<dyn SessionAuthorization>, Denied> {
        let Peer(_) = peer;
        let grant = grant::Native::parse(proof).ok_or(Denied)?;
        let workspace = WorkspaceId::parse(&live::workspace_id(grant.workspace)).ok_or(Denied)?;
        let members = channels::members_of(&self.engine, workspace).map_err(|_| Denied)?;
        let host = self.engine.host_of(workspace);
        let request = bind(peer, &grant, &members, host, self.device, now_ns())?;
        // A device that sealed under an epoch this relay already moved past would publish nothing
        // anyone opens: it is refused at the preface.
        let newest = self
            .epochs
            .lock_recover()
            .get(&workspace)
            .copied()
            .unwrap_or(0);
        if grant.epoch < newest {
            return Err(Denied);
        }
        let scope = scope_of(request.workspace);
        Ok(Arc::new(Permit {
            engine: Arc::clone(&self.engine),
            changed: self.changed.subscribe(),
            device: request.device,
            workspace: request.workspace,
            workspace_name: request.workspace.to_string(),
            publication: Namespace::new(&scope, request.device).path(),
            prefix: live::participants(&scope),
            scope: scope.id(),
        }))
    }
}

/// The first relay of `candidates` whose plane `open` returned, trying them in the order the
/// workspace names them: the relay that is offline costs one attempt and the plane opens on the one
/// below it. When none carries it, the relay that says why: the first that refused this device,
/// since a refusal is an answer that waiting does not change, and otherwise the first one tried.
async fn reach<'a, T>(
    candidates: &[[u8; 32]],
    mut open: impl FnMut(
        [u8; 32],
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<T, PlaneError>> + Send + 'a>,
    >,
) -> Result<([u8; 32], T), Option<Missed>>
where
    T: 'a,
{
    let mut missed: Option<Missed> = None;
    for candidate in candidates.iter().copied() {
        match open(candidate).await {
            Ok(opened) => return Ok((candidate, opened)),
            Err(error) => {
                tilo_failure::ignored!(
                    "an unreachable relay is skipped, and the next is tried",
                    tracing::debug!(relay = %hex::encode(candidate), %error,
                        "this relay of the workspace was not reached")
                );
                let refused = matches!(error, PlaneError::Denied);
                if missed.is_none_or(|known| refused && !known.refused) {
                    missed = Some(Missed {
                        relay: candidate,
                        refused,
                    });
                }
            }
        }
    }
    Err(missed)
}

impl Shared {
    /// This workspace's members and the devices among them that may relay its live plane, in the
    /// order every device tries them; `None` — after withdrawing the plane — when the membership is
    /// unknown or names no relay.
    fn relay_candidates(&self, workspace: WorkspaceId) -> Option<(Vec<Member>, Vec<[u8; 32]>)> {
        let members = match channels::members_of(&self.engine, workspace) {
            Ok(members) => members,
            Err(error) => {
                tracing::warn!(%workspace, %error, "the live plane's membership is unknown");
                self.withdraw(workspace);
                self.stand(workspace, Standing::Unread);
                return None;
            }
        };
        let candidates = relays_of(&members, self.engine.host_of(workspace), now_ns());
        if candidates.is_empty() {
            // No member may relay it: the workspace has no live plane.
            if self.planes.lock_recover().contains_key(&workspace) {
                tracing::info!(%workspace, "no member relays this workspace's live plane any more");
            }
            self.withdraw(workspace);
            self.stand(workspace, Standing::NoRelay);
            return None;
        }
        Some((members, candidates))
    }

    /// The keys of this device's own streams in `workspace`, lent by its host, when this session
    /// has a lender and the host answers in time.
    async fn lent(&self, workspace: WorkspaceId) -> Option<channel_crypto::SenderMediaKeys> {
        let lender = Arc::clone(self.lender.get()?);
        match tokio::time::timeout(LEND_PATIENCE, lender.lend(workspace)).await {
            Ok(Some(lent)) if lent.sender() == self.device => Some(lent),
            Ok(_) => None,
            Err(_) => {
                // Nothing this device shows reaches anyone until it has them: worth a warning.
                tracing::warn!(%workspace, "the host lent no media keys in time");
                None
            }
        }
    }

    /// Read `workspace` as it is now and join through the first of its relays
    /// this device reaches, move the plane's keys and roster when they changed,
    /// or leave when the workspace names no relay any more.
    async fn settle(self: &Arc<Self>, workspace: WorkspaceId, endpoint: &Endpoint, relay: &Relay) {
        let Some((members, candidates)) = self.relay_candidates(workspace) else {
            return;
        };
        // The plane's tracks are sealed under the workspace's media channel: taking part in it asks
        // the workspace's devices for access, and the plane opens on the keys arriving.
        let keys = self.engine.seal_keys(
            workspace,
            collab_types::scope::MEDIA_APP,
            collab_types::scope::WORKSPACE_RECORD,
        );
        // A device that may hold none of them (a session in a workspace's VM) is lent its own
        // streams' keys instead, asked afresh at each join: a plane whose epoch moved is cut, and
        // joins again on the keys its host lends then.
        let (keys, lent) = match keys {
            Ok(crate::engine::SealKeys::Here(keys)) => (PlaneKeys::from(keys), false),
            held => {
                let Some(lent) = self.lent(workspace).await else {
                    self.without_keys(workspace, &held);
                    return;
                };
                (PlaneKeys::from(lent), true)
            }
        };
        self.observe(workspace, keys.epoch());
        self.observe_roster(workspace, &members);
        let roster = Roster::new(members.iter().map(|member| member.device));
        let scope = scope_of(workspace);
        let kept = {
            let planes = self.planes.lock_recover();
            planes
                .get(&workspace)
                .filter(|joined| candidates.contains(&joined.relay) && !joined.plane.ended())
                .map(|joined| (joined.relay, Arc::clone(&joined.plane)))
        };
        if let Some((at, plane)) = kept {
            // The workspace moved under a relay that still carries it: the plane keeps its sessions
            // and seals under the keys and roster it now has.
            tracing::debug!(%workspace, relay = %hex::encode(at),
                epoch = keys.epoch(), "the live plane keeps its relay");
            let epoch = keys.epoch();
            plane.set_keys(keys);
            plane.set_roster(roster);
            self.stand(
                workspace,
                Standing::Open {
                    relay: relayer(&members, at, self.device),
                    epoch,
                    lent,
                },
            );
            self.bridge(workspace, endpoint, relay, &scope, epoch, &candidates)
                .await;
            return;
        }
        // A plane whose relay the workspace no longer names, or one that ended, is closed before
        // another is opened in its place.
        drop(self.planes.lock_recover().remove(&workspace));
        let epoch = keys.epoch();
        let join = Join {
            scope: scope.clone(),
            device: self.device,
            keys,
            roster,
            publish: true,
        };
        let reached = self
            .join_through(workspace, endpoint, relay, &join, &candidates)
            .await;
        let (chosen, plane) = match reached {
            Ok(reached) => reached,
            Err(missed) => {
                self.missed(workspace, &members, missed);
                return;
            }
        };
        tracing::debug!(%workspace, relay = %hex::encode(chosen),
            local = chosen == self.device, epoch,
            "the live plane opened");
        let open = Standing::Open {
            relay: relayer(&members, chosen, self.device),
            epoch,
            lent,
        };
        self.keep(workspace, chosen, plane, open);
        self.bridge(workspace, endpoint, relay, &scope, epoch, &candidates)
            .await;
    }

    /// `workspace` 's keys are not here (`held`), and none were lent: the plane opens when they
    /// are, and until then it stands as waiting for them, or as holding none where none are coming.
    /// A device whose host lends it keys waits for those. A plane already open keeps what it has
    /// until they move.
    fn without_keys(&self, workspace: WorkspaceId, held: &Result<crate::engine::SealKeys, String>) {
        tracing::debug!(%workspace, "the live plane waits for the workspace's keys");
        if self.joined(workspace) {
            return;
        }
        let waiting =
            matches!(held, Ok(crate::engine::SealKeys::Waiting)) || self.lender.get().is_some();
        self.stand(
            workspace,
            if waiting {
                Standing::WaitingForKeys
            } else {
                Standing::Keyless
            },
        );
    }

    /// Join `workspace` 's plane as `join` through the first of `candidates` this device reaches:
    /// its own relay when it is one of them, and otherwise that device's, dialled over iroh with
    /// this device's certificate as its grant.
    async fn join_through(
        self: &Arc<Self>,
        workspace: WorkspaceId,
        endpoint: &Endpoint,
        relay: &Relay,
        join: &Join,
        candidates: &[[u8; 32]],
    ) -> Result<([u8; 32], Plane), Option<Missed>> {
        let epoch = join.keys.epoch();
        let host = self.engine.workspace_host(workspace);
        reach(candidates, |chosen| {
            let join = join.clone();
            let endpoint = endpoint.clone();
            let host = host.clone();
            Box::pin(async move {
                if chosen == self.device {
                    return Plane::local(relay, &join);
                }
                let relay_id =
                    EndpointId::from_bytes(&chosen).map_err(|_| PlaneError::Unavailable)?;
                let grant = self
                    .grant(workspace, &join.scope, epoch)
                    .map_err(|_| PlaneError::Unavailable)?;
                // The member this enrollment pinned is reached at the address its host signed,
                // which is its endpoint's, not its device key's.
                let addr = host
                    .as_ref()
                    .filter(|(pinned, _)| *pinned == chosen)
                    .map_or_else(|| EndpointAddr::new(relay_id), |(_, addr)| addr.clone());
                let carrier = native::Iroh::new(endpoint, addr);
                Plane::join_within(Arc::new(carrier), &grant, join, ATTEMPT).await
            })
        })
        .await
    }

    /// Keep `plane`, opened through the relay `chosen`, as `workspace` 's: it wakes the session on
    /// what it hears, has the workspace settled again when it ends, and stands as `open` says.
    fn keep(
        self: &Arc<Self>,
        workspace: WorkspaceId,
        chosen: [u8; 32],
        plane: Plane,
        open: Standing,
    ) {
        plane.set_wake(Arc::clone(&self.wake));
        let ended = Arc::clone(self);
        plane.set_ended(Arc::new(move || ended.ended(workspace)));
        let replaced = self.planes.lock_recover().insert(
            workspace,
            Joined {
                relay: chosen,
                plane: Arc::new(plane),
            },
        );
        drop(replaced);
        self.stand(workspace, open);
        // A plane opened again where one stood is another plane, whatever its standing says.
        self.moved();
        self.bump();
    }

    /// Tell the session that a standing moved or a plane opened: it reads both again.
    fn moved(&self) {
        self.moves
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        (self.wake)();
    }

    /// The grant this device presents to be admitted into `workspace` 's scope: its certificate,
    /// the workspace's uuid bytes and the epoch it seals under.
    fn grant(&self, workspace: WorkspaceId, scope: &Scope, epoch: u64) -> Result<Vec<u8>, String> {
        let certificate = serde_json::to_vec(&self.engine.identity().device_cert)
            .map_err(|error| error.to_string())?;
        grant::Native::new(
            *workspace.as_uuid().as_bytes(),
            &scope.id(),
            epoch,
            certificate,
            Vec::new(),
        )
        .map(|grant| grant.encode())
    }

    /// Note that `workspace` seals under `epoch` from now on, and tell the sessions open on this
    /// relay.
    fn observe(&self, workspace: WorkspaceId, epoch: u64) {
        let mut epochs = self.epochs.lock_recover();
        let newest = epochs.get(&workspace).copied().unwrap_or(0);
        if epoch <= newest {
            return;
        }
        epochs.insert(workspace, epoch);
        drop(epochs);
        self.bump();
    }

    /// Note who `workspace` admits now, and tell the sessions open on this relay when that moved.
    fn observe_roster(&self, workspace: WorkspaceId, members: &[Member]) {
        let devices: BTreeSet<[u8; 32]> = members.iter().map(|member| member.device).collect();
        let moved = {
            let mut rosters = self.rosters.lock_recover();
            match rosters.insert(workspace, devices.clone()) {
                Some(before) => before != devices,
                None => false,
            }
        };
        if moved {
            self.bump();
        }
    }

    /// Whether this device is in `workspace` 's plane now.
    fn joined(&self, workspace: WorkspaceId) -> bool {
        self.planes
            .lock_recover()
            .get(&workspace)
            .is_some_and(|joined| !joined.plane.ended())
    }

    /// Note how far `workspace` 's plane came, and wake the session when that moved: what it
    /// publishes of the plane is read again. Answers whether it moved.
    fn stand(&self, workspace: WorkspaceId, standing: Standing) -> bool {
        let moved = {
            let mut standings = self.standings.lock_recover();
            let moved = standings.get(&workspace) != Some(&standing);
            if moved {
                standings.insert(workspace, standing);
            }
            moved
        };
        if moved {
            self.moved();
        }
        moved
    }

    /// No relay of `workspace` carried its plane: the plane stands as waiting for the relay
    /// `missed` names, or as refused by it, and the journal says so when that is news.
    fn missed(&self, workspace: WorkspaceId, members: &[Member], missed: Option<Missed>) {
        let Some(missed) = missed else {
            return;
        };
        let relay = relayer(members, missed.relay, self.device);
        let standing = if missed.refused {
            Standing::Refused { relay }
        } else {
            Standing::Unreached { relay }
        };
        if self.stand(workspace, standing) {
            tracing::warn!(%workspace, relay = %hex::encode(missed.relay), refused = missed.refused,
                "the live plane is not open: no device that relays this workspace took this device in");
        } else {
            tracing::debug!(%workspace, "no relay of this workspace was reached");
        }
    }

    /// What wakes `workspace` 's reconciler when a plane of it ended.
    fn nudge(&self, workspace: WorkspaceId) -> Arc<tokio::sync::Notify> {
        Arc::clone(self.nudges.lock_recover().entry(workspace).or_default())
    }

    /// Whether `workspace` names a relay and none took this device in when it was last settled.
    fn awaits_relay(&self, workspace: WorkspaceId) -> bool {
        matches!(
            self.standings.lock_recover().get(&workspace),
            Some(Standing::Unreached { .. } | Standing::Refused { .. })
        )
    }

    /// Stop carrying `workspace` : the plane is closed and its sessions cut.
    fn withdraw(&self, workspace: WorkspaceId) {
        self.rosters.lock_recover().remove(&workspace);
        self.bridged
            .lock_recover()
            .retain(|(held, _), _| *held != workspace);
        let gone = self.planes.lock_recover().remove(&workspace);
        if gone.is_some() {
            self.bump();
        }
    }

    /// A plane of `workspace` ended: settle the workspace again, through the first relay it reaches
    /// now.
    fn ended(&self, workspace: WorkspaceId) {
        self.planes.lock_recover().remove(&workspace);
        // Until it is settled again the plane is not open: the relay it ran through is the one it
        // waits to reach.
        let was = self.standings.lock_recover().get(&workspace).cloned();
        if let Some(Standing::Open { relay, .. }) = was {
            self.stand(workspace, Standing::Unreached { relay });
        }
        self.nudge(workspace).notify_one();
    }

    /// Keep every other relay of `workspace` bridged into this device's own,
    /// when this device relays it: a member that reached another relay is
    /// heard here, and a member here is heard there through that relay's own
    /// bridge.
    async fn bridge(
        &self,
        workspace: WorkspaceId,
        endpoint: &Endpoint,
        relay: &Relay,
        scope: &Scope,
        epoch: u64,
        candidates: &[[u8; 32]],
    ) {
        let relays_here = candidates.contains(&self.device);
        let others: Vec<[u8; 32]> = candidates
            .iter()
            .copied()
            .filter(|device| relays_here && *device != self.device)
            .collect();
        self.bridged
            .lock_recover()
            .retain(|(held, device), _| *held != workspace || others.contains(device));
        let host = self.engine.workspace_host(workspace);
        for other in others {
            if self
                .bridged
                .lock_recover()
                .contains_key(&(workspace, other))
            {
                continue;
            }
            let Ok(relay_id) = EndpointId::from_bytes(&other) else {
                continue;
            };
            let Ok(grant) = self.grant(workspace, scope, epoch) else {
                continue;
            };
            let addr = host
                .as_ref()
                .filter(|(pinned, _)| *pinned == other)
                .map_or_else(|| EndpointAddr::new(relay_id), |(_, addr)| addr.clone());
            let carrier = native::Iroh::new(endpoint.clone(), addr);
            match tilo_live::bridge::Bridge::join(&carrier, &grant, scope, relay, BRIDGE_JOINT)
                .await
            {
                Ok(bridge) => {
                    tracing::debug!(%workspace, relay = %hex::encode(other),
                        "the other relay of this workspace is bridged here");
                    self.bridged
                        .lock_recover()
                        .insert((workspace, other), Bridged { _copy: bridge });
                }
                Err(error) => tilo_failure::ignored!(
                    "an unreachable relay is skipped, and the next is tried",
                    tracing::debug!(%workspace, relay = %hex::encode(other), %error,
                        "the other relay of this workspace was not reached")
                ),
            }
        }
    }

    fn bump(&self) {
        self.changed
            .send_modify(|count| *count = count.wrapping_add(1));
    }
}

/// The live plane of one session: its relay role and the planes it joined,
/// on a thread of their own.
pub struct Live {
    shared: Arc<Shared>,
    /// The relay this device's ALPN handler serves, told to it once it is created: the handler is
    /// installed before the endpoint binds, and the relay is created on the plane's own runtime.
    relayed: tokio::sync::watch::Sender<Option<Arc<Relay>>>,
    /// Told when the session ends; dropping it ends the plane's thread.
    stopped: Option<tokio::sync::mpsc::UnboundedSender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
    /// The ALPN handler this session serves, handed over before the endpoint binds
    /// ([`Self::protocols`]): which workspaces it relays is read per connection, from their state.
    protocols: Protocols,
}

impl std::fmt::Debug for Live {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Live")
            .field(
                "relay",
                &self.shared.engine.identity().device_cert.device_public_key,
            )
            .finish_non_exhaustive()
    }
}

impl Live {
    /// This session's live plane, and what it answers on the endpoint for the workspaces it relays:
    /// its ALPN handler, handed to the session before the endpoint binds
    /// ([`crate::sync::Client::start`]).
    /// Nothing runs until [`Self::attach`] hands it the session's endpoint.
    pub(crate) fn new(engine: Arc<Engine>, wake: Arc<dyn Fn() + Send + Sync>) -> Self {
        let device = engine.identity().device_cert.device_public_key;
        let (changed, _) = tokio::sync::watch::channel(0);
        let shared = Arc::new(Shared {
            engine,
            device,
            epochs: Mutex::new(BTreeMap::new()),
            rosters: Mutex::new(BTreeMap::new()),
            changed,
            planes: Mutex::new(BTreeMap::new()),
            standings: Mutex::new(BTreeMap::new()),
            moves: std::sync::atomic::AtomicU64::new(0),
            bridged: Mutex::new(BTreeMap::new()),
            nudges: Mutex::new(BTreeMap::new()),
            readopt: tokio::sync::Notify::new(),
            wake,
            runtime: std::sync::OnceLock::new(),
            lender: std::sync::OnceLock::new(),
        });
        let (relayed, named) = tokio::sync::watch::channel::<Option<Arc<Relay>>>(None);
        let handler = native::handler(move |session: native::Session| {
            let mut named = named.clone();
            async move {
                let Some(relay) = served(&mut named).await else {
                    return;
                };
                let peer = Peer(session.remote());
                relay.serve_session(session, peer).await;
            }
        });
        let protocols = vec![(
            native::ALPN.to_vec(),
            Box::<dyn iroh::protocol::DynProtocolHandler>::from(handler),
        )];
        Self {
            shared,
            relayed,
            stopped: None,
            thread: None,
            protocols,
        }
    }

    /// Have `lender` lend this device its own streams' media keys in every workspace whose keys
    /// it holds none of, from now on: each such workspace is joined again at once. Only the first
    /// lender counts.
    pub(crate) fn lend_media_keys(&self, lender: Arc<dyn MediaKeysLender>) {
        if self.shared.lender.set(lender).is_err() {
            return;
        }
        // Each workspace's own reconciler settles it again, and a workspace none runs for yet is
        // given one, which settles it at once.
        for workspace in self.shared.engine.workspaces() {
            self.shared.nudge(workspace).notify_one();
        }
        self.shared.readopt.notify_one();
    }

    /// Run for the rest of the session on `endpoint` : this device's relay role, for the workspaces
    /// that name it, and one plane per workspace it holds.
    pub(crate) fn attach(&mut self, endpoint: Endpoint) {
        let (stop, stopped) = tokio::sync::mpsc::unbounded_channel();
        let shared = Arc::clone(&self.shared);
        let relayed = self.relayed.clone();
        match std::thread::Builder::new()
            .name("tilo-live".into())
            .spawn(move || run(shared, endpoint, relayed, stopped))
        {
            Ok(thread) => {
                self.thread = Some(thread);
                self.stopped = Some(stop);
            }
            Err(error) => tracing::warn!(%error, "the live plane runs no thread"),
        }
    }

    /// The protocols this session serves on its endpoint for the live plane.
    pub(crate) fn protocols(&mut self) -> Protocols {
        std::mem::take(&mut self.protocols)
    }

    /// The next object heard in `workspace` 's live plane, or `None` .
    #[must_use]
    pub fn try_recv(&self, workspace: WorkspaceId) -> Option<Heard> {
        self.shared
            .planes
            .lock_recover()
            .get(&workspace)
            .and_then(|joined| joined.plane.try_recv())
    }

    /// The planes this device is in, by workspace: what a call's audio is published into, and who
    /// it hears there.
    #[must_use]
    pub fn planes(&self) -> Vec<(WorkspaceId, Arc<Plane>)> {
        self.shared
            .planes
            .lock_recover()
            .iter()
            .map(|(workspace, joined)| (*workspace, Arc::clone(&joined.plane)))
            .collect()
    }

    /// How far each workspace's plane came on this device, by workspace: open, or what it waits
    /// for. A workspace this session has not looked at yet is not among them.
    #[must_use]
    pub fn standings(&self) -> Vec<(WorkspaceId, Standing)> {
        self.shared
            .standings
            .lock_recover()
            .iter()
            .map(|(workspace, standing)| (*workspace, standing.clone()))
            .collect()
    }

    /// Moves whenever a standing did or a plane opened: [`Self::standings`] and [`Self::planes`]
    /// say nothing new while it stands still.
    #[must_use]
    pub fn standings_revision(&self) -> u64 {
        self.shared.moves.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// The runtime the planes run on, once it started: a plane asked to show a track starts its
    /// reader there.
    #[must_use]
    pub fn runtime(&self) -> Option<tokio::runtime::Handle> {
        self.shared.runtime.get().cloned()
    }

    /// A handle that says this plane's presence frames, for the transport the session says on.
    pub(crate) fn saying(&self) -> Say {
        Say(Arc::clone(&self.shared))
    }
}

/// This plane's presence track, as the transport the session says on carries it: the words this
/// device says of itself, said in the workspace they are about.
pub(crate) struct Say(Arc<Shared>);

impl std::fmt::Debug for Say {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("live plane")
    }
}

impl crate::sync::SayPresence for Say {
    fn say(
        &self,
        workspace: WorkspaceId,
        frames: &[(u32, collab_types::presence::Presence)],
    ) -> Result<(), String> {
        let plane = self
            .0
            .planes
            .lock_recover()
            .get(&workspace)
            .map(|joined| Arc::clone(&joined.plane))
            .ok_or_else(|| "this workspace has no live plane".to_owned())?;
        tilo_live::presence::say(&plane, frames).map_err(|error| error.to_string())
    }
}

impl Drop for Live {
    fn drop(&mut self) {
        drop(self.stopped.take());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Wait for the relay this device's handler serves, if it ever is one.
async fn served(
    named: &mut tokio::sync::watch::Receiver<Option<Arc<Relay>>>,
) -> Option<Arc<Relay>> {
    loop {
        let held = {
            let now = named.borrow();
            now.clone()
        };
        if held.is_some() {
            return held;
        }
        if named.changed().await.is_err() {
            return None;
        }
    }
}

/// The live plane's thread: its relay role, and one reconciler per workspace,
/// each woken by the channel service when the workspace moved.
fn run(
    shared: Arc<Shared>,
    endpoint: Endpoint,
    relayed: tokio::sync::watch::Sender<Option<Arc<Relay>>>,
    mut stopped: tokio::sync::mpsc::UnboundedReceiver<()>,
) {
    let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    else {
        tracing::warn!("the live plane could not start");
        return;
    };
    let _ = shared.runtime.set(runtime.handle().clone());
    runtime.block_on(async move {
        let authority: Arc<dyn RelayAuthority> = Arc::clone(&shared) as Arc<dyn RelayAuthority>;
        let serving = Arc::new(Relay::new(authority));
        relayed.send_replace(Some(Arc::clone(&serving)));
        tracing::debug!(device = %hex::encode(shared.device),
            "the live plane runs on this session's endpoint");
        let keys = shared.engine.keys();
        let mut keyed = keys.generation();
        let mut known = BTreeSet::new();
        adopt(&shared, &endpoint, &serving, &mut known);
        // A workspace this session joins later opens its plane the same way: its channel keys
        // arriving is announced by the channel service, and the workspace list is read again then.
        // Each workspace's own reconciler settles it again when a plane of it ended, or when no
        // relay took this device in ([`reconcile`]).
        loop {
            tokio::select! {
                _ = stopped.recv() => break,
                changed = keys.changed(keyed) => {
                    keyed = changed;
                    adopt(&shared, &endpoint, &serving, &mut known);
                }
                () = shared.readopt.notified() => {
                    adopt(&shared, &endpoint, &serving, &mut known);
                }
            }
        }
    });
}

/// Start a reconciler for every workspace of `shared` this session has not opened a plane for yet.
fn adopt(
    shared: &Arc<Shared>,
    endpoint: &Endpoint,
    relay: &Arc<Relay>,
    known: &mut BTreeSet<WorkspaceId>,
) {
    for workspace in shared.engine.workspaces() {
        if known.insert(workspace) {
            tokio::spawn(reconcile(
                Arc::clone(shared),
                workspace,
                endpoint.clone(),
                Arc::clone(relay),
            ));
        }
    }
}

/// Keep one workspace's plane in step with its membership and keys, on the event that moved them:
/// the workspace's media channel changed, or a plane of the workspace ended.
///
/// One thing has no event. A relay that was away when this device asked, or that did not know this
/// device yet, says nothing when that changes, and the keys this device already holds do not move
/// because of it. So while the workspace names a relay and none took this device in, its relays
/// are asked again after [`retry_wait`], until the plane opens. Without that a device whose session
/// started before the device relaying the workspace was reachable stayed out of the workspace's
/// live plane for as long as both ran: no presence, no call and no shared window crossed, and
/// nothing said so.
async fn reconcile(
    shared: Arc<Shared>,
    workspace: WorkspaceId,
    endpoint: Endpoint,
    relay: Arc<Relay>,
) {
    let channel = scope_of(workspace).channel_id();
    let keys = shared.engine.keys();
    let nudge = shared.nudge(workspace);
    let mut after = 0;
    let mut missed: u32 = 0;
    loop {
        Arc::clone(&shared)
            .settle(workspace, &endpoint, &relay)
            .await;
        missed = if shared.awaits_relay(workspace) {
            missed.saturating_add(1)
        } else {
            0
        };
        let again = async {
            if missed == 0 {
                std::future::pending::<()>().await;
            }
            tokio::time::sleep(retry_wait(missed)).await;
        };
        tokio::select! {
            changed = keys.channel_changed(channel, after) => after = changed,
            () = nudge.notified() => {}
            () = again => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_user_identity::DeviceCapabilities;

    fn cert(device: [u8; 32], host: bool, expires_at_ns: Option<u64>) -> DeviceCert {
        DeviceCert {
            user_public_key: [1; 32],
            device_public_key: device,
            device_label: format!("device {}", device[0]),
            capabilities: DeviceCapabilities {
                can_act_as_host: host,
                can_own_workspaces: !host,
                ..DeviceCapabilities::nothing()
            },
            issued_at_ns: 1,
            expires_at_ns,
            signature: [0; 64],
            issuer: None,
        }
    }

    fn member(device: [u8; 32], host: bool, expires_at_ns: Option<u64>) -> Member {
        Member {
            device,
            cert: cert(device, host, expires_at_ns),
        }
    }

    #[test]
    fn the_relays_are_the_host_capable_members_in_key_order() {
        let members = vec![
            member([9; 32], false, None),
            member([5; 32], true, None),
            member([3; 32], true, None),
            member([1; 32], false, None),
        ];
        assert_eq!(relays_of(&members, None, 10), vec![[3; 32], [5; 32]]);
    }

    #[tokio::test]
    async fn the_first_reachable_relay_carries_the_plane() {
        let candidates = [[1; 32], [2; 32], [3; 32]];
        let reached = reach(&candidates, |relay| {
            Box::pin(async move {
                if relay == [1; 32] {
                    return Err(PlaneError::Unavailable);
                }
                Ok(relay)
            })
        })
        .await;
        assert_eq!(
            reached,
            Ok(([2; 32], [2; 32])),
            "the plane opens on the next relay"
        );
    }

    #[tokio::test]
    async fn a_workspace_whose_relays_are_all_away_opens_no_plane() {
        let candidates = [[1; 32], [2; 32]];
        let reached = reach(&candidates, |_| {
            Box::pin(async move { Err::<(), _>(PlaneError::Unavailable) })
        })
        .await;
        assert_eq!(
            reached,
            Err(Some(Missed {
                relay: [1; 32],
                refused: false
            })),
            "the first relay is the one the plane waits to reach"
        );
    }

    /// A relay that answers and refuses this device is why the plane is not open, whichever relay
    /// beside it was merely away: waiting reaches the one, and changes nothing about the other.
    #[tokio::test]
    async fn a_relay_that_refuses_is_named_before_one_that_is_away() {
        let candidates = [[1; 32], [2; 32], [3; 32]];
        let reached = reach(&candidates, |relay| {
            Box::pin(async move {
                Err::<(), _>(if relay == [2; 32] {
                    PlaneError::Denied
                } else {
                    PlaneError::Unavailable
                })
            })
        })
        .await;
        assert_eq!(
            reached,
            Err(Some(Missed {
                relay: [2; 32],
                refused: true
            }))
        );
    }

    /// A relay is named by what its person called it, as the workspace's certificate of it says,
    /// and a device they never named by nothing.
    #[test]
    fn a_relay_is_named_as_its_certificate_names_it() {
        let members = vec![member([1; 32], false, None), member([2; 32], false, None)];
        assert_eq!(
            relayer(&members, [2; 32], [1; 32]),
            Relayer {
                device: [2; 32],
                name: "device 2".to_owned(),
                here: false
            }
        );
        assert!(relayer(&members, [1; 32], [1; 32]).here);
        assert_eq!(relayer(&members, [7; 32], [1; 32]).name, "");
    }

    /// A workspace whose relays did not take this device in asks them again soon, then ever less
    /// often, and never stops asking: the relay it waits for may come back at any time.
    #[test]
    fn a_relay_that_was_missed_is_asked_again_ever_less_often_and_always() {
        assert_eq!(retry_wait(1), RETRY_FIRST);
        assert_eq!(retry_wait(2), RETRY_FIRST * 2);
        assert_eq!(retry_wait(3), RETRY_FIRST * 4);
        let waits: Vec<Duration> = (1..40).map(retry_wait).collect();
        assert!(waits.windows(2).all(|pair| pair[0] <= pair[1]), "{waits:?}");
        assert_eq!(waits.last(), Some(&RETRY_LONGEST));
        assert_eq!(retry_wait(u32::MAX), RETRY_LONGEST);
        assert_eq!(retry_wait(0), RETRY_FIRST);
    }

    #[test]
    fn without_a_host_capable_member_the_device_hosting_the_workspace_relays() {
        let members = vec![member([1; 32], false, None), member([2; 32], false, None)];
        assert_eq!(relays_of(&members, Some([2; 32]), 10), vec![[2; 32]]);
        assert!(
            relays_of(&members, None, 10).is_empty(),
            "a workspace nobody hosts here and nobody may relay has no live plane"
        );
        assert!(
            relays_of(&members, Some([7; 32]), 10).is_empty(),
            "a host that is no member relays nothing"
        );
        let members = vec![member([1; 32], false, None), member([5; 32], true, None)];
        assert_eq!(
            relays_of(&members, Some([1; 32]), 10),
            vec![[5; 32]],
            "a host-capable member takes the work from the device hosting the workspace"
        );
        let members = vec![
            member([1; 32], false, Some(10)),
            member([2; 32], false, None),
        ];
        assert!(relays_of(&members, Some([1; 32]), 10).is_empty());
    }

    #[test]
    fn an_ordinary_device_hosting_the_workspace_admits_its_members() {
        let workspace = WorkspaceId::new();
        let mine = cert([1; 32], false, None);
        let other = cert([2; 32], false, None);
        let members = vec![
            Member {
                device: [1; 32],
                cert: mine,
            },
            Member {
                device: [2; 32],
                cert: other.clone(),
            },
        ];
        let asked = grant(workspace, &other);
        assert!(
            bind(Peer([2; 32]), &asked, &members, Some([1; 32]), [1; 32], 10).is_ok(),
            "the device hosting the workspace relays it"
        );
        assert!(
            bind(Peer([2; 32]), &asked, &members, Some([2; 32]), [1; 32], 10).is_err(),
            "a device that hosts nothing here and may not host relays nothing"
        );
    }

    #[test]
    fn an_expired_certificate_relays_nothing() {
        let members = vec![member([1; 32], true, Some(10)), member([2; 32], true, None)];
        assert_eq!(relays_of(&members, None, 10), vec![[2; 32]]);
        assert_eq!(relays_of(&members, None, 11), vec![[2; 32]]);
        let members = vec![member([1; 32], true, Some(10))];
        assert!(relays_of(&members, None, 10).is_empty());
    }

    /// Whether a relay that is `own`, in a workspace nobody hosts here, admits `grant` from the
    /// endpoint `peer` at `now_ns`.
    fn admits(
        peer: [u8; 32],
        grant: &grant::Native,
        members: &[Member],
        own: [u8; 32],
        now_ns: u64,
    ) -> bool {
        bind(Peer(peer), grant, members, None, own, now_ns).is_ok()
    }

    fn grant(workspace: WorkspaceId, device: &DeviceCert) -> grant::Native {
        grant::Native::new(
            *workspace.as_uuid().as_bytes(),
            &scope_of(workspace).id(),
            1,
            serde_json::to_vec(device).unwrap(),
            Vec::new(),
        )
        .unwrap()
    }

    #[test]
    fn a_grant_is_admitted_only_for_the_handshakes_own_device() {
        let workspace = WorkspaceId::new();
        let mine = cert([1; 32], true, None);
        let other = cert([2; 32], false, None);
        let members = vec![
            Member {
                device: [1; 32],
                cert: mine.clone(),
            },
            Member {
                device: [2; 32],
                cert: other.clone(),
            },
        ];
        assert!(
            admits([1; 32], &grant(workspace, &mine), &members, [1; 32], 10),
            "the relay admits the device whose certificate it holds"
        );
        assert!(
            !admits([2; 32], &grant(workspace, &mine), &members, [1; 32], 10),
            "a device presenting another's certificate is refused"
        );
        assert!(
            !admits([3; 32], &grant(workspace, &other), &members, [1; 32], 10),
            "a device that is not on the roster is refused"
        );
    }

    /// A page keeps its device key unextractable, so it dials under a key of its own and presents
    /// the device's signature for it: the relay admits that endpoint as the device, and refuses an
    /// endpoint the device never signed for or whose delegation ran out.
    #[test]
    fn a_delegated_transport_is_admitted_as_its_device_only_while_it_lasts() {
        let workspace = WorkspaceId::new();
        let device_key = ed25519_dalek::SigningKey::from_bytes(&[1; 32]);
        let page_key = ed25519_dalek::SigningKey::from_bytes(&[9; 32]);
        let page = page_key.verifying_key().to_bytes();
        let mine = cert(device_key.verifying_key().to_bytes(), true, None);
        let members = vec![Member {
            device: mine.device_public_key,
            cert: mine.clone(),
        }];
        let delegation = tilo_user_identity::sign_endpoint_delegation(
            &device_key,
            page,
            100,
            60 * 60 * 1_000_000_000,
        )
        .unwrap();
        let mut delegated = grant(workspace, &mine);
        delegated.delegation = serde_json::to_vec(&delegation).unwrap();
        assert!(
            admits(page, &delegated, &members, mine.device_public_key, 200),
            "the page's endpoint speaks for the device that signed for it"
        );
        assert!(
            !admits(
                page,
                &grant(workspace, &mine),
                &members,
                mine.device_public_key,
                200
            ),
            "without the device's signature the endpoint is somebody else"
        );
        let later = 100 + 60 * 60 * 1_000_000_000;
        assert!(
            !admits(page, &delegated, &members, mine.device_public_key, later),
            "an expired delegation admits nobody"
        );
        let mut anyone = delegated;
        anyone.delegation = serde_json::to_vec(
            &tilo_user_identity::sign_endpoint_delegation(&page_key, page, 100, 1000).unwrap(),
        )
        .unwrap();
        assert!(
            !admits(page, &anyone, &members, mine.device_public_key, 200),
            "an endpoint cannot vouch for itself"
        );
    }

    #[test]
    fn a_grant_must_name_a_relay_of_this_workspace_and_this_scope() {
        let workspace = WorkspaceId::new();
        let mine = cert([2; 32], true, None);
        let lower = cert([1; 32], true, None);
        let members = vec![
            Member {
                device: [2; 32],
                cert: mine.clone(),
            },
            Member {
                device: [1; 32],
                cert: lower,
            },
        ];
        assert!(
            admits([2; 32], &grant(workspace, &mine), &members, [2; 32], 10),
            "every host-capable member relays, so this one admits too"
        );
        let mut elsewhere = grant(workspace, &mine);
        elsewhere.scope =
            Scope::channel(workspace.to_string(), collab_types::ChannelId::default()).id();
        assert!(
            !admits([2; 32], &elsewhere, &members, [2; 32], 10),
            "a scope this session carries no plane for is refused"
        );
    }

    #[test]
    fn an_expired_certificate_is_refused_at_the_preface() {
        let workspace = WorkspaceId::new();
        let expired = cert([1; 32], true, Some(5));
        let members = vec![Member {
            device: [1; 32],
            cert: expired.clone(),
        }];
        assert!(
            !admits([1; 32], &grant(workspace, &expired), &members, [1; 32], 5),
            "an expired certificate relays nothing and admits nothing"
        );
    }
}
