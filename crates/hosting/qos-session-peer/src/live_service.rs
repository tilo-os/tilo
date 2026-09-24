//! LIVE_V1 host authority: issues host-signed live grants on the proof-required
//! `POST /peer/v3/live/grant` route, admits them as the MoQ Draft 21
//! Authorization Token on `/moq/v21/live`, rechecks current roster permission
//! on every relay observation, validates live frames with the canonical wire
//! codec, and forwards admitted signed changes to `ops/<record>` tracks.
//!
//! Decisions (see `docs/superpowers/specs/2026-09-23-live-v1-design.md`):
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
//! * Forwarded signed changes are published by the host on behalf of the
//!   author into the author's own namespace `(room, author device)`, track
//!   `ops/<record digest>`, as their own groups. The frame carries the exact
//!   canonical delivery item bytes; receivers admit it like replay.
//! * Lane C (forward-before-commit): items are offered from inside the
//!   durable commit, after every check that can reject them and immediately
//!   before the store barrier (`owner::delivery::early_forward`), for inline
//!   and staged uploads alike. Record-less documents use the stable live
//!   record [`PACKAGE_PRIMARY_RECORD`]. A forwarded item whose barrier fails
//!   is not durable; receivers hold forwarded changes as speculative until
//!   durable replay confirms their digest (browser `peer_live` overlay).
use crate::{
    SessionOwner,
    committed_authority::{CommittedAuthority, Observation},
};
use anyhow::{Context, Result, bail, ensure};
use collab_types::caps::{Actor, CapDecision, ResourceRef, Verb, decide_capability};
use ed25519_dalek::VerifyingKey;
use qos_moq::{
    live::{LiveAuthority, LiveRelay, LiveSession},
    relay::{Admission, AuthorizationStatus},
};
use qos_peer_state::StatePeer;
use rand::RngCore;
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tilo_relay::delivery_v3::{
    capabilities::PRODUCT_DATA_V1,
    http_proof::VerifiedRequest,
    live_wire::{
        GRANT_LIFETIME_NS, LIVE_GRANT_TARGET, LIVE_WEBTRANSPORT_PATH, LiveDirections,
        LiveGrantMetadata, LiveGrantRequest, LiveNamespace, LiveRecord, OpsFrame, PresenceFrame,
        SignedLiveGrant, room_digest,
    },
};
use web_time::SystemTime;

/// Live leases retained at once (one per participation).
const MAX_LEASES: usize = 256;
/// Admitted items waiting for forwarding; excess is dropped, never blocks.
const FORWARD_QUEUE: usize = 256;
/// Recently forwarded canonical digests remembered for de-duplication.
const RECENT_FORWARDS: usize = 512;

struct Lease {
    device: [u8; 32],
    expires_at_ns: u64,
}

struct Shared {
    committed: CommittedAuthority,
    /// Governed room authority; must stay current for any live permission.
    authority: [u8; 32],
    /// Pinned workspace host device key (the data session's proved host) that
    /// signs live grants.
    host: [u8; 32],
    host_boot: [u8; 32],
    audience: String,
    room: String,
    room_hex: String,
    leases: Mutex<HashMap<[u8; 32], Lease>>,
}

/// Host-private live authority; cloning shares one lease registry and boot.
#[derive(Clone)]
pub(crate) struct LiveService(Arc<Shared>);

/// Queue half of the admitted-change forwarder, installed on the data routes.
#[derive(Clone)]
pub(crate) struct LiveForwarder(tokio::sync::mpsc::Sender<([u8; 32], Vec<u8>)>);
impl LiveForwarder {
    /// Never blocks the owner loop: a full queue drops the item (the durable
    /// path still delivers it).
    pub(crate) fn offer(&self, device: [u8; 32], canonical: &[u8]) {
        if canonical.len() <= tilo_relay::delivery_v3::live_wire::MAX_STREAM_FRAME_BYTES {
            let _ = self.0.try_send((device, canonical.to_vec()));
        }
    }
}

