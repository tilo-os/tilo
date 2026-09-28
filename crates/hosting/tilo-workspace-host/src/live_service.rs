//! LIVE_V1 host authority, shared by both hosts over the [`HostOwner`] seam: issues host-signed live grants on the proof-required
//! `POST /peer/v3/live/grant` route, admits them from the carrier's admission
//! preface on `/moq/live`, rechecks current roster permission
//! on every relay observation, validates live frames with the canonical wire
//! codec, and forwards admitted signed changes to `ops/<record>` tracks.
//!
//! Decisions:
//! * A grant is bound to `(device, participation_id)`. Renewal is a new grant
//!   for the same pair; it extends the relay-side lease of a connected session
//!   without reconnecting. Grants of a previous host boot never verify.
//! * Grants are signed by the pinned workspace host device key (the host the
//!   data session proved); `root` is the device certificate's user key.
//! * Directions are the authorized subset of the request: a viewer (no room
//!   write capability) receives a subscribe-only grant; a request with no
//!   permitted direction is refused.
//! * `membership_generation` is an opaque digest of the governed policy
//!   revision at issue. The relay does not compare it; it re-evaluates the
//!   device's current room permission (cert, revocation, roster, role, caps)
//!   every observation, so an unrelated policy edit does not drop sessions.
//! * Permission observations share one owner turn per [`OBSERVATION_AGE`]
//!   across every live session, so live never contends with the owner more
//!   than a few times per 100 ms regardless of session count. An observation
//!   also pins the owner's continuity: a session admitted under one owner
//!   continuity is refused once the owner recovers into another.
//! * Forwarded signed changes are published by the host on behalf of the
//!   author into the author's own namespace `(room, author device)`, track
//!   `ops/<record digest>`, as their own groups. The frame carries the exact
//!   canonical delivery item bytes; receivers admit it like replay.
//! * Forward-before-commit: items are offered from inside the
//!   durable commit, after every check that can reject them and immediately
//!   before the store barrier (`owner::delivery::early_forward`), for inline
//!   and staged uploads alike. Record-less documents use the stable live
//!   record [`PACKAGE_PRIMARY_RECORD`]. A forwarded item whose barrier fails
//!   is not durable; receivers hold forwarded changes as speculative until
//!   durable replay confirms their digest (browser `peer_live` overlay).
use crate::owner_turn::{HostOwner, Observed, OwnerTurn};
use anyhow::{Context, Result, bail, ensure};
use collab_types::caps::{Actor, CapDecision, ResourceRef, Verb, decide_capability};
use ed25519_dalek::{SigningKey, VerifyingKey};
use rand::TryRng as _;
use std::{
    collections::{HashMap, VecDeque},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tilo_delivery_protocol::delivery_v3::{
    capabilities::PRODUCT_DATA_V1,
    http_proof::{Method, VerifiedRequest},
    live_wire::{
        GRANT_LIFETIME_NS, LIVE_GRANT_TARGET, LIVE_WEBTRANSPORT_PATH, LiveDirections,
        LiveGrantMetadata, LiveGrantRequest, LiveNamespace, LiveRecord, MAX_STREAM_FRAME_BYTES,
        OpsFrame, PresenceFrame, SignedLiveGrant, room_digest,
    },
};
use tilo_moq::{
    live::{LiveAuthority, LiveRelay, LiveSession},
    relay::{Admission, AuthorizationStatus},
};
use tilo_peer_state::StatePeer;
use tilo_user_identity::DeviceCert;
use web_time::SystemTime;

/// Live leases retained at once (one per participation).
const MAX_LEASES: usize = 256;
/// Admitted items waiting for forwarding; excess is dropped, never blocks.
const FORWARD_QUEUE: usize = 256;
/// Recently forwarded canonical digests remembered for de-duplication.
const RECENT_FORWARDS: usize = 512;
/// A shared permission observation is reused for at most this long, so the
/// relay's 100 ms per-session re-check sees state at most this much older.
const OBSERVATION_AGE: Duration = Duration::from_millis(50);

struct Lease {
    device: [u8; 32],
    expires_at_ns: u64,
}

/// One shared observation of current live permission for every leased device.
enum Snapshot {
    /// `None` is a definite current denial for that device.
    Current {
        continuity: u64,
        permissions: HashMap<[u8; 32], Option<LiveDirections>>,
    },
    Busy,
    Denied,
}
struct Observation {
    at: Instant,
    snapshot: Snapshot,
}

enum Permission {
    Current(u64),
    Busy,
    Denied,
}

struct Shared<O: HostOwner> {
    owner: O,
    /// Governed room authority; must stay current for any live permission.
    authority: [u8; 32],
    /// Pinned workspace host device key (the data session's proved host) that
    /// signs live grants. The Cloud host signs with its room authority.
    host: [u8; 32],
    host_boot: [u8; 32],
    audience: String,
    room: String,
    room_hex: String,
    leases: Mutex<HashMap<[u8; 32], Lease>>,
    observed: Mutex<Option<Observation>>,
}

/// Live grant authority over one host owner; cloning shares one lease registry
/// and boot.
pub struct LiveService<O: HostOwner>(Arc<Shared<O>>);
impl<O: HostOwner> Clone for LiveService<O> {
    fn clone(&self) -> Self {
        Self(Arc::clone(&self.0))
    }
}

/// Queue half of the admitted-change forwarder, installed as the delivery
/// admission sink.
#[derive(Clone)]
pub struct LiveForwarder(tokio::sync::mpsc::Sender<([u8; 32], Vec<u8>)>);
impl LiveForwarder {
    /// Never blocks the owner: a full queue drops the item (the durable path
    /// still delivers it).
    pub fn offer(&self, device: [u8; 32], canonical: &[u8]) {
        if !canonical.is_empty() && canonical.len() <= MAX_STREAM_FRAME_BYTES {
            let _ = self.0.try_send((device, canonical.to_vec()));
        }
    }
}

/// An installed live relay: service, forwarder and its task. Dropping it stops
/// forwarding; the relay itself stays owned by the transport.
pub struct InstalledLive<O: HostOwner> {
    pub service: LiveService<O>,
    pub forwarder: LiveForwarder,
    pub relay: LiveRelay,
    task: tokio::task::JoinHandle<()>,
}
impl<O: HostOwner> Drop for InstalledLive<O> {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// The host key that signs grants, with the certificate a data session must
/// have proved for it.
pub struct LiveSigner<'a> {
    pub key: &'a SigningKey,
    pub certificate: &'a DeviceCert,
}

fn at(ns: u64) -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_nanos(ns)
}

