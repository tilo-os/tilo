//! LIVE_V1 on the Cloud host: the same contract as the native live service
//! (`qos-session-peer/src/live_service.rs`), with the actual Cloud Store as the
//! only authority.
//!
//! * `POST /peer/v3/live/grant` is admitted inside a reserved Store turn with
//!   the original request proof. The grant is signed by the pinned workspace
//!   host (the Cloud Store authority key), `root` is the device certificate's
//!   user key, lifetime <= 60 s (capped by the device certificate), `host_boot`
//!   is random per process, and directions are the authorized subset of the
//!   request (viewers: subscribe only; none permitted refuses).
//! * `/moq/v21/live` is served by the shared `qos-moq` live relay on the Cloud
//!   host's QUIC endpoint. Admission verifies signature, lifetime, audience,
//!   host boot, room and namespace, then the current lease and roster
//!   permission. Every relay observation (100 ms) re-checks the lease and
//!   current permission; a refused observation aborts only that publisher.
//! * Permission observations share one Store turn per [`OBSERVATION_AGE`]
//!   across all live sessions, so live never contends with the Store owner
//!   more than a few times per 100 ms regardless of session count.
//! * Newly accepted state items (inline and staged) are offered
//!   (non-blocking) to a forwarder task that publishes `OpsFrame::SignedChange`
//!   with the exact canonical bytes on the author's namespace, host-fed if the
//!   author has no live session. Lane C: the offer happens inside the Store
//!   commit after every admission check and immediately before the barrier
//!   (`crate::delivery::early_forward`), so forwarding runs in parallel with
//!   durability. Record-less documents use [`PACKAGE_PRIMARY_RECORD`].
use super::{CloudStoreSource, CloudStoreTurn, Current};
use anyhow::{Context, Result, bail, ensure};
use collab_types::caps::{Actor, CapDecision, ResourceRef, Verb, decide_capability};
use ed25519_dalek::VerifyingKey;
use qos_moq::{
    live::{LiveAuthority, LiveRelay, LiveSession},
    relay::{Admission, AuthorizationStatus},
};
use qos_relay::delivery_v3::{
    capabilities::PRODUCT_DATA_V1,
    http_proof::{Method, VerifiedRequest},
    live_wire::{
        GRANT_LIFETIME_NS, LIVE_GRANT_TARGET, LIVE_WEBTRANSPORT_PATH, LiveDirections,
        LiveGrantMetadata, LiveGrantRequest, LiveNamespace, LiveRecord, MAX_STREAM_FRAME_BYTES,
        OpsFrame, PresenceFrame, SignedLiveGrant, room_digest,
    },
};
use rand::RngCore;
use std::{
    collections::{HashMap, VecDeque},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
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
    Current(HashMap<[u8; 32], Option<LiveDirections>>),
    Busy,
    Denied,
}
struct Observed {
    at: Instant,
    snapshot: Snapshot,
}

enum Observation {
    Current,
    Busy,
    Denied,
}

struct Shared {
    source: CloudStoreSource,
    /// Pinned workspace host device key (the Cloud Store authority) that signs
    /// live grants; also the governed room authority that must stay current.
    host: [u8; 32],
    host_boot: [u8; 32],
    audience: String,
    room: String,
    room_hex: String,
    leases: Mutex<HashMap<[u8; 32], Lease>>,
    observed: Mutex<Option<Observed>>,
}

/// Cloud live authority; cloning shares one lease registry and boot.
#[derive(Clone)]
pub(crate) struct CloudLiveService(Arc<Shared>);

/// Queue half of the admitted-change forwarder.
#[derive(Clone)]
pub(crate) struct CloudLiveForwarder(tokio::sync::mpsc::Sender<([u8; 32], Vec<u8>)>);
impl CloudLiveForwarder {
    /// Never blocks the Store owner: a full queue drops the item (the durable
    /// path still delivers it).
    pub(crate) fn offer(&self, device: [u8; 32], canonical: &[u8]) {
        if !canonical.is_empty() && canonical.len() <= MAX_STREAM_FRAME_BYTES {
            let _ = self.0.try_send((device, canonical.to_vec()));
        }
    }
}