fn now_ns() -> u64 {
    tilo_relay::clock::now_ns()
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

impl LiveService {
    /// `origin` is the installed HTTPS origin; the audience appends the live path.
    pub(crate) fn for_owner(
        owner: &SessionOwner,
        committed: CommittedAuthority,
        origin: &str,
    ) -> Result<Self> {
        ensure!(owner.permits_publication(), "live authority unavailable");
        let mut host_boot = [0; 32];
        while host_boot == [0; 32] {
            rand::rngs::OsRng.try_fill_bytes(&mut host_boot)?;
        }
        let room = owner.peer().room().to_owned();
        Ok(Self(Arc::new(Shared {
            committed,
            authority: owner.peer().authority().device_public_key,
            host: owner.host_certificate().device_public_key,
            host_boot,
            audience: format!("{origin}{LIVE_WEBTRANSPORT_PATH}"),
            room_hex: hex::encode(room_digest(&room)),
            room,
            leases: Mutex::default(),
        })))
    }

    /// Start the forwarder task that turns admitted canonical items into
    /// `OpsFrame::SignedChange` frames on the relay. Must run inside Tokio.
    pub(crate) fn forwarder(
        &self,
        relay: LiveRelay,
    ) -> (LiveForwarder, tokio::task::JoinHandle<()>) {
        let (sender, mut receiver) =
            tokio::sync::mpsc::channel::<([u8; 32], Vec<u8>)>(FORWARD_QUEUE);
        let room = self.0.room_hex.clone();
        let task = tokio::spawn(async move {
            let mut maintenance = tokio::time::interval(Duration::from_secs(1));
            // A lost-reply probe re-reports the original acceptance; forward
            // each canonical item once (receivers also de-duplicate by digest).
            let mut recent = std::collections::VecDeque::with_capacity(RECENT_FORWARDS);
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

    /// Serve one verified `POST /peer/v3/live/grant` on the serialized owner.
    pub(crate) fn issue(
        &self,
        owner: &SessionOwner,
        request: &VerifiedRequest<'_>,
    ) -> Result<Vec<u8>> {
        let shared = &self.0;
        ensure!(
            request.request().method() == tilo_relay::delivery_v3::http_proof::Method::Post
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
        ensure!(
            wanted.room == shared.room && owner.peer().room() == shared.room,
            "live grant room mismatch"
        );
        ensure!(
            owner.identity.host.verifying_key().to_bytes() == shared.host
                && session.host_certificate() == owner.host_certificate(),
            "live grant signer is not the pinned host"
        );
        let issued_at_ns = now_ns();
        let root = session.device_certificate().user_public_key;
        let (allowed, generation) = {
            let guard = shared.committed.read().map_err(anyhow::Error::msg)?;
            let peer = guard.peer();
            let (allowed, person) = permitted(peer, shared.authority, device, at(issued_at_ns))
                .context("live grant not permitted")?;
            ensure!(person == root, "live grant participant root mismatch");
            let revision = qos_peer_state::owner_commit::governance_revision(peer)
                .map_err(anyhow::Error::msg)?;
            let digest = blake3::hash(revision.as_bytes());
            (
                allowed,
                u64::from_le_bytes(digest.as_bytes()[..8].try_into().expect("eight bytes")),
            )
        };
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
        let grant = SignedLiveGrant::sign(metadata, &owner.identity.host)
            .map_err(|error| anyhow::anyhow!("live grant signing failed: {error}"))?;
        grant
            .encode()
            .map_err(|error| anyhow::anyhow!("live grant encoding failed: {error}"))
    }

    fn lease_deadline(&self, participation: &[u8; 32], device: [u8; 32]) -> Option<u64> {
        let leases = self.0.leases.lock().unwrap();
        leases
            .get(participation)
            .filter(|lease| lease.device == device)
            .map(|lease| lease.expires_at_ns)
    }

    /// Monotonic instant for an absolute wall deadline (never extends it).
    fn instant(&self, deadline_ns: u64) -> Instant {
        let now = now_ns();
        Instant::now() + Duration::from_nanos(deadline_ns.saturating_sub(now))
    }

    /// One committed observation for `device` and the granted directions.
    fn observe(
        &self,
        device: [u8; 32],
        directions: LiveDirections,
        continuity: Option<u64>,
    ) -> Observed {
        let shared = &self.0;
        match shared.committed.observe() {
            Observation::Current(current) => {
                if continuity.is_some_and(|value| value != current.continuity()) {
                    return Observed::Denied;
                }
                let peer = current.peer();
                if peer.room() != shared.room
                    || peer.authority().device_public_key != shared.authority
                {
                    return Observed::Denied;
                }
                match permitted(peer, shared.authority, device, at(now_ns())) {
                    Some((allowed, _))
                        if (!directions.publish || allowed.publish)
                            && (!directions.subscribe || allowed.subscribe) =>
                    {
                        Observed::Current(current.continuity())
                    }
                    _ => Observed::Denied,
                }
            }
            Observation::Updating => Observed::Busy,
            Observation::Denied => Observed::Denied,
        }
    }
}

enum Observed {
    Current(u64),
    Busy,
    Denied,
}

/// Live record id of an application's record-less (package-primary) document,
/// such as Chat's `("dev.qos.chat", "")`. Identical to the UI's
/// `ui::record::RecordId::package_primary()`, the id the desktop already uses
/// for that document, and never a valid durable record id (durable ids are
/// `file:`, `uuid:`, `tab:` or `derived:`), so the mapping is injective.
pub(crate) const PACKAGE_PRIMARY_RECORD: &str = "@qos:package-primary";

/// Stable live record for a durable `(app_id, record_id)`, within the frozen
/// contract's 1..=128 byte bounds: an empty record id maps to
/// [`PACKAGE_PRIMARY_RECORD`]; a durable id spelling that sentinel, or out of
/// bounds, has no live record (it still arrives by durable replay).
pub(crate) fn live_record(app_id: String, record_id: String) -> Option<LiveRecord> {
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
pub(crate) fn signed_change(canonical: &[u8]) -> Option<(LiveRecord, Vec<u8>)> {
    let qos_peer_state::NeighbourMsg::AppDocument {
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

struct Permit {
    service: LiveService,
    device: [u8; 32],
    participation: [u8; 32],
    directions: LiveDirections,
    continuity: u64,
    room: String,
    publisher: String,
}
impl LiveSession for Permit {
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
        if now_ns() >= deadline {
            return AuthorizationStatus::Denied;
        }
        match self
            .service
            .observe(self.device, self.directions, Some(self.continuity))
        {
            Observed::Current(_) => AuthorizationStatus::Current,
            Observed::Busy => AuthorizationStatus::Unavailable {
                expires_at: self.service.instant(deadline),
            },
            Observed::Denied => AuthorizationStatus::Denied,
        }
    }
}

impl LiveAuthority for LiveService {
    fn try_admit(&self, token: &[u8]) -> std::result::Result<Arc<dyn LiveSession>, Admission> {
        let shared = &self.0;
        let grant = SignedLiveGrant::decode(token).map_err(|_| Admission::Denied)?;
        let host = VerifyingKey::from_bytes(&shared.host).map_err(|_| Admission::Denied)?;
        let now = now_ns();
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
        let continuity = match self.observe(device, m.request.directions, None) {
            Observed::Current(continuity) => continuity,
            Observed::Busy => return Err(Admission::Busy),
            Observed::Denied => return Err(Admission::Denied),
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