/// Current room permission for `device`: the governed room authority must
/// still be current, and each direction must be allowed. Returns the allowed
/// directions and the participant root (user) key.
fn permitted(
    peer: &StatePeer,
    authority: [u8; 32],
    device: [u8; 32],
    now: SystemTime,
) -> Option<(LiveDirections, [u8; 32])> {
    peer.permissions(authority, now).ok()?;
    let (role, caps, person) = peer.permissions(device, now).ok()?;
    let room = peer.room().to_owned();
    let actor = Actor::person(Some(role), &person).in_room(room.clone());
    let resource = ResourceRef::Room(room);
    let seconds = now.duration_since(SystemTime::UNIX_EPOCH).ok()?.as_secs();
    let allow = |verb| {
        decide_capability(&actor, &resource, verb, &caps, Some(seconds)) == CapDecision::Allow
    };
    let mut root = [0; 32];
    hex::decode_to_slice(&person, &mut root).ok()?;
    Some((
        LiveDirections {
            publish: allow(Verb::Write),
            subscribe: allow(Verb::Read),
        },
        root,
    ))
}

impl<O: HostOwner> LiveService<O> {
    /// Bind to `owner` under one of its current turns. `host` is the key that
    /// signs grants; `origin` is the installed HTTPS origin, to which the
    /// audience appends the live path.
    pub fn bind(owner: O, turn: &O::Turn<'_>, host: [u8; 32], origin: &str) -> Result<Self> {
        let mut host_boot = [0; 32];
        while host_boot == [0; 32] {
            rand::rngs::SysRng.try_fill_bytes(&mut host_boot)?;
        }
        let peer = turn.peer();
        let room = peer.room().to_owned();
        Ok(Self(Arc::new(Shared {
            owner,
            authority: peer.authority().device_public_key,
            host,
            host_boot,
            audience: format!("{origin}{LIVE_WEBTRANSPORT_PATH}"),
            room_hex: hex::encode(room_digest(&room)),
            room,
            leases: Mutex::default(),
            observed: Mutex::default(),
        })))
    }