/// An installed Cloud live relay: service, forwarder and its task. Dropping
/// it stops forwarding; the relay itself stays owned by the transport.
pub(crate) struct InstalledLive {
    pub(crate) service: CloudLiveService,
    pub(crate) forwarder: CloudLiveForwarder,
    pub(crate) relay: LiveRelay,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for InstalledLive {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn at(ns: u64) -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_nanos(ns)
}

/// Current room permission for `device`: the governed room authority must
/// still be current, and each direction must be allowed. Returns the allowed
/// directions and the participant root (user) key.
fn permitted(
    peer: &crate::StatePeer,
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

/// Live record id of an application's record-less (package-primary)
/// document, e.g. Chat's `("dev.qos.chat", "")`; identical to the native host
/// and to the UI's `RecordId::package_primary()`, and never a valid durable
/// record id, so the mapping is injective.
pub(crate) const PACKAGE_PRIMARY_RECORD: &str = "@qos:package-primary";

/// Stable live record for a durable `(app_id, record_id)` within the frozen
/// contract's 1..=128 byte bounds (see the native `live_service::live_record`).
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

/// Bytes of an admitted `AppDocument` item as a signed-change ops frame;
/// items whose frame would exceed `MAX_STREAM_FRAME_BYTES` are not forwarded.
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

impl CloudLiveService {
    /// Bind to the actual source under one current turn. `origin` is the
    /// installed HTTPS origin; the audience appends the live path.
    pub(crate) fn bind(
        source: CloudStoreSource,
        turn: &CloudStoreTurn,
        origin: &str,
    ) -> Result<Self> {
        ensure!(
            source.owns_turn(turn),
            "live authority needs this source's turn"
        );
        let host = turn.peer().authority().device_public_key;
        ensure!(
            turn.store.key().verifying_key().to_bytes() == host,
            "live signer is not the Store authority"
        );
        let mut host_boot = [0; 32];
        while host_boot == [0; 32] {
            rand::rngs::OsRng.try_fill_bytes(&mut host_boot)?;
        }
        let room = turn.peer().room().to_owned();
        Ok(Self(Arc::new(Shared {
            source,
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
    pub(crate) fn install(self) -> InstalledLive {
        let relay = LiveRelay::new(Arc::new(self.clone()));
        let (forwarder, task) = self.forwarder(relay.clone());
        InstalledLive {
            service: self,
            forwarder,
            relay,
            task,
        }
    }

    fn forwarder(&self, relay: LiveRelay) -> (CloudLiveForwarder, tokio::task::JoinHandle<()>) {
        let (sender, mut receiver) =
            tokio::sync::mpsc::channel::<([u8; 32], Vec<u8>)>(FORWARD_QUEUE);
        let room = self.0.room_hex.clone();
        let task = tokio::spawn(async move {
            let mut maintenance = tokio::time::interval(Duration::from_secs(1));
            // A lost-reply probe re-reports the original acceptance; forward
            // each canonical item once (receivers also de-duplicate by digest).
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
        (CloudLiveForwarder(sender), task)
    }

    /// Issue or renew one grant for an already admitted request inside this
    /// exact reserved Store turn.
    pub(crate) fn issue_in(
        &self,
        turn: &CloudStoreTurn,
        request: &VerifiedRequest<'_>,
    ) -> Result<Vec<u8>> {
        let shared = &self.0;
        ensure!(shared.source.owns_turn(turn), "foreign Store turn");
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
        let key = turn.store.key();
        ensure!(
            key.verifying_key().to_bytes() == shared.host
                && peer.authority().device_public_key == shared.host
                && session.host_certificate() == peer.authority(),
            "live grant signer is not the pinned host"
        );
        let issued_at_ns = turn.fresh_now_ns()?;
        let root = session.device_certificate().user_public_key;
        let (allowed, person) = permitted(peer, shared.host, device, at(issued_at_ns))
            .context("live grant not permitted")?;
        ensure!(person == root, "live grant participant root mismatch");
        let revision =
            qos_peer_state::owner_commit::governance_revision(peer).map_err(anyhow::Error::msg)?;
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
        let grant = SignedLiveGrant::sign(metadata, &key)
            .map_err(|error| anyhow::anyhow!("live grant signing failed: {error}"))?;
        grant
            .encode()
            .map_err(|error| anyhow::anyhow!("live grant encoding failed: {error}"))
    }

    fn now_ns(&self) -> Option<u64> {
        self.0.source.policy_now_ns().ok()
    }

    fn lease_deadline(&self, participation: &[u8; 32], device: [u8; 32]) -> Option<u64> {
        let leases = self.0.leases.lock().unwrap();
        leases
            .get(participation)
            .filter(|lease| lease.device == device)
            .map(|lease| lease.expires_at_ns)
    }

    /// Monotonic instant for an absolute deadline on the source clock.
    fn instant(&self, deadline_ns: u64) -> Instant {
        let now = self.now_ns().unwrap_or(deadline_ns);
        Instant::now() + Duration::from_nanos(deadline_ns.saturating_sub(now))
    }

    /// One Store turn observing every currently leased device.
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
        let observed = shared.source.with_current(|turn| {
            let peer = turn.peer();
            if peer.room() != shared.room || peer.authority().device_public_key != shared.host {
                return None;
            }
            let now = at(turn.now_ns());
            Some(
                devices
                    .iter()
                    .map(|device| {
                        (
                            *device,
                            permitted(peer, shared.host, *device, now).map(|(allowed, _)| allowed),
                        )
                    })
                    .collect(),
            )
        });
        match observed {
            Current::Current(Some(map)) => Snapshot::Current(map),
            Current::Current(None) | Current::Denied => Snapshot::Denied,
            Current::Busy => Snapshot::Busy,
        }
    }

    /// Current permission of `device` for the granted directions, from a
    /// shared observation no older than [`OBSERVATION_AGE`].
    fn observe(&self, device: [u8; 32], directions: LiveDirections) -> Observation {
        let mut observed = self.0.observed.lock().unwrap();
        let usable = observed.as_ref().is_some_and(|observed| {
            observed.at.elapsed() < OBSERVATION_AGE
                && match &observed.snapshot {
                    Snapshot::Current(map) => map.contains_key(&device),
                    Snapshot::Busy | Snapshot::Denied => true,
                }
        });
        if !usable {
            *observed = Some(Observed {
                at: Instant::now(),
                snapshot: self.refresh(),
            });
        }
        match &observed.as_ref().expect("observed").snapshot {
            Snapshot::Current(map) => match map.get(&device) {
                Some(Some(allowed))
                    if (!directions.publish || allowed.publish)
                        && (!directions.subscribe || allowed.subscribe) =>
                {
                    Observation::Current
                }
                _ => Observation::Denied,
            },
            Snapshot::Busy => Observation::Busy,
            Snapshot::Denied => Observation::Denied,
        }
    }
}

struct Permit {
    service: CloudLiveService,
    device: [u8; 32],
    participation: [u8; 32],
    directions: LiveDirections,
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
        if self.service.now_ns().is_none_or(|now| now >= deadline) {
            return AuthorizationStatus::Denied;
        }
        match self.service.observe(self.device, self.directions) {
            Observation::Current => AuthorizationStatus::Current,
            Observation::Busy => AuthorizationStatus::Unavailable {
                expires_at: self.service.instant(deadline),
            },
            Observation::Denied => AuthorizationStatus::Denied,
        }
    }
}

impl LiveAuthority for CloudLiveService {
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
        match self.observe(device, m.request.directions) {
            Observation::Current => {}
            Observation::Busy => return Err(Admission::Busy),
            Observation::Denied => return Err(Admission::Denied),
        }
        Ok(Arc::new(Permit {
            service: self.clone(),
            device,
            participation: m.request.participation_id,
            directions: m.request.directions,
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
        let chat = live_record("dev.qos.chat".into(), String::new()).unwrap();
        assert_eq!(chat.record, PACKAGE_PRIMARY_RECORD);
        assert_eq!(chat.app, "dev.qos.chat");
        let named = live_record("dev.qos.notes".into(), "tab:0000000000000001".into()).unwrap();
        assert_eq!(named.record, "tab:0000000000000001");
        assert_ne!(chat.digest(), named.digest());
        // Injective and inside the frozen 1..=128 byte bounds.
        assert!(live_record("dev.qos.chat".into(), PACKAGE_PRIMARY_RECORD.into()).is_none());
        assert!(live_record("a".into(), "r".repeat(129)).is_none());
        assert!(live_record(String::new(), "r".into()).is_none());
        assert!(live_record("a".repeat(128), "r".repeat(128)).is_some());
    }

    #[test]
    fn only_record_scoped_app_documents_become_signed_change_frames() {
        assert!(signed_change(b"not a message").is_none());
        let message = qos_peer_state::NeighbourMsg::SyncReq {
            since: vec![],
            room: "room".into(),
            invite_id: None,
        };
        assert!(signed_change(&postcard::to_stdvec(&message).unwrap()).is_none());
    }

    #[test]
    fn relay_bounds_match_the_frozen_wire_contract() {
        use qos_moq::live;
        use qos_relay::delivery_v3::live_wire as wire;
        assert_eq!(live::LIVE_PATH, wire::LIVE_WEBTRANSPORT_PATH);
        assert_eq!(live::LIVE_PROTOCOL, wire::LIVE_PROTOCOL);
        assert_eq!(live::NAMESPACE_ROOT, wire::LIVE_NAMESPACE_ROOT);
        assert_eq!(live::MAX_TOKEN_BYTES, wire::MAX_GRANT_BYTES);
        assert_eq!(live::MAX_STREAM_FRAME_BYTES, wire::MAX_STREAM_FRAME_BYTES);
    }
}