    /// Relay, forwarder and task for this service. Must run inside Tokio.
    pub fn install(self) -> InstalledLive<O> {
        let relay = LiveRelay::new(Arc::new(self.clone()));
        let (forwarder, task) = forwarder(self.0.room_hex.clone(), relay.clone());
        InstalledLive {
            service: self,
            forwarder,
            relay,
            task,
        }
    }

    /// Issue or renew one grant for an already admitted request, inside one
    /// current owner turn.
    pub fn issue(
        &self,
        turn: &O::Turn<'_>,
        signer: LiveSigner<'_>,
        request: &VerifiedRequest<'_>,
    ) -> Result<Vec<u8>> {
        let shared = &self.0;
        ensure!(
            request.request().method() == Method::Post
                && request.request().target() == LIVE_GRANT_TARGET,
            "not the live grant route"
        );
        let session = request.session();
        ensure!(
            session.capabilities() & PRODUCT_DATA_V1 == PRODUCT_DATA_V1,
            "live grants require an authenticated data session"
        );
        let mut wanted = LiveGrantRequest::decode(request.request().body())
            .map_err(|error| anyhow::anyhow!("invalid live grant request: {error}"))?;
        wanted
            .validate()
            .map_err(|error| anyhow::anyhow!("invalid live grant request: {error}"))?;
        let device = session.device_certificate().device_public_key;
        ensure!(
            wanted.device == device,
            "live grant device differs from the data session device"
        );
        let peer = turn.peer();
        ensure!(
            wanted.room == shared.room && peer.room() == shared.room,
            "live grant room mismatch"
        );
        ensure!(
            signer.key.verifying_key().to_bytes() == shared.host
                && signer.certificate.device_public_key == shared.host
                && peer.authority().device_public_key == shared.authority
                && session.host_certificate() == signer.certificate,
            "live grant signer is not the pinned host"
        );
        let issued_at_ns = turn.fresh_now_ns().context("owner clock unavailable")?;
        let root = session.device_certificate().user_public_key;
        let (allowed, person) = permitted(peer, shared.authority, device, at(issued_at_ns))
            .context("live grant not permitted")?;
        ensure!(person == root, "live grant participant root mismatch");
        let revision =
            tilo_peer_state::owner_commit::governance_revision(peer).map_err(anyhow::Error::msg)?;
        let digest = blake3::hash(revision.as_bytes());
        let generation =
            u64::from_le_bytes(digest.as_bytes()[..8].try_into().expect("eight bytes"));
        wanted.directions = LiveDirections {
            publish: wanted.directions.publish && allowed.publish,
            subscribe: wanted.directions.subscribe && allowed.subscribe,
        };
        ensure!(
            wanted.directions.publish || wanted.directions.subscribe,
            "no permitted live direction"
        );
        let mut expires_at_ns = issued_at_ns
            .checked_add(GRANT_LIFETIME_NS)
            .context("grant expiry overflow")?;
        if let Some(end) = session.device_certificate().expires_at_ns {
            expires_at_ns = expires_at_ns.min(end);
        }
        ensure!(expires_at_ns > issued_at_ns, "device certificate expired");
        {
            let mut leases = shared.leases.lock().unwrap();
            leases.retain(|_, lease| lease.expires_at_ns > issued_at_ns);
            match leases.get_mut(&wanted.participation_id) {
                Some(lease) if lease.device != device => {
                    bail!("participation belongs to another device")
                }
                Some(lease) => lease.expires_at_ns = lease.expires_at_ns.max(expires_at_ns),
                None => {
                    ensure!(leases.len() < MAX_LEASES, "live lease capacity");
                    leases.insert(
                        wanted.participation_id,
                        Lease {
                            device,
                            expires_at_ns,
                        },
                    );
                }
            }
        }
        let metadata = LiveGrantMetadata {
            root,
            namespace: LiveNamespace::new(&wanted.room, device),
            request: wanted,
            membership_generation: generation,
            audience: shared.audience.clone(),
            host: shared.host,
            host_boot: shared.host_boot,
            issued_at_ns,
            expires_at_ns,
        };
        let grant = SignedLiveGrant::sign(metadata, signer.key)
            .map_err(|error| anyhow::anyhow!("live grant signing failed: {error}"))?;
        grant
            .encode()
            .map_err(|error| anyhow::anyhow!("live grant encoding failed: {error}"))
    }

    /// Whether the device of an already admitted request may drive the room's
    /// legacy windows: only a member live grants let publish (room write),
    /// never a viewer, so watching stays read-only for pixels too.
    ///
    /// # Errors
    ///
    /// Why the device may not.
    pub fn check_control(&self, turn: &O::Turn<'_>, request: &VerifiedRequest<'_>) -> Result<()> {
        let session = request.session();
        ensure!(
            session.capabilities() & PRODUCT_DATA_V1 == PRODUCT_DATA_V1,
            "input requires an authenticated data session"
        );
        let device = session.device_certificate().device_public_key;
        let now = turn.fresh_now_ns().context("owner clock unavailable")?;
        let (allowed, person) = permitted(turn.peer(), self.0.authority, device, at(now))
            .context("input not permitted")?;
        ensure!(
            person == session.device_certificate().user_public_key,
            "input participant root mismatch"
        );
        ensure!(allowed.publish, "viewers may not drive legacy windows");
        Ok(())
    }

    fn now_ns(&self) -> Option<u64> {
        self.0.owner.now_ns()
    }

    fn lease_deadline(&self, participation: &[u8; 32], device: [u8; 32]) -> Option<u64> {
        let leases = self.0.leases.lock().unwrap();
        leases
            .get(participation)
            .filter(|lease| lease.device == device)
            .map(|lease| lease.expires_at_ns)
    }

    /// Monotonic instant for an absolute deadline on the owner clock (never
    /// extends it).
    fn instant(&self, deadline_ns: u64) -> Instant {
        let now = self.now_ns().unwrap_or(deadline_ns);
        Instant::now() + Duration::from_nanos(deadline_ns.saturating_sub(now))
    }

    /// One owner turn observing every currently leased device.
    fn refresh(&self) -> Snapshot {
        let shared = &self.0;
        let devices: Vec<[u8; 32]> = {
            let now = self.now_ns().unwrap_or(u64::MAX);
            let leases = shared.leases.lock().unwrap();
            let mut devices: Vec<_> = leases
                .values()
                .filter(|lease| lease.expires_at_ns > now)
                .map(|lease| lease.device)
                .collect();
            devices.sort_unstable();
            devices.dedup();
            devices
        };
        let observed = shared.owner.with_current(|turn| {
            let peer = turn.peer();
            if peer.room() != shared.room || peer.authority().device_public_key != shared.authority
            {
                return None;
            }
            let now = at(turn.now_ns());
            let permissions = devices
                .iter()
                .map(|device| {
                    (
                        *device,
                        permitted(peer, shared.authority, *device, now).map(|(allowed, _)| allowed),
                    )
                })
                .collect();
            Some((turn.continuity(), permissions))
        });
        match observed {
            Observed::Current(Some((continuity, permissions))) => Snapshot::Current {
                continuity,
                permissions,
            },
            Observed::Current(None) | Observed::Denied => Snapshot::Denied,
            Observed::Busy => Snapshot::Busy,
        }
    }

    /// Current permission of `device` for the granted directions, from a
    /// shared observation no older than [`OBSERVATION_AGE`].
    fn observe(&self, device: [u8; 32], directions: LiveDirections) -> Permission {
        let mut observed = self.0.observed.lock().unwrap();
        let usable = observed.as_ref().is_some_and(|observed| {
            observed.at.elapsed() < OBSERVATION_AGE
                && match &observed.snapshot {
                    Snapshot::Current { permissions, .. } => permissions.contains_key(&device),
                    Snapshot::Busy | Snapshot::Denied => true,
                }
        });
        if !usable {
            *observed = Some(Observation {
                at: Instant::now(),
                snapshot: self.refresh(),
            });
        }
        match &observed.as_ref().expect("observed").snapshot {
            Snapshot::Current {
                continuity,
                permissions,
            } => match permissions.get(&device) {
                Some(Some(allowed))
                    if (!directions.publish || allowed.publish)
                        && (!directions.subscribe || allowed.subscribe) =>
                {
                    Permission::Current(*continuity)
                }
                _ => Permission::Denied,
            },
            Snapshot::Busy => Permission::Busy,
            Snapshot::Denied => Permission::Denied,
        }
    }
}

fn forwarder(room: String, relay: LiveRelay) -> (LiveForwarder, tokio::task::JoinHandle<()>) {
    let (sender, mut receiver) = tokio::sync::mpsc::channel::<([u8; 32], Vec<u8>)>(FORWARD_QUEUE);
    let task = tokio::spawn(async move {
        let mut maintenance = tokio::time::interval(Duration::from_secs(1));
        // A lost-reply probe re-reports the original acceptance; forward each
        // canonical item once (receivers also de-duplicate by digest).
        let mut recent = VecDeque::with_capacity(RECENT_FORWARDS);
        loop {
            tokio::select! {
                item = receiver.recv() => {
                    let Some((device, canonical)) = item else { break };
                    let digest = *blake3::hash(&canonical).as_bytes();
                    if recent.contains(&digest) {
                        continue;
                    }
                    if recent.len() == RECENT_FORWARDS {
                        recent.pop_front();
                    }
                    recent.push_back(digest);
                    if let Some((record, frame)) = signed_change(&canonical) {
                        relay.inject_ops(&room, &hex::encode(device), &hex::encode(record.digest()), &frame);
                    }
                }
                _ = maintenance.tick() => relay.maintain(),
            }
        }
    });
    (LiveForwarder(sender), task)
}

/// Live record id of an application's record-less (package-primary) document,
/// such as Chat's `("so.tilo.chat", "")`. Identical to the UI's
/// `ui::record::RecordId::package_primary()`, the id the desktop already uses
/// for that document, and never a valid durable record id (durable ids are
/// `file:`, `uuid:`, `tab:` or `derived:`), so the mapping is injective.
pub const PACKAGE_PRIMARY_RECORD: &str = "@tilo:package-primary";

/// Stable live record for a durable `(app_id, record_id)`, within the frozen
/// contract's 1..=128 byte bounds: an empty record id maps to
/// [`PACKAGE_PRIMARY_RECORD`]; a durable id spelling that sentinel, or out of
/// bounds, has no live record (it still arrives by durable replay).
pub fn live_record(app_id: String, record_id: String) -> Option<LiveRecord> {
    let record = match record_id.as_str() {
        "" => PACKAGE_PRIMARY_RECORD.to_owned(),
        PACKAGE_PRIMARY_RECORD => return None,
        _ => record_id,
    };
    (!app_id.is_empty() && app_id.len() <= 128 && record.len() <= 128).then_some(LiveRecord {
        app: app_id,
        record,
    })
}

/// Bytes of an admitted `AppDocument` item as a signed-change ops frame.
/// Items whose frame would exceed `MAX_STREAM_FRAME_BYTES` are not forwarded.
pub fn signed_change(canonical: &[u8]) -> Option<(LiveRecord, Vec<u8>)> {
    let tilo_peer_state::NeighbourMsg::AppDocument {
        app_id, record_id, ..
    } = postcard::from_bytes(canonical).ok()?
    else {
        return None;
    };
    let record = live_record(app_id, record_id)?;
    let frame = OpsFrame::SignedChange {
        record: record.clone(),
        digest: *blake3::hash(canonical).as_bytes(),
        canonical: canonical.to_vec(),
    }
    .encode()
    .ok()?;
    Some((record, frame))
}

struct Permit<O: HostOwner> {
    service: LiveService<O>,
    device: [u8; 32],
    participation: [u8; 32],
    directions: LiveDirections,
    continuity: u64,
    room: String,
    publisher: String,
}
impl<O: HostOwner> LiveSession for Permit<O> {
    fn room(&self) -> &str {
        &self.room
    }
    fn publisher(&self) -> &str {
        &self.publisher
    }
    fn publish(&self) -> bool {
        self.directions.publish
    }
    fn subscribe(&self) -> bool {
        self.directions.subscribe
    }
    fn status(&self) -> AuthorizationStatus {
        // The lease is the latest renewal for this participation.
        let Some(deadline) = self
            .service
            .lease_deadline(&self.participation, self.device)
        else {
            return AuthorizationStatus::Denied;
        };
        if self.service.now_ns().is_none_or(|now| now >= deadline) {
            return AuthorizationStatus::Denied;
        }
        match self.service.observe(self.device, self.directions) {
            Permission::Current(continuity) if continuity == self.continuity => {
                AuthorizationStatus::Current
            }
            Permission::Busy => AuthorizationStatus::Unavailable {
                expires_at: self.service.instant(deadline),
            },
            Permission::Current(_) | Permission::Denied => AuthorizationStatus::Denied,
        }
    }
}

impl<O: HostOwner> LiveAuthority for LiveService<O> {
    fn try_admit(&self, token: &[u8]) -> std::result::Result<Arc<dyn LiveSession>, Admission> {
        let shared = &self.0;
        let grant = SignedLiveGrant::decode(token).map_err(|_| Admission::Denied)?;
        let host = VerifyingKey::from_bytes(&shared.host).map_err(|_| Admission::Denied)?;
        let now = self.now_ns().ok_or(Admission::Denied)?;
        grant.verify(&host, now).map_err(|_| Admission::Denied)?;
        let m = &grant.metadata;
        let device = m.request.device;
        if m.audience != shared.audience
            || m.host_boot != shared.host_boot
            || m.request.room != shared.room
            || m.namespace != LiveNamespace::new(&shared.room, device)
        {
            return Err(Admission::Denied);
        }
        let deadline = self
            .lease_deadline(&m.request.participation_id, device)
            .ok_or(Admission::Denied)?;
        if now >= deadline {
            return Err(Admission::Denied);
        }
        let continuity = match self.observe(device, m.request.directions) {
            Permission::Current(continuity) => continuity,
            Permission::Busy => return Err(Admission::Busy),
            Permission::Denied => return Err(Admission::Denied),
        };
        Ok(Arc::new(Permit {
            service: self.clone(),
            device,
            participation: m.request.participation_id,
            directions: m.request.directions,
            continuity,
            room: shared.room_hex.clone(),
            publisher: hex::encode(device),
        }))
    }
    fn presence_seq(&self, frame: &[u8]) -> Option<u64> {
        PresenceFrame::decode(frame).ok().map(|frame| frame.seq)
    }
    fn ops_valid(&self, record: &str, frame: &[u8]) -> bool {
        let Ok(frame) = OpsFrame::decode(frame) else {
            return false;
        };
        let named = match &frame {
            OpsFrame::Preview { record, .. } | OpsFrame::SignedChange { record, .. } => record,
        };
        hex::encode(named.digest()) == record
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_less_documents_map_to_the_stable_package_primary_live_record() {
        let chat = live_record("so.tilo.chat".into(), String::new()).unwrap();
        assert_eq!(chat.record, PACKAGE_PRIMARY_RECORD);
        assert_eq!(chat.app, "so.tilo.chat");
        let named = live_record("so.tilo.notes".into(), "tab:0000000000000001".into()).unwrap();
        assert_eq!(named.record, "tab:0000000000000001");
        assert_ne!(chat.digest(), named.digest());
        // Injective and inside the frozen 1..=128 byte bounds.
        assert!(live_record("so.tilo.chat".into(), PACKAGE_PRIMARY_RECORD.into()).is_none());
        assert!(live_record("a".into(), "r".repeat(129)).is_none());
        assert!(live_record(String::new(), "r".into()).is_none());
        assert!(live_record("a".repeat(128), "r".repeat(128)).is_some());
    }

    #[test]
    fn relay_bounds_match_the_frozen_wire_contract() {
        use tilo_delivery_protocol::delivery_v3::live_wire as wire;
        use tilo_moq::live;
        assert_eq!(live::LIVE_PATH, wire::LIVE_WEBTRANSPORT_PATH);
        assert_eq!(live::LIVE_PROTOCOL, wire::LIVE_PROTOCOL);
        assert_eq!(live::NAMESPACE_ROOT, wire::LIVE_NAMESPACE_ROOT);
        assert_eq!(live::MAX_TOKEN_BYTES, wire::MAX_GRANT_BYTES);
        assert_eq!(live::MAX_STREAM_FRAME_BYTES, wire::MAX_STREAM_FRAME_BYTES);
    }
}
