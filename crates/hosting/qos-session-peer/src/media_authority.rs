//! Native, ephemeral media authority. Transport adapters must use a current
//! committed StatePeer and the native call-policy owner for every operation.

use collab_types::caps::{Actor, CapDecision, ResourceRef, Verb, decide_capability};
use ed25519_dalek::{Signature, VerifyingKey};
use qos_peer_state::StatePeer;
use rand::RngCore;
use serde::Serialize;
use std::collections::BTreeMap;
use tilo_user_identity::canonical::{canonical_bytes, from_canonical_bytes, signing_digest};
use web_time::SystemTime;

use tilo_relay::delivery_v3::media_wire::{
    ChallengeTranscript, NativeChallenge, RefreshChallenge, RelayProofChallenge, SignedGrant,
};
pub use tilo_relay::delivery_v3::media_wire::{
    GrantMetadata, GrantOperation, GrantRequest, MediaDirections, MediaNamespace,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CallPolicy {
    pub epoch: u64,
    pub membership_generation: u64,
    pub directions: MediaDirections,
}

pub trait MediaCallPolicy {
    fn current(
        &self,
        peer: &StatePeer,
        request: &GrantRequest,
        now: SystemTime,
    ) -> Result<CallPolicy, MediaError>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MediaError {
    InvalidRequest,
    Unauthorized,
    StaleGeneration,
    Capacity,
    InvalidProof,
    UnknownChallenge,
    Expired,
    InvalidGrant,
    SigningFailed,
    EntropyUnavailable,
}

#[derive(Clone, Debug, Serialize)]
pub struct MediaChallenge {
    #[serde(skip)]
    pub(crate) transcript: ChallengeTranscript,
    pub nonce: [u8; 32],
    pub signing_digest: [u8; 32],
    pub expires_at_ns: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MediaDirection {
    Publish,
    Subscribe,
}

pub struct MediaGrant {
    bytes: Vec<u8>,
    pub metadata: GrantMetadata,
}
impl MediaGrant {
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }
}

pub enum GrantCompletion {
    Granted(MediaGrant),
    Left,
    Present { expires_at_ns: u64 },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct MediaPresence {
    pub root: [u8; 32],
    pub device: [u8; 32],
    pub participation_id: [u8; 32],
    pub generation: u64,
    pub namespace: MediaNamespace,
    pub expires_at_ns: u64,
}
/// Only successful token-bound device proof creates this handle. Keep it with
/// the exact transport session whose ID was supplied to `relay_challenge`.
pub struct RelaySession {
    metadata: GrantMetadata,
    session_id: [u8; 32],
    authorization: [u8; 32],
}
impl RelaySession {
    pub fn metadata(&self) -> &GrantMetadata {
        &self.metadata
    }
    pub fn session_id(&self) -> [u8; 32] {
        self.session_id
    }
}

/// Frozen signature domain. The canonical tuple's first byte separates grant,
/// native request, relay proof and namespace derivation purposes.
pub const MEDIA_PROOF_DOMAIN: &[u8] = b"tilo-media-v1\0";
pub const MAX_MEDIA_GRANT_BYTES: usize = 4096;
pub const MAX_MEDIA_CHALLENGES: usize = 128;
pub const MAX_MEDIA_PARTICIPATION_KEYS: usize = 1024;
pub const MAX_MEDIA_SESSION_FENCES: usize = 1024;
pub const MEDIA_PRESENCE_LEASE_NS: u64 = 15_000_000_000;
pub const MEDIA_HEARTBEAT_INTERVAL_NS: u64 = 5_000_000_000;
const LIFETIME_NS: u64 = 60_000_000_000;
const MAX_CALL_PARTICIPANTS: usize = 8;
const MAX_ACTIVE_PARTICIPANTS: usize = 32;

type ParticipationKey = ([u8; 32], String, String);
struct Participation {
    metadata: GrantMetadata,
    active: bool,
    // Independent online-presence projection. This never determines the grant
    // occupancy budget or extends a media authorization.
    presence_expires_at_ns: u64,
}
struct RelayRecord {
    authorization: [u8; 32],
    expires_at_ns: u64,
    closed: bool,
}
struct Pending {
    challenge: MediaChallenge,
    issued_at_ns: u64,
    purpose: Purpose,
}
enum Purpose {
    Native {
        request: GrantRequest,
        operation: GrantOperation,
        policy: CallPolicy,
    },
    Relay {
        metadata: GrantMetadata,
        session_id: [u8; 32],
    },
    Refresh {
        metadata: GrantMetadata,
        session_id: [u8; 32],
        authorization: [u8; 32],
    },
}

/// Ephemeral authority owned by the serialized native owner. This type never
/// snapshots StatePeer itself: callers supply the current committed snapshot on
/// every turn. The call-policy implementation must read that same committed view.
/// Restart creates a fresh boot ID and invalidates all prior grants and proofs.
/// No method persists, logs, or puts grant material in a URL.
pub struct MediaAuthority {
    host: [u8; 32],
    audience: String,
    host_boot: [u8; 32],
    pending: BTreeMap<[u8; 32], Pending>,
    // Non-evicting intent fences for this boot: a delayed Join cannot acquire a newer generation.
    reservations: BTreeMap<(ParticipationKey, [u8; 32]), GrantRequest>,
    // High-water marks survive Leave/expiry for this boot. Never evict one to
    // admit another key: doing so could resurrect stale participation activity.
    participations: BTreeMap<ParticipationKey, Participation>,
    relay_sessions: BTreeMap<[u8; 32], RelayRecord>,
}

impl MediaAuthority {
    /// Pin the exact native room authority signer and the adapter's audience.
    /// The key's certification is checked against StatePeer on every operation.
    pub fn new(host: [u8; 32], audience: &str) -> Result<Self, MediaError> {
        let key = VerifyingKey::from_bytes(&host).map_err(|_| MediaError::InvalidRequest)?;
        if key.is_weak() || !bounded_text(audience, 256) {
            return Err(MediaError::InvalidRequest);
        }
        Ok(Self {
            host,
            audience: audience.into(),
            host_boot: fresh_nonce()?,
            pending: BTreeMap::new(),
            reservations: BTreeMap::new(),
            participations: BTreeMap::new(),
            relay_sessions: BTreeMap::new(),
        })
    }

    /// Reserve a nonzero generation for an authenticated fresh participation intent.
    /// Caller identity is checked by control before this method; native permissions
    /// are rechecked here. Intent fences are bounded and never evicted during boot.
    pub(crate) fn reserve_join(
        &mut self,
        peer: &StatePeer,
        policy: &dyn MediaCallPolicy,
        mut request: GrantRequest,
        now: SystemTime,
    ) -> Result<GrantRequest, MediaError> {
        if request.generation != 0 {
            return Err(MediaError::InvalidRequest);
        }
        let now_ns = timestamp(now)?;
        let key = participation_key(&request);
        let prior = self.participations.get(&key);
        let floor = self
            .reservations
            .iter()
            .filter(|((k, _), _)| k == &key)
            .map(|(_, r)| r.generation)
            .max()
            .unwrap_or(0)
            .max(prior.map_or(0, |p| p.metadata.request.generation));
        let intent_key = (key, request.participation_id);
        if let Some(reserved) = self.reservations.get(&intent_key) {
            request.generation = reserved.generation;
            if &request != reserved
                || request.generation < floor
                || prior.is_some_and(|p| {
                    p.metadata.request.participation_id == request.participation_id
                        && (!p.active || p.metadata.expires_at_ns <= now_ns)
                })
            {
                return Err(MediaError::StaleGeneration);
            }
        } else {
            if self.reservations.len() >= MAX_MEDIA_PARTICIPATION_KEYS {
                return Err(MediaError::Capacity);
            }
            if prior
                .is_some_and(|p| p.metadata.request.participation_id == request.participation_id)
            {
                return Err(MediaError::StaleGeneration);
            }
            request.generation = floor.checked_add(1).ok_or(MediaError::StaleGeneration)?;
        }
        self.permission(peer, policy, &request, now)?;
        self.reservations.insert(intent_key, request.clone());
        Ok(request)
    }

    /// Begin Join, Renew or Leave. The request is untrusted. The response digest
    /// binds the entire request, operation, current policy, audience and boot.
    pub fn challenge(
        &mut self,
        peer: &StatePeer,
        policy: &dyn MediaCallPolicy,
        request: GrantRequest,
        operation: GrantOperation,
        now: SystemTime,
    ) -> Result<MediaChallenge, MediaError> {
        let now_ns = timestamp(now)?;
        let (_, current) = self.permission(peer, policy, &request, now)?;
        self.check_generation(&request, operation, current, now_ns)?;
        let nonce = self.reserve_nonce(now_ns)?;
        let expires_at_ns = now_ns
            .checked_add(LIFETIME_NS)
            .ok_or(MediaError::InvalidRequest)?;
        let digest = digest(&(
            0_u8,
            &request,
            operation,
            current.epoch,
            current.membership_generation,
            current.directions,
            &self.audience,
            self.host,
            self.host_boot,
            nonce,
            now_ns,
            expires_at_ns,
        ))?;
        let challenge = MediaChallenge {
            transcript: ChallengeTranscript::Native(NativeChallenge {
                request: request.clone(),
                operation,
                epoch: current.epoch,
                membership_generation: current.membership_generation,
                directions: current.directions,
                audience: self.audience.clone(),
                host: self.host,
                host_boot: self.host_boot,
                nonce,
                issued_at_ns: now_ns,
                expires_at_ns,
            }),
            nonce,
            signing_digest: digest,
            expires_at_ns,
        };
        self.pending.insert(
            nonce,
            Pending {
                challenge: challenge.clone(),
                issued_at_ns: now_ns,
                purpose: Purpose::Native {
                    request,
                    operation,
                    policy: current,
                },
            },
        );
        Ok(challenge)
    }

    /// Consume a one-use challenge, verify the actual device signature and
    /// recheck current governance before asking native custody to sign a grant.
    /// A bad proof consumes its nonce too. A signing failure never admits a lease.
    pub fn complete(
        &mut self,
        peer: &StatePeer,
        policy: &dyn MediaCallPolicy,
        nonce: [u8; 32],
        signature: [u8; 64],
        now: SystemTime,
        sign: impl FnOnce(&[u8; 32]) -> Result<[u8; 64], MediaError>,
    ) -> Result<GrantCompletion, MediaError> {
        let pending = self.take_challenge(nonce, now)?;
        let Purpose::Native {
            request,
            operation,
            policy: challenged_policy,
        } = pending.purpose
        else {
            return Err(MediaError::InvalidProof);
        };
        verify_signature(request.device, &pending.challenge.signing_digest, signature)
            .map_err(|_| MediaError::InvalidProof)?;
        let (root, current) = self.permission(peer, policy, &request, now)?;
        if current != challenged_policy {
            return Err(MediaError::Unauthorized);
        }
        let now_ns = timestamp(now)?;
        self.check_generation(&request, operation, current, now_ns)?;
        let key = participation_key(&request);
        match operation {
            GrantOperation::Leave => {
                let participation = self
                    .participations
                    .get_mut(&key)
                    .ok_or(MediaError::StaleGeneration)?;
                participation.active = false;
                participation.presence_expires_at_ns = 0;
                return Ok(GrantCompletion::Left);
            }
            GrantOperation::Heartbeat => {
                let participation = self
                    .participations
                    .get_mut(&key)
                    .ok_or(MediaError::StaleGeneration)?;
                let expires_at_ns = now_ns
                    .checked_add(MEDIA_PRESENCE_LEASE_NS)
                    .ok_or(MediaError::InvalidRequest)?
                    .min(participation.metadata.expires_at_ns);
                participation.presence_expires_at_ns = expires_at_ns;
                return Ok(GrantCompletion::Present { expires_at_ns });
            }
            GrantOperation::Join | GrantOperation::Renew => {}
        }
        let namespace = self.namespace(root, &request, current.epoch)?;
        let metadata = GrantMetadata {
            root,
            request,
            epoch: current.epoch,
            membership_generation: current.membership_generation,
            namespace,
            audience: self.audience.clone(),
            host: self.host,
            host_boot: self.host_boot,
            issued_at_ns: now_ns,
            expires_at_ns: now_ns
                .checked_add(LIFETIME_NS)
                .ok_or(MediaError::InvalidRequest)?,
        };
        let signing_digest = digest(&(1_u8, &metadata))?;
        let signature = sign(&signing_digest)?;
        verify_signature(self.host, &signing_digest, signature)
            .map_err(|_| MediaError::SigningFailed)?;
        let wire = SignedGrant {
            version: 1,
            metadata: metadata.clone(),
            signature: split_signature(signature),
        };
        let bytes = canonical_bytes(&wire).map_err(|_| MediaError::InvalidGrant)?;
        if bytes.len() > MAX_MEDIA_GRANT_BYTES {
            return Err(MediaError::InvalidGrant);
        }
        let presence_expires_at_ns = if operation == GrantOperation::Join {
            now_ns
                .checked_add(MEDIA_PRESENCE_LEASE_NS)
                .ok_or(MediaError::InvalidRequest)?
                .min(metadata.expires_at_ns)
        } else {
            self.participations
                .get(&key)
                .ok_or(MediaError::StaleGeneration)?
                .presence_expires_at_ns
        };
        self.participations.insert(
            key,
            Participation {
                metadata: metadata.clone(),
                active: true,
                presence_expires_at_ns,
            },
        );
        Ok(GrantCompletion::Granted(MediaGrant { bytes, metadata }))
    }

    /// The adapter generates a fresh connection-local session ID outside client
    /// control. Native control can mint this challenge before transport setup;
    /// carry its proof in the negotiated protocol's standard authorization data.
    pub fn relay_challenge(
        &mut self,
        peer: &StatePeer,
        policy: &dyn MediaCallPolicy,
        grant: &[u8],
        session_id: [u8; 32],
        now: SystemTime,
    ) -> Result<MediaChallenge, MediaError> {
        if session_id == [0; 32] {
            return Err(MediaError::InvalidGrant);
        }
        let now_ns = timestamp(now)?;
        self.reclaim_expired(now_ns);
        self.check_new_session(session_id, now_ns)?;
        if self.pending.values().any(|p| {
            matches!(&p.purpose,
            Purpose::Relay { session_id: id, .. } if *id == session_id)
        }) {
            return Err(MediaError::StaleGeneration);
        }
        let metadata = self.verify_grant(peer, policy, grant, now)?;
        let nonce = self.reserve_nonce(now_ns)?;
        let expires_at_ns = now_ns
            .checked_add(LIFETIME_NS)
            .ok_or(MediaError::InvalidRequest)?
            .min(metadata.expires_at_ns);
        let signing_digest = digest(&(
            2_u8,
            blake3::hash(grant).as_bytes(),
            session_id,
            &self.audience,
            self.host,
            self.host_boot,
            nonce,
            now_ns,
            expires_at_ns,
        ))?;
        let challenge = MediaChallenge {
            transcript: ChallengeTranscript::Relay(RelayProofChallenge {
                grant_digest: *blake3::hash(grant).as_bytes(),
                session_id,
                audience: self.audience.clone(),
                host: self.host,
                host_boot: self.host_boot,
                nonce,
                issued_at_ns: now_ns,
                expires_at_ns,
            }),
            nonce,
            signing_digest,
            expires_at_ns,
        };
        self.pending.insert(
            nonce,
            Pending {
                challenge: challenge.clone(),
                issued_at_ns: now_ns,
                purpose: Purpose::Relay {
                    metadata,
                    session_id,
                },
            },
        );
        Ok(challenge)
    }

    /// A grant is never a bearer credential: a fresh device proof on this exact
    /// relay session is required before returning the private session handle.
    pub fn authenticate_relay(
        &mut self,
        peer: &StatePeer,
        policy: &dyn MediaCallPolicy,
        session_id: [u8; 32],
        nonce: [u8; 32],
        signature: [u8; 64],
        now: SystemTime,
    ) -> Result<RelaySession, MediaError> {
        let pending = self.take_challenge(nonce, now)?;
        let Purpose::Relay {
            metadata,
            session_id: expected,
        } = pending.purpose
        else {
            return Err(MediaError::InvalidProof);
        };
        if expected != session_id {
            return Err(MediaError::InvalidProof);
        }
        verify_signature(
            metadata.request.device,
            &pending.challenge.signing_digest,
            signature,
        )
        .map_err(|_| MediaError::InvalidProof)?;
        self.check_live(peer, policy, &metadata, now)?;
        let now_ns = timestamp(now)?;
        self.reclaim_expired(now_ns);
        self.check_new_session(session_id, now_ns)?;
        self.relay_sessions.insert(
            session_id,
            RelayRecord {
                authorization: nonce,
                expires_at_ns: metadata.expires_at_ns,
                closed: false,
            },
        );
        Ok(RelaySession {
            metadata,
            session_id,
            authorization: nonce,
        })
    }

    /// Native-control renewal for an already authenticated transport session.
    /// This does not define an extra MOQT control message. The proof is bound to
    /// this exact open handle and the canonical replacement grant.
    pub fn refresh_challenge(
        &mut self,
        peer: &StatePeer,
        policy: &dyn MediaCallPolicy,
        session: &RelaySession,
        new_grant: &[u8],
        now: SystemTime,
    ) -> Result<MediaChallenge, MediaError> {
        self.check_session(session)?;
        self.check_live(peer, policy, &session.metadata, now)?;
        let metadata = self.verify_grant(peer, policy, new_grant, now)?;
        if !same_relay_identity(&session.metadata, &metadata)
            || metadata.issued_at_ns < session.metadata.issued_at_ns
            || metadata.expires_at_ns < session.metadata.expires_at_ns
        {
            return Err(MediaError::InvalidGrant);
        }
        let now_ns = timestamp(now)?;
        let nonce = self.reserve_nonce(now_ns)?;
        // Every outstanding refresh proof dies no later than the authorization
        // it replaces. This is the reclamation bound for its session fence.
        let expires_at_ns = now_ns
            .checked_add(LIFETIME_NS)
            .ok_or(MediaError::InvalidRequest)?
            .min(session.metadata.expires_at_ns)
            .min(metadata.expires_at_ns);
        let signing_digest = digest(&(
            5_u8,
            blake3::hash(new_grant).as_bytes(),
            session.session_id,
            session.authorization,
            &self.audience,
            self.host,
            self.host_boot,
            nonce,
            now_ns,
            expires_at_ns,
        ))?;
        let challenge = MediaChallenge {
            transcript: ChallengeTranscript::Refresh(RefreshChallenge {
                grant_digest: *blake3::hash(new_grant).as_bytes(),
                session_id: session.session_id,
                authorization: session.authorization,
                audience: self.audience.clone(),
                host: self.host,
                host_boot: self.host_boot,
                nonce,
                issued_at_ns: now_ns,
                expires_at_ns,
            }),
            nonce,
            signing_digest,
            expires_at_ns,
        };
        self.pending.insert(
            nonce,
            Pending {
                challenge: challenge.clone(),
                issued_at_ns: now_ns,
                purpose: Purpose::Refresh {
                    metadata,
                    session_id: session.session_id,
                    authorization: session.authorization,
                },
            },
        );
        Ok(challenge)
    }

    /// Atomically replace the native session authorization. The transport owner
    /// must install this returned private handle in the same serialized owner
    /// turn. Every old handle immediately fails authorization and cannot close
    /// this replacement. A concurrent Close invalidates the pending proof.
    pub fn refresh_relay(
        &mut self,
        peer: &StatePeer,
        policy: &dyn MediaCallPolicy,
        session: &RelaySession,
        nonce: [u8; 32],
        signature: [u8; 64],
        now: SystemTime,
    ) -> Result<RelaySession, MediaError> {
        let pending = self.take_challenge(nonce, now)?;
        let Purpose::Refresh {
            metadata,
            session_id,
            authorization,
        } = pending.purpose
        else {
            return Err(MediaError::InvalidProof);
        };
        if session_id != session.session_id || authorization != session.authorization {
            return Err(MediaError::InvalidProof);
        }
        self.check_session(session)?;
        verify_signature(
            metadata.request.device,
            &pending.challenge.signing_digest,
            signature,
        )
        .map_err(|_| MediaError::InvalidProof)?;
        self.check_live(peer, policy, &session.metadata, now)?;
        self.check_live(peer, policy, &metadata, now)?;
        if !same_relay_identity(&session.metadata, &metadata) {
            return Err(MediaError::InvalidGrant);
        }
        self.relay_sessions.insert(
            session_id,
            RelayRecord {
                authorization: nonce,
                expires_at_ns: metadata.expires_at_ns,
                closed: false,
            },
        );
        Ok(RelaySession {
            metadata,
            session_id,
            authorization: nonce,
        })
    }

    /// Close only the current authorization generation; a late cleanup callback
    /// carrying an older handle cannot close a renewed session.
    pub fn close_relay(&mut self, session: &RelaySession) -> Result<(), MediaError> {
        self.check_session(session)?;
        self.relay_sessions
            .get_mut(&session.session_id)
            .ok_or(MediaError::StaleGeneration)?
            .closed = true;
        self.pending.retain(|_, p| match &p.purpose {
            Purpose::Relay { session_id, .. } | Purpose::Refresh { session_id, .. } => {
                *session_id != session.session_id
            }
            Purpose::Native { .. } => true,
        });
        Ok(())
    }

    /// Exact bounded namespace inventory for a protocol adapter; contains no
    /// credentials. Each later subscription/forward must still call authorize.
    pub fn subscription_namespaces(
        &self,
        peer: &StatePeer,
        policy: &dyn MediaCallPolicy,
        session: &RelaySession,
        now: SystemTime,
    ) -> Result<Vec<MediaNamespace>, MediaError> {
        self.check_session(session)?;
        self.check_live(peer, policy, &session.metadata, now)?;
        if !session.metadata.request.directions.subscribe {
            return Err(MediaError::Unauthorized);
        }
        let mut namespaces = Vec::new();
        for p in self.participations.values().filter(|p| {
            p.active
                && p.metadata.namespace.call == session.metadata.namespace.call
                && p.metadata.namespace != session.metadata.namespace
        }) {
            if self
                .authorize(
                    peer,
                    policy,
                    session,
                    p.metadata.namespace,
                    MediaDirection::Subscribe,
                    now,
                )
                .is_ok()
            {
                if namespaces.len() >= MAX_CALL_PARTICIPANTS {
                    return Err(MediaError::Capacity);
                }
                namespaces.push(p.metadata.namespace);
            }
        }
        Ok(namespaces)
    }

    /// Short online-presence leases, distinct from granted media resource
    /// occupancy. Clients refresh at MEDIA_HEARTBEAT_INTERVAL_NS; after silence
    /// this projection expires at 15 seconds, while the hard grant resource cap
    /// remains reserved until Leave or grant expiry. Grant renewal alone does
    /// not refresh presence. Reading this roster requires current Room Read and
    /// a privately authenticated session with the subscribe direction.
    pub fn presence_snapshot(
        &self,
        peer: &StatePeer,
        policy: &dyn MediaCallPolicy,
        session: &RelaySession,
        now: SystemTime,
    ) -> Result<Vec<MediaPresence>, MediaError> {
        self.check_session(session)?;
        self.check_live(peer, policy, &session.metadata, now)?;
        if !session.metadata.request.directions.subscribe {
            return Err(MediaError::Unauthorized);
        }
        let now_ns = timestamp(now)?;
        let mut snapshot = Vec::new();
        for participation in self.participations.values().filter(|p| {
            p.active
                && p.presence_expires_at_ns > now_ns
                && p.metadata.namespace.call == session.metadata.namespace.call
        }) {
            if self
                .check_live(peer, policy, &participation.metadata, now)
                .is_ok()
            {
                if snapshot.len() >= MAX_CALL_PARTICIPANTS {
                    return Err(MediaError::Capacity);
                }
                let metadata = &participation.metadata;
                snapshot.push(MediaPresence {
                    root: metadata.root,
                    device: metadata.request.device,
                    participation_id: metadata.request.participation_id,
                    generation: metadata.request.generation,
                    namespace: metadata.namespace,
                    expires_at_ns: participation.presence_expires_at_ns,
                });
            }
        }
        Ok(snapshot)
    }

    /// This relay session's own current authority, independent of any remote
    /// publisher: the session half of every `authorize` decision. A relay
    /// that observes this separately can tell its own revocation apart from a
    /// remote publisher whose authority lapsed.
    pub fn authorize_session(
        &self,
        peer: &StatePeer,
        policy: &dyn MediaCallPolicy,
        session: &RelaySession,
        now: SystemTime,
    ) -> Result<(), MediaError> {
        self.check_session(session)?;
        self.check_live(peer, policy, &session.metadata, now)
    }

    /// Check each publish/subscribe and each forwarding turn. The adapter must
    /// call this again during forwarding, even when a stream remains open.
    /// These two fixed opaque components are the complete authorized voice
    /// namespace; the adapter maps only the qualified audio track beneath it.
    pub fn authorize(
        &self,
        peer: &StatePeer,
        policy: &dyn MediaCallPolicy,
        session: &RelaySession,
        namespace: MediaNamespace,
        direction: MediaDirection,
        now: SystemTime,
    ) -> Result<(), MediaError> {
        let metadata = &session.metadata;
        self.check_session(session)?;
        self.check_live(peer, policy, metadata, now)?;
        if namespace.call != metadata.namespace.call {
            return Err(MediaError::Unauthorized);
        }
        match direction {
            MediaDirection::Publish => {
                if !metadata.request.directions.publish || namespace != metadata.namespace {
                    return Err(MediaError::Unauthorized);
                }
            }
            MediaDirection::Subscribe => {
                if !metadata.request.directions.subscribe {
                    return Err(MediaError::Unauthorized);
                }
                // Never authorize an unbounded wildcard or an unknown publisher.
                let publisher = self
                    .participations
                    .values()
                    .find(|p| p.active && p.metadata.namespace == namespace)
                    .ok_or(MediaError::Unauthorized)?;
                self.check_live(peer, policy, &publisher.metadata, now)?;
            }
        }
        Ok(())
    }

    fn permission(
        &self,
        peer: &StatePeer,
        policy: &dyn MediaCallPolicy,
        request: &GrantRequest,
        now: SystemTime,
    ) -> Result<([u8; 32], CallPolicy), MediaError> {
        if !bounded_text(&request.room, 36)
            || !bounded_text(&request.call_id, 128)
            || request.participation_id == [0; 32]
            || request.generation == 0
            || (!request.directions.publish && !request.directions.subscribe)
        {
            return Err(MediaError::InvalidRequest);
        }
        if request.room != peer.room() || self.host != peer.authority().device_public_key {
            return Err(MediaError::Unauthorized);
        }
        // permissions() verifies current cert, root signature, revocations, exact
        // governed roster certificate and current membership before role/caps.
        peer.permissions(self.host, now)
            .map_err(|_| MediaError::Unauthorized)?;
        let (role, caps, person) = peer
            .permissions(request.device, now)
            .map_err(|_| MediaError::Unauthorized)?;
        let actor = Actor::person(Some(role), &person).in_room(request.room.clone());
        let resource = ResourceRef::Room(request.room.clone());
        let seconds = timestamp(now)? / 1_000_000_000;
        for (requested, verb) in [
            (request.directions.subscribe, Verb::Read),
            (request.directions.publish, Verb::Write),
        ] {
            if requested
                && decide_capability(&actor, &resource, verb, &caps, Some(seconds))
                    != CapDecision::Allow
            {
                return Err(MediaError::Unauthorized);
            }
        }
        let current = policy.current(peer, request, now)?;
        if current.epoch == 0
            || current.membership_generation == 0
            || (request.directions.publish && !current.directions.publish)
            || (request.directions.subscribe && !current.directions.subscribe)
        {
            return Err(MediaError::Unauthorized);
        }
        // StatePeer derives this principal from the currently verified root.
        // Decode its canonical key instead of cloning the entire roster on each
        // forwarding check.
        let mut root = [0; 32];
        hex::decode_to_slice(&person, &mut root).map_err(|_| MediaError::Unauthorized)?;
        Ok((root, current))
    }

    fn check_generation(
        &self,
        request: &GrantRequest,
        operation: GrantOperation,
        policy: CallPolicy,
        now_ns: u64,
    ) -> Result<(), MediaError> {
        let key = participation_key(request);
        if self
            .reservations
            .iter()
            .any(|((k, _), r)| k == &key && r.generation > request.generation)
        {
            return Err(MediaError::StaleGeneration);
        }
        let previous = self.participations.get(&key);
        let same = previous
            .is_some_and(|p| p.metadata.request == *request && p.metadata.epoch == policy.epoch);
        let live = previous.is_some_and(|p| p.active && p.metadata.expires_at_ns > now_ns);
        match operation {
            GrantOperation::Join => {
                if let Some(previous) = previous {
                    if request.generation < previous.metadata.request.generation
                        || (request.generation == previous.metadata.request.generation
                            && !(same && live))
                    {
                        return Err(MediaError::StaleGeneration);
                    }
                } else if self.participations.len() >= MAX_MEDIA_PARTICIPATION_KEYS {
                    return Err(MediaError::Capacity);
                }
                // Count the resulting set, excluding only the key replaced by
                // this admission. A live entry from an ended epoch does not
                // reserve capacity in the new epoch.
                let mut total = 0;
                let mut call = 0;
                for (other_key, p) in self.participations.iter().filter(|(other_key, p)| {
                    *other_key != &key && p.active && p.metadata.expires_at_ns > now_ns
                }) {
                    total += 1;
                    if other_key.1 == request.room
                        && other_key.2 == request.call_id
                        && p.metadata.epoch == policy.epoch
                    {
                        call += 1;
                    }
                }
                if total >= MAX_ACTIVE_PARTICIPANTS || call >= MAX_CALL_PARTICIPANTS {
                    return Err(MediaError::Capacity);
                }
            }
            GrantOperation::Renew | GrantOperation::Heartbeat if !same || !live => {
                return Err(MediaError::StaleGeneration);
            }
            // Leaving a participation that moderation superseded (the call
            // epoch advanced) is still the same participation's exit: accept
            // it so the client can clear local intent and rejoin at once.
            GrantOperation::Leave if !previous.is_some_and(|p| p.metadata.request == *request) => {
                return Err(MediaError::StaleGeneration);
            }
            _ => {}
        }
        Ok(())
    }

    fn check_live(
        &self,
        peer: &StatePeer,
        policy: &dyn MediaCallPolicy,
        metadata: &GrantMetadata,
        now: SystemTime,
    ) -> Result<(), MediaError> {
        let now_ns = timestamp(now)?;
        if metadata.host != self.host
            || metadata.host_boot != self.host_boot
            || metadata.audience != self.audience
            || metadata.issued_at_ns > now_ns
            || metadata.expires_at_ns <= metadata.issued_at_ns
            || metadata.expires_at_ns - metadata.issued_at_ns > LIFETIME_NS
        {
            return Err(MediaError::InvalidGrant);
        }
        if now_ns >= metadata.expires_at_ns {
            return Err(MediaError::Expired);
        }
        let (root, current) = self
            .permission(peer, policy, &metadata.request, now)
            .inspect_err(
                |error| tracing::debug!(reason = ?error, "media grant no longer permitted"),
            )?;
        if root != metadata.root
            || current.epoch != metadata.epoch
            || current.membership_generation != metadata.membership_generation
            || self.namespace(root, &metadata.request, current.epoch)? != metadata.namespace
        {
            // Content-free: which binding changed since the grant was signed.
            tracing::debug!(
                root_changed = root != metadata.root,
                epoch_changed = current.epoch != metadata.epoch,
                membership_changed =
                    current.membership_generation != metadata.membership_generation,
                "media grant binding changed"
            );
            // Still permitted under current policy (permission() passed) but
            // bound to a superseded epoch/membership: stale, not revoked.
            return Err(if root == metadata.root {
                MediaError::StaleGeneration
            } else {
                MediaError::Unauthorized
            });
        }
        self.check_generation(&metadata.request, GrantOperation::Renew, current, now_ns)
    }

    fn namespace(
        &self,
        root: [u8; 32],
        request: &GrantRequest,
        epoch: u64,
    ) -> Result<MediaNamespace, MediaError> {
        let call = digest(&(3_u8, self.host_boot, &request.room, &request.call_id, epoch))?;
        let publisher = digest(&(
            4_u8,
            call,
            root,
            request.device,
            request.participation_id,
            request.generation,
        ))?;
        Ok(MediaNamespace { call, publisher })
    }

    fn verify_grant(
        &self,
        peer: &StatePeer,
        policy: &dyn MediaCallPolicy,
        grant: &[u8],
        now: SystemTime,
    ) -> Result<GrantMetadata, MediaError> {
        if grant.is_empty() || grant.len() > MAX_MEDIA_GRANT_BYTES {
            return Err(MediaError::InvalidGrant);
        }
        let signed: SignedGrant =
            from_canonical_bytes(grant).map_err(|_| MediaError::InvalidGrant)?;
        if signed.version != 1
            || canonical_bytes(&signed).map_err(|_| MediaError::InvalidGrant)? != grant
        {
            return Err(MediaError::InvalidGrant);
        }
        verify_signature(
            self.host,
            &digest(&(1_u8, &signed.metadata))?,
            join_signature(signed.signature),
        )
        .map_err(|_| MediaError::InvalidGrant)?;
        self.check_live(peer, policy, &signed.metadata, now)?;
        Ok(signed.metadata)
    }

    fn check_session(&self, session: &RelaySession) -> Result<(), MediaError> {
        if self
            .relay_sessions
            .get(&session.session_id)
            .is_some_and(|record| {
                !record.closed
                    && record.authorization == session.authorization
                    && record.expires_at_ns == session.metadata.expires_at_ns
            })
        {
            Ok(())
        } else {
            Err(MediaError::StaleGeneration)
        }
    }

    fn check_new_session(&self, session_id: [u8; 32], now_ns: u64) -> Result<(), MediaError> {
        if self.relay_sessions.contains_key(&session_id) {
            return Err(MediaError::StaleGeneration);
        }
        if self.relay_sessions.len() >= MAX_MEDIA_SESSION_FENCES
            || self
                .relay_sessions
                .values()
                .filter(|record| !record.closed && record.expires_at_ns > now_ns)
                .count()
                >= MAX_ACTIVE_PARTICIPANTS
        {
            return Err(MediaError::Capacity);
        }
        Ok(())
    }

    fn reclaim_expired(&mut self, now_ns: u64) {
        self.pending
            .retain(|_, p| p.challenge.expires_at_ns > now_ns);
        // A fence outlives every grant it represents and every refresh challenge
        // that can reference its authorization. Close also consumes its pending
        // proofs. After expiry, the old handle's grant cannot authorize anything;
        // reuse creates a new random authorization nonce, never the old handle.
        self.relay_sessions
            .retain(|_, record| record.expires_at_ns > now_ns);
    }

    fn reserve_nonce(&mut self, now_ns: u64) -> Result<[u8; 32], MediaError> {
        self.reclaim_expired(now_ns);
        if self.pending.len() >= MAX_MEDIA_CHALLENGES {
            return Err(MediaError::Capacity);
        }
        for _ in 0..4 {
            let nonce = fresh_nonce()?;
            if !self.pending.contains_key(&nonce) {
                return Ok(nonce);
            }
        }
        Err(MediaError::Capacity)
    }

    fn take_challenge(&mut self, nonce: [u8; 32], now: SystemTime) -> Result<Pending, MediaError> {
        let pending = self
            .pending
            .remove(&nonce)
            .ok_or(MediaError::UnknownChallenge)?;
        let now_ns = timestamp(now)?;
        if now_ns < pending.issued_at_ns || now_ns >= pending.challenge.expires_at_ns {
            return Err(MediaError::Expired);
        }
        Ok(pending)
    }
}

impl std::fmt::Display for MediaError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Only closed error labels: never echo untrusted credentials or proofs.
        write!(f, "media authority: {self:?}")
    }
}
impl std::error::Error for MediaError {}

fn same_relay_identity(before: &GrantMetadata, after: &GrantMetadata) -> bool {
    before.root == after.root
        && before.request == after.request
        && before.epoch == after.epoch
        && before.namespace == after.namespace
        && before.audience == after.audience
        && before.host == after.host
        && before.host_boot == after.host_boot
}
fn bounded_text(value: &str, limit: usize) -> bool {
    !value.is_empty() && value.len() <= limit && !value.chars().any(char::is_control)
}
fn timestamp(now: SystemTime) -> Result<u64, MediaError> {
    now.duration_since(SystemTime::UNIX_EPOCH)
        .map_err(|_| MediaError::InvalidRequest)?
        .as_nanos()
        .try_into()
        .map_err(|_| MediaError::InvalidRequest)
}
fn digest(value: &impl Serialize) -> Result<[u8; 32], MediaError> {
    signing_digest(MEDIA_PROOF_DOMAIN, value).map_err(|_| MediaError::InvalidRequest)
}
fn fresh_nonce() -> Result<[u8; 32], MediaError> {
    let mut nonce = [0; 32];
    rand::rngs::OsRng
        .try_fill_bytes(&mut nonce)
        .map_err(|_| MediaError::EntropyUnavailable)?;
    Ok(nonce)
}
fn participation_key(request: &GrantRequest) -> ParticipationKey {
    (
        request.device,
        request.room.clone(),
        request.call_id.clone(),
    )
}
fn verify_signature(key: [u8; 32], digest: &[u8; 32], signature: [u8; 64]) -> Result<(), ()> {
    VerifyingKey::from_bytes(&key)
        .map_err(|_| ())?
        .verify_strict(digest, &Signature::from_bytes(&signature))
        .map_err(|_| ())
}
fn split_signature(signature: [u8; 64]) -> [[u8; 32]; 2] {
    let mut halves = [[0; 32]; 2];
    halves[0].copy_from_slice(&signature[..32]);
    halves[1].copy_from_slice(&signature[32..]);
    halves
}
fn join_signature(halves: [[u8; 32]; 2]) -> [u8; 64] {
    let mut signature = [0; 64];
    signature[..32].copy_from_slice(&halves[0]);
    signature[32..].copy_from_slice(&halves[1]);
    signature
}

#[cfg(test)]
mod tests {
    use super::*;
    use collab_types::{
        Ed25519PublicKey,
        caps::{CapOverride, Effect, Principal, Scope, Subject, Verb},
    };
    use ed25519_dalek::{Signer, SigningKey};
    use qos_peer_state::SeedState;
    use tilo_user_identity::{DeviceCapabilities, DeviceCert, RevocationList, sign_device_cert};
    use ui::automerge::{AutoCommit, ROOT, ReadDoc, transaction::Transactable};
    use web_time::Duration;

    const ROOM: &str = "0190e67f-71e8-7edc-b54e-e47b9fbb6170";
    const BOTH: MediaDirections = MediaDirections {
        publish: true,
        subscribe: true,
    };
    const LISTEN: MediaDirections = MediaDirections {
        publish: false,
        subscribe: true,
    };

    fn at(seconds: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(seconds)
    }

    struct Policy(CallPolicy);
    impl MediaCallPolicy for Policy {
        fn current(
            &self,
            _: &StatePeer,
            request: &GrantRequest,
            _: SystemTime,
        ) -> Result<CallPolicy, MediaError> {
            if request.call_id != "call-one" && request.call_id != "ab".repeat(32) {
                return Err(MediaError::Unauthorized);
            }
            Ok(self.0)
        }
    }

    struct Fixture {
        peer: StatePeer,
        host: SigningKey,
        device: SigningKey,
        policy: Policy,
        service: MediaAuthority,
    }

    fn cert(root: &SigningKey, device: &SigningKey, host: bool) -> DeviceCert {
        let mut cert = DeviceCert {
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key: device.verifying_key().to_bytes(),
            device_label: "test device".into(),
            capabilities: if host {
                DeviceCapabilities::host()
            } else {
                DeviceCapabilities::member()
            },
            issued_at_ns: 0,
            expires_at_ns: None,
            signature: [0; 64],
        };
        sign_device_cert(&mut cert, root).unwrap();
        cert
    }

    impl Fixture {
        fn new(verb: Verb) -> Self {
            Self::with_certificate_expiry(verb, None)
        }

        fn with_certificate_expiry(verb: Verb, expires_at_ns: Option<u64>) -> Self {
            let root = SigningKey::from_bytes(&[1; 32]);
            let member_root = SigningKey::from_bytes(&[2; 32]);
            let host = SigningKey::from_bytes(&[3; 32]);
            let device = SigningKey::from_bytes(&[4; 32]);
            let host_cert = cert(&root, &host, true);
            let mut device_cert = cert(&member_root, &device, false);
            device_cert.expires_at_ns = expires_at_ns;
            sign_device_cert(&mut device_cert, &member_root).unwrap();
            let roster = vec![host_cert.clone(), device_cert.clone()];
            let mut peer = StatePeer::new(
                ROOM,
                host_cert.clone(),
                roster.clone(),
                RevocationList::new(),
                at(100),
            )
            .unwrap();
            let seed = qos_peer_state::seed_room(ROOM, &host_cert, &roster).unwrap();
            let mut doc = qos_peer_state::room::candidate(None, &seed).unwrap();
            doc.set_actor(ui::automerge::ActorId::from(
                host.verifying_key().to_bytes().to_vec(),
            ));
            let caps = doc.get(ROOT, "capabilities").unwrap().unwrap().1;
            let person = Principal::person_from_key(Ed25519PublicKey::from_bytes(
                device_cert.user_public_key,
            ));
            let cap = CapOverride::new(
                Subject::Principal(person),
                Scope::Room(ROOM.into()),
                Effect::Grant,
                verb,
            );
            doc.put(
                &caps,
                "member",
                ui::automerge::ScalarValue::Bytes(serde_json::to_vec(&cap).unwrap()),
            )
            .unwrap();
            peer.install_seed(
                SeedState {
                    room_changes: doc
                        .get_changes(&[])
                        .into_iter()
                        .map(|c| c.raw_bytes().to_vec())
                        .collect(),
                    documents: vec![],
                },
                &host,
                at(100),
            )
            .unwrap();
            let service =
                MediaAuthority::new(host.verifying_key().to_bytes(), "relay-one").unwrap();
            Self {
                peer,
                host,
                device,
                policy: Policy(CallPolicy {
                    epoch: 7,
                    membership_generation: 3,
                    directions: BOTH,
                }),
                service,
            }
        }

        fn request(&self) -> GrantRequest {
            GrantRequest {
                device: self.device.verifying_key().to_bytes(),
                room: ROOM.into(),
                call_id: "call-one".into(),
                participation_id: [9; 32],
                generation: 1,
                directions: BOTH,
            }
        }

        fn change_room(&mut self, change: impl FnOnce(&mut AutoCommit)) {
            let mut doc = self.peer.admitted_room().unwrap().clone();
            doc.set_actor(ui::automerge::ActorId::from(
                self.host.verifying_key().to_bytes().to_vec(),
            ));
            change(&mut doc);
            self.peer
                .install_seed(
                    SeedState {
                        room_changes: doc
                            .get_changes(&[])
                            .into_iter()
                            .map(|c| c.raw_bytes().to_vec())
                            .collect(),
                        documents: vec![],
                    },
                    &self.host,
                    at(100),
                )
                .unwrap();
        }

        fn add_device(&mut self, device: &SigningKey) {
            let certificate = cert(&SigningKey::from_bytes(&[2; 32]), device, false);
            let mut roster = self.peer.certificates();
            roster.push(certificate.clone());
            let mut peer = StatePeer::new(
                ROOM,
                self.peer.authority().clone(),
                roster,
                RevocationList::new(),
                at(100),
            )
            .unwrap();
            let mut doc = self.peer.admitted_room().unwrap().clone();
            doc.set_actor(ui::automerge::ActorId::from(
                self.host.verifying_key().to_bytes().to_vec(),
            ));
            let identities = doc.get(ROOT, "peer_identities_v1").unwrap().unwrap().1;
            let identity = serde_json::json!({ "device_cert": certificate, "revocations": [] });
            doc.put(
                &identities,
                hex::encode(device.verifying_key().to_bytes()),
                ui::automerge::ScalarValue::Bytes(serde_json::to_vec(&identity).unwrap()),
            )
            .unwrap();
            peer.install_seed(
                SeedState {
                    room_changes: doc
                        .get_changes(&[])
                        .into_iter()
                        .map(|c| c.raw_bytes().to_vec())
                        .collect(),
                    documents: vec![],
                },
                &self.host,
                at(100),
            )
            .unwrap();
            self.peer = peer;
        }

        fn complete(
            &mut self,
            challenge: &MediaChallenge,
            seconds: u64,
        ) -> Result<GrantCompletion, MediaError> {
            let signature = self.device.sign(&challenge.signing_digest).to_bytes();
            self.service.complete(
                &self.peer,
                &self.policy,
                challenge.nonce,
                signature,
                at(seconds),
                |digest| Ok(self.host.sign(digest).to_bytes()),
            )
        }

        fn grant(
            &mut self,
            request: GrantRequest,
            operation: GrantOperation,
            seconds: u64,
        ) -> MediaGrant {
            let challenge = self
                .service
                .challenge(&self.peer, &self.policy, request, operation, at(seconds))
                .unwrap();
            let GrantCompletion::Granted(grant) = self.complete(&challenge, seconds).unwrap()
            else {
                panic!("expected grant");
            };
            grant
        }

        fn relay(&mut self, grant: &MediaGrant, seconds: u64) -> RelaySession {
            self.relay_at_id(grant, [12; 32], seconds)
        }

        fn relay_at_id(
            &mut self,
            grant: &MediaGrant,
            session_id: [u8; 32],
            seconds: u64,
        ) -> RelaySession {
            let challenge = self
                .service
                .relay_challenge(
                    &self.peer,
                    &self.policy,
                    grant.as_bytes(),
                    session_id,
                    at(seconds),
                )
                .unwrap();
            self.service
                .authenticate_relay(
                    &self.peer,
                    &self.policy,
                    session_id,
                    challenge.nonce,
                    self.device.sign(&challenge.signing_digest).to_bytes(),
                    at(seconds),
                )
                .unwrap()
        }
    }

    #[test]
    fn governed_editor_receives_fresh_bounded_challenges() {
        let mut f = Fixture::new(Verb::Write);
        let first = f
            .service
            .challenge(
                &f.peer,
                &f.policy,
                f.request(),
                GrantOperation::Join,
                at(100),
            )
            .unwrap();
        let second = f
            .service
            .challenge(
                &f.peer,
                &f.policy,
                f.request(),
                GrantOperation::Join,
                at(100),
            )
            .unwrap();
        assert_ne!(first.nonce, second.nonce);
        assert_ne!(first.signing_digest, second.signing_digest);
        assert_eq!(first.expires_at_ns, 160_000_000_000);
    }

    #[test]
    fn grants_require_real_device_proof_and_challenges_are_one_use() {
        let mut f = Fixture::new(Verb::Write);
        let c = f
            .service
            .challenge(
                &f.peer,
                &f.policy,
                f.request(),
                GrantOperation::Join,
                at(100),
            )
            .unwrap();
        let wrong = f.host.sign(&c.signing_digest).to_bytes();
        assert!(matches!(
            f.service
                .complete(&f.peer, &f.policy, c.nonce, wrong, at(100), |_| panic!(
                    "must not sign"
                )),
            Err(MediaError::InvalidProof)
        ));
        assert!(matches!(
            f.complete(&c, 100),
            Err(MediaError::UnknownChallenge)
        ));
        let c = f
            .service
            .challenge(
                &f.peer,
                &f.policy,
                f.request(),
                GrantOperation::Join,
                at(100),
            )
            .unwrap();
        let GrantCompletion::Granted(grant) = f.complete(&c, 100).unwrap() else {
            panic!("grant missing");
        };
        assert_eq!(grant.metadata.epoch, 7);
        assert_eq!(grant.metadata.membership_generation, 3);
        assert_eq!(grant.metadata.expires_at_ns, 160_000_000_000);
        assert_eq!(
            grant.metadata.request.device,
            f.device.verifying_key().to_bytes()
        );
        assert!(matches!(
            f.complete(&c, 100),
            Err(MediaError::UnknownChallenge)
        ));
    }

    #[test]
    fn wrong_room_and_read_only_publication_are_denied() {
        let mut f = Fixture::new(Verb::Read);
        assert!(matches!(
            f.service.challenge(
                &f.peer,
                &f.policy,
                f.request(),
                GrantOperation::Join,
                at(100)
            ),
            Err(MediaError::Unauthorized)
        ));
        let mut request = f.request();
        request.directions = LISTEN;
        request.room = "0190e67f-71e8-7edc-b54e-e47b9fbb6171".into();
        assert!(matches!(
            f.service.challenge(
                &f.peer,
                &f.policy,
                request.clone(),
                GrantOperation::Join,
                at(100)
            ),
            Err(MediaError::Unauthorized)
        ));
        request.room = ROOM.into();
        let grant = f.grant(request, GrantOperation::Join, 100);
        let session = f.relay(&grant, 100);
        assert_eq!(
            f.service.authorize(
                &f.peer,
                &f.policy,
                &session,
                grant.metadata.namespace,
                MediaDirection::Publish,
                at(100)
            ),
            Err(MediaError::Unauthorized)
        );
        assert_eq!(
            f.service.authorize(
                &f.peer,
                &f.policy,
                &session,
                grant.metadata.namespace,
                MediaDirection::Subscribe,
                at(100)
            ),
            Ok(())
        );
    }

    #[test]
    fn exact_room_restriction_overrides_editor_role() {
        let mut f = Fixture::new(Verb::Write);
        f.change_room(|doc| {
            let caps = doc.get(ROOT, "capabilities").unwrap().unwrap().1;
            let cap = CapOverride::new(
                Subject::Role(collab_types::Role::Editor),
                Scope::Room(ROOM.into()),
                Effect::Restrict,
                Verb::Read,
            );
            doc.put(
                &caps,
                "readonly",
                ui::automerge::ScalarValue::Bytes(serde_json::to_vec(&cap).unwrap()),
            )
            .unwrap();
        });
        assert!(matches!(
            f.service.challenge(
                &f.peer,
                &f.policy,
                f.request(),
                GrantOperation::Join,
                at(100)
            ),
            Err(MediaError::Unauthorized)
        ));
    }

    #[test]
    fn challenge_expiry_is_inclusive_and_does_not_sign() {
        let mut f = Fixture::new(Verb::Write);
        let c = f
            .service
            .challenge(
                &f.peer,
                &f.policy,
                f.request(),
                GrantOperation::Join,
                at(100),
            )
            .unwrap();
        assert!(matches!(f.complete(&c, 160), Err(MediaError::Expired)));
    }

    #[test]
    fn relay_requires_token_bound_session_proof_and_exact_namespace() {
        let mut f = Fixture::new(Verb::Write);
        let grant = f.grant(f.request(), GrantOperation::Join, 100);
        let c = f
            .service
            .relay_challenge(&f.peer, &f.policy, grant.as_bytes(), [12; 32], at(100))
            .unwrap();
        assert!(matches!(
            f.service.authenticate_relay(
                &f.peer,
                &f.policy,
                [13; 32],
                c.nonce,
                f.device.sign(&c.signing_digest).to_bytes(),
                at(100)
            ),
            Err(MediaError::InvalidProof)
        ));
        assert!(matches!(
            f.service.authenticate_relay(
                &f.peer,
                &f.policy,
                [12; 32],
                c.nonce,
                f.device.sign(&c.signing_digest).to_bytes(),
                at(100)
            ),
            Err(MediaError::UnknownChallenge)
        ));
        let session = f.relay(&grant, 100);
        let own = grant.metadata.namespace;
        assert_eq!(
            f.service.authorize(
                &f.peer,
                &f.policy,
                &session,
                own,
                MediaDirection::Publish,
                at(159)
            ),
            Ok(())
        );
        let other = MediaNamespace {
            publisher: [42; 32],
            ..own
        };
        assert_eq!(
            f.service.authorize(
                &f.peer,
                &f.policy,
                &session,
                other,
                MediaDirection::Publish,
                at(100)
            ),
            Err(MediaError::Unauthorized)
        );
        assert_eq!(
            f.service.authorize(
                &f.peer,
                &f.policy,
                &session,
                MediaNamespace {
                    call: [42; 32],
                    ..own
                },
                MediaDirection::Subscribe,
                at(100)
            ),
            Err(MediaError::Unauthorized)
        );
        assert_eq!(
            f.service.authorize(
                &f.peer,
                &f.policy,
                &session,
                own,
                MediaDirection::Subscribe,
                at(160)
            ),
            Err(MediaError::Expired)
        );
        let mut tampered = grant.as_bytes().to_vec();
        tampered[0] ^= 1;
        assert!(matches!(
            f.service
                .relay_challenge(&f.peer, &f.policy, &tampered, [14; 32], at(100)),
            Err(MediaError::InvalidGrant)
        ));
        let mut trailing = grant.as_bytes().to_vec();
        trailing.push(0);
        assert!(matches!(
            f.service
                .relay_challenge(&f.peer, &f.policy, &trailing, [15; 32], at(100)),
            Err(MediaError::InvalidGrant)
        ));
        let mut changed: SignedGrant = from_canonical_bytes(grant.as_bytes()).unwrap();
        changed.metadata.epoch += 1;
        assert!(matches!(
            f.service.relay_challenge(
                &f.peer,
                &f.policy,
                &canonical_bytes(&changed).unwrap(),
                [16; 32],
                at(100)
            ),
            Err(MediaError::InvalidGrant)
        ));
        let mut changed: SignedGrant = from_canonical_bytes(grant.as_bytes()).unwrap();
        changed.signature[0][0] ^= 1;
        assert!(matches!(
            f.service.relay_challenge(
                &f.peer,
                &f.policy,
                &canonical_bytes(&changed).unwrap(),
                [17; 32],
                at(100)
            ),
            Err(MediaError::InvalidGrant)
        ));
    }

    #[test]
    fn current_roster_removal_denies_renewal_and_active_forwarding() {
        let mut f = Fixture::new(Verb::Write);
        let grant = f.grant(f.request(), GrantOperation::Join, 100);
        let session = f.relay(&grant, 100);
        let renewal = f
            .service
            .challenge(
                &f.peer,
                &f.policy,
                f.request(),
                GrantOperation::Renew,
                at(120),
            )
            .unwrap();
        let key = hex::encode(f.device.verifying_key().to_bytes());
        f.change_room(|doc| {
            let identities = doc.get(ROOT, "peer_identities_v1").unwrap().unwrap().1;
            doc.delete(&identities, &key).unwrap();
        });
        assert!(matches!(
            f.complete(&renewal, 120),
            Err(MediaError::Unauthorized)
        ));
        assert_eq!(
            f.service.authorize(
                &f.peer,
                &f.policy,
                &session,
                grant.metadata.namespace,
                MediaDirection::Publish,
                at(120)
            ),
            Err(MediaError::Unauthorized)
        );
    }

    #[test]
    fn renewal_rechecks_policy_and_old_epoch_grants_stop_immediately() {
        let mut f = Fixture::new(Verb::Write);
        let grant = f.grant(f.request(), GrantOperation::Join, 100);
        let session = f.relay(&grant, 100);
        let renewed = f.grant(f.request(), GrantOperation::Renew, 120);
        assert_eq!(renewed.metadata.expires_at_ns, 180_000_000_000);
        assert_eq!(renewed.metadata.namespace, grant.metadata.namespace);
        f.policy.0.epoch += 1;
        // Denied immediately; classified stale (the person is still permitted,
        // so the client rejoins the new epoch instead of ending the call).
        assert_eq!(
            f.service.authorize(
                &f.peer,
                &f.policy,
                &session,
                grant.metadata.namespace,
                MediaDirection::Publish,
                at(121)
            ),
            Err(MediaError::StaleGeneration)
        );
        assert!(matches!(
            f.service.challenge(
                &f.peer,
                &f.policy,
                f.request(),
                GrantOperation::Renew,
                at(121)
            ),
            Err(MediaError::StaleGeneration)
        ));
    }

    #[test]
    fn leave_fences_inflight_renewal_and_rejoin_uses_new_generation() {
        let mut f = Fixture::new(Verb::Write);
        let grant = f.grant(f.request(), GrantOperation::Join, 100);
        let session = f.relay(&grant, 100);
        let renewal = f
            .service
            .challenge(
                &f.peer,
                &f.policy,
                f.request(),
                GrantOperation::Renew,
                at(120),
            )
            .unwrap();
        let leave = f
            .service
            .challenge(
                &f.peer,
                &f.policy,
                f.request(),
                GrantOperation::Leave,
                at(120),
            )
            .unwrap();
        assert!(matches!(f.complete(&leave, 120), Ok(GrantCompletion::Left)));
        assert!(matches!(
            f.complete(&renewal, 120),
            Err(MediaError::StaleGeneration)
        ));
        assert!(matches!(
            f.service.challenge(
                &f.peer,
                &f.policy,
                f.request(),
                GrantOperation::Join,
                at(121)
            ),
            Err(MediaError::StaleGeneration)
        ));
        assert_eq!(
            f.service.authorize(
                &f.peer,
                &f.policy,
                &session,
                grant.metadata.namespace,
                MediaDirection::Publish,
                at(121)
            ),
            Err(MediaError::StaleGeneration)
        );
        let mut next = f.request();
        next.generation = 2;
        next.participation_id = [10; 32];
        let next_grant = f.grant(next, GrantOperation::Join, 121);
        assert_ne!(
            next_grant.metadata.namespace.publisher,
            grant.metadata.namespace.publisher
        );
    }

    #[test]
    fn host_restart_and_membership_generation_reject_old_grants() {
        let mut f = Fixture::new(Verb::Write);
        let grant = f.grant(f.request(), GrantOperation::Join, 100);
        let session = f.relay(&grant, 100);
        f.policy.0.membership_generation += 1;
        assert_eq!(
            f.service.authorize(
                &f.peer,
                &f.policy,
                &session,
                grant.metadata.namespace,
                MediaDirection::Publish,
                at(101)
            ),
            Err(MediaError::StaleGeneration)
        );
        f.policy.0.membership_generation -= 1;
        f.service = MediaAuthority::new(f.host.verifying_key().to_bytes(), "relay-one").unwrap();
        assert!(
            f.service
                .relay_challenge(&f.peer, &f.policy, grant.as_bytes(), [12; 32], at(101))
                .is_err()
        );
    }

    #[test]
    fn challenge_capacity_never_evicts_a_still_valid_nonce() {
        let mut f = Fixture::new(Verb::Write);
        let first = f
            .service
            .challenge(
                &f.peer,
                &f.policy,
                f.request(),
                GrantOperation::Join,
                at(100),
            )
            .unwrap();
        for _ in 1..128 {
            f.service
                .challenge(
                    &f.peer,
                    &f.policy,
                    f.request(),
                    GrantOperation::Join,
                    at(100),
                )
                .unwrap();
        }
        assert!(matches!(
            f.service.challenge(
                &f.peer,
                &f.policy,
                f.request(),
                GrantOperation::Join,
                at(100)
            ),
            Err(MediaError::Capacity)
        ));
        assert!(matches!(
            f.complete(&first, 100),
            Ok(GrantCompletion::Granted(_))
        ));
        assert!(
            f.service
                .challenge(
                    &f.peer,
                    &f.policy,
                    f.request(),
                    GrantOperation::Join,
                    at(100)
                )
                .is_ok()
        );
    }

    #[test]
    fn root_signed_device_revocation_stops_existing_session_and_renewal() {
        let mut f = Fixture::new(Verb::Write);
        let grant = f.grant(f.request(), GrantOperation::Join, 100);
        let session = f.relay(&grant, 100);
        let root = SigningKey::from_bytes(&[2; 32]);
        let mut revocation = tilo_user_identity::RevocationEntry {
            user_public_key: root.verifying_key().to_bytes(),
            revoked_device_public_key: f.device.verifying_key().to_bytes(),
            revoked_at_ns: 101_000_000_000,
            reason: tilo_user_identity::RevocationReason::Compromised,
            signature: [0; 64],
        };
        tilo_user_identity::sign_revocation_entry(&mut revocation, &root).unwrap();
        let key = hex::encode(f.device.verifying_key().to_bytes());
        f.change_room(|doc| {
            let identities = doc.get(ROOT, "peer_identities_v1").unwrap().unwrap().1;
            let (value, _) = doc.get(&identities, &key).unwrap().unwrap();
            let mut identity: serde_json::Value =
                serde_json::from_slice(value.to_bytes().unwrap()).unwrap();
            identity["revocations"] = serde_json::json!([revocation]);
            doc.put(
                &identities,
                &key,
                ui::automerge::ScalarValue::Bytes(serde_json::to_vec(&identity).unwrap()),
            )
            .unwrap();
        });
        assert_eq!(
            f.service.authorize(
                &f.peer,
                &f.policy,
                &session,
                grant.metadata.namespace,
                MediaDirection::Subscribe,
                at(102)
            ),
            Err(MediaError::Unauthorized)
        );
        assert!(matches!(
            f.service.challenge(
                &f.peer,
                &f.policy,
                f.request(),
                GrantOperation::Renew,
                at(102)
            ),
            Err(MediaError::Unauthorized)
        ));
    }

    #[test]
    fn policy_change_between_challenge_and_completion_is_not_signed() {
        let mut f = Fixture::new(Verb::Write);
        let c = f
            .service
            .challenge(
                &f.peer,
                &f.policy,
                f.request(),
                GrantOperation::Join,
                at(100),
            )
            .unwrap();
        f.policy.0.membership_generation += 1;
        assert!(matches!(f.complete(&c, 101), Err(MediaError::Unauthorized)));
        let c = f
            .service
            .challenge(
                &f.peer,
                &f.policy,
                f.request(),
                GrantOperation::Join,
                at(101),
            )
            .unwrap();
        f.policy.0.directions = LISTEN;
        assert!(matches!(f.complete(&c, 102), Err(MediaError::Unauthorized)));
    }

    #[test]
    fn host_signer_must_be_the_exact_current_room_authority() {
        let mut f = Fixture::new(Verb::Write);
        f.service = MediaAuthority::new(f.device.verifying_key().to_bytes(), "relay-one").unwrap();
        assert!(matches!(
            f.service.challenge(
                &f.peer,
                &f.policy,
                f.request(),
                GrantOperation::Join,
                at(100)
            ),
            Err(MediaError::Unauthorized)
        ));
    }

    #[test]
    fn rejoining_from_an_old_epoch_cannot_exceed_current_call_capacity() {
        let mut f = Fixture::new(Verb::Write);
        f.grant(f.request(), GrantOperation::Join, 100);
        f.policy.0.epoch = 8;
        for seed in 10..18 {
            let device = SigningKey::from_bytes(&[seed; 32]);
            f.add_device(&device);
            f.device = device;
            f.grant(f.request(), GrantOperation::Join, 101);
        }
        f.device = SigningKey::from_bytes(&[4; 32]);
        let mut request = f.request();
        request.generation = 2;
        assert!(matches!(
            f.service
                .challenge(&f.peer, &f.policy, request, GrantOperation::Join, at(101)),
            Err(MediaError::Capacity)
        ));
    }

    #[test]
    fn authenticated_refresh_keeps_transport_alive_past_original_expiry() {
        let mut f = Fixture::new(Verb::Write);
        let first = f.grant(f.request(), GrantOperation::Join, 100);
        let session = f.relay(&first, 100);
        let renewed = f.grant(f.request(), GrantOperation::Renew, 120);
        let challenge = f
            .service
            .refresh_challenge(&f.peer, &f.policy, &session, renewed.as_bytes(), at(120))
            .unwrap();
        let signature = f.device.sign(&challenge.signing_digest).to_bytes();
        let updated = f
            .service
            .refresh_relay(
                &f.peer,
                &f.policy,
                &session,
                challenge.nonce,
                signature,
                at(120),
            )
            .unwrap();
        assert_eq!(updated.session_id(), session.session_id());
        assert_eq!(
            f.service.authorize(
                &f.peer,
                &f.policy,
                &updated,
                renewed.metadata.namespace,
                MediaDirection::Publish,
                at(170)
            ),
            Ok(())
        );
        assert_eq!(
            f.service.authorize(
                &f.peer,
                &f.policy,
                &session,
                first.metadata.namespace,
                MediaDirection::Publish,
                at(130)
            ),
            Err(MediaError::StaleGeneration)
        );
        assert_eq!(
            f.service.close_relay(&session),
            Err(MediaError::StaleGeneration)
        );
        assert_eq!(
            f.service.authorize(
                &f.peer,
                &f.policy,
                &updated,
                renewed.metadata.namespace,
                MediaDirection::Publish,
                at(170)
            ),
            Ok(())
        );
        assert!(matches!(
            f.service.refresh_relay(
                &f.peer,
                &f.policy,
                &session,
                challenge.nonce,
                signature,
                at(120)
            ),
            Err(MediaError::UnknownChallenge)
        ));
    }

    #[test]
    fn refresh_proof_cannot_move_between_sessions_or_survive_close() {
        let mut f = Fixture::new(Verb::Write);
        let first = f.grant(f.request(), GrantOperation::Join, 100);
        let a = f.relay_at_id(&first, [12; 32], 100);
        let b = f.relay_at_id(&first, [13; 32], 100);
        let renewed = f.grant(f.request(), GrantOperation::Renew, 120);
        let challenge = f
            .service
            .refresh_challenge(&f.peer, &f.policy, &a, renewed.as_bytes(), at(120))
            .unwrap();
        assert!(matches!(
            f.service.refresh_relay(
                &f.peer,
                &f.policy,
                &b,
                challenge.nonce,
                f.device.sign(&challenge.signing_digest).to_bytes(),
                at(120)
            ),
            Err(MediaError::InvalidProof)
        ));
        let challenge = f
            .service
            .refresh_challenge(&f.peer, &f.policy, &a, renewed.as_bytes(), at(120))
            .unwrap();
        f.service.close_relay(&a).unwrap();
        assert!(
            f.service
                .refresh_relay(
                    &f.peer,
                    &f.policy,
                    &a,
                    challenge.nonce,
                    f.device.sign(&challenge.signing_digest).to_bytes(),
                    at(120)
                )
                .is_err()
        );
        assert!(
            f.service
                .refresh_challenge(&f.peer, &f.policy, &a, renewed.as_bytes(), at(120))
                .is_err()
        );
        assert!(
            f.service
                .relay_challenge(&f.peer, &f.policy, renewed.as_bytes(), [12; 32], at(120))
                .is_err()
        );
        assert_eq!(
            f.service.authorize(
                &f.peer,
                &f.policy,
                &a,
                first.metadata.namespace,
                MediaDirection::Publish,
                at(120)
            ),
            Err(MediaError::StaleGeneration)
        );
        assert_eq!(
            f.service.authorize(
                &f.peer,
                &f.policy,
                &b,
                first.metadata.namespace,
                MediaDirection::Publish,
                at(120)
            ),
            Ok(())
        );
    }

    #[test]
    fn refresh_rechecks_membership_and_rejects_replacement_participation() {
        let mut f = Fixture::new(Verb::Write);
        let first = f.grant(f.request(), GrantOperation::Join, 100);
        let session = f.relay(&first, 100);
        let renewed = f.grant(f.request(), GrantOperation::Renew, 120);
        let challenge = f
            .service
            .refresh_challenge(&f.peer, &f.policy, &session, renewed.as_bytes(), at(120))
            .unwrap();
        f.policy.0.membership_generation += 1;
        assert!(
            f.service
                .refresh_relay(
                    &f.peer,
                    &f.policy,
                    &session,
                    challenge.nonce,
                    f.device.sign(&challenge.signing_digest).to_bytes(),
                    at(120)
                )
                .is_err()
        );
        f.policy.0.membership_generation -= 1;
        let mut next = f.request();
        next.generation = 2;
        next.participation_id = [10; 32];
        let next = f.grant(next, GrantOperation::Join, 120);
        assert!(
            f.service
                .refresh_challenge(&f.peer, &f.policy, &session, next.as_bytes(), at(120))
                .is_err()
        );
    }

    #[test]
    fn expired_closed_session_fences_reclaim_without_eventual_capacity_exhaustion() {
        let mut f = Fixture::new(Verb::Write);
        let first = f.grant(f.request(), GrantOperation::Join, 100);
        let old = f.relay_at_id(&first, [12; 32], 100);
        f.service.close_relay(&old).unwrap();
        for index in 1_u64..=1100 {
            let seconds = 100 + index * 20;
            let renewed = f.grant(f.request(), GrantOperation::Renew, seconds);
            let mut session_id = [1; 32];
            session_id[..8].copy_from_slice(&index.to_le_bytes());
            let session = f.relay_at_id(&renewed, session_id, seconds);
            f.service.close_relay(&session).unwrap();
        }
        let renewed = f.grant(f.request(), GrantOperation::Renew, 22120);
        let replacement = f.relay_at_id(&renewed, [12; 32], 22120);
        assert_eq!(
            f.service.close_relay(&old),
            Err(MediaError::StaleGeneration)
        );
        assert!(
            f.service
                .refresh_challenge(&f.peer, &f.policy, &old, renewed.as_bytes(), at(22120))
                .is_err()
        );
        assert_eq!(
            f.service.authorize(
                &f.peer,
                &f.policy,
                &replacement,
                renewed.metadata.namespace,
                MediaDirection::Subscribe,
                at(22120)
            ),
            Ok(())
        );
    }

    #[test]
    fn subscription_inventory_contains_only_current_authorized_call_namespaces() {
        let mut f = Fixture::new(Verb::Write);
        let first = f.grant(f.request(), GrantOperation::Join, 100);
        let session = f.relay(&first, 100);
        let other = SigningKey::from_bytes(&[20; 32]);
        f.add_device(&other);
        f.device = other;
        let second = f.grant(f.request(), GrantOperation::Join, 100);
        let namespaces = f
            .service
            .subscription_namespaces(&f.peer, &f.policy, &session, at(100))
            .unwrap();
        assert_eq!(namespaces.len(), 1);
        assert!(!namespaces.contains(&first.metadata.namespace));
        assert!(namespaces.contains(&second.metadata.namespace));
        let key = hex::encode(f.device.verifying_key().to_bytes());
        f.change_room(|doc| {
            let identities = doc.get(ROOT, "peer_identities_v1").unwrap().unwrap().1;
            doc.delete(&identities, &key).unwrap();
        });
        assert_eq!(
            f.service
                .subscription_namespaces(&f.peer, &f.policy, &session, at(101))
                .unwrap(),
            Vec::<MediaNamespace>::new()
        );
        assert!(
            f.service
                .subscription_namespaces(&f.peer, &f.policy, &session, at(160))
                .is_err()
        );
    }

    #[test]
    fn listener_inventory_has_seven_exact_remote_publishers_and_no_scope_expansion() {
        let mut f = Fixture::new(Verb::Write);
        let mut listener = f.request();
        listener.directions = LISTEN;
        let first = f.grant(listener, GrantOperation::Join, 100);
        let session = f.relay(&first, 100);
        let mut expected = Vec::new();
        for n in 20..27 {
            let device = SigningKey::from_bytes(&[n; 32]);
            f.add_device(&device);
            f.device = device;
            let grant = f.grant(f.request(), GrantOperation::Join, 100);
            expected.push(grant.metadata.namespace);
        }
        expected.sort_unstable();
        let mut actual = f
            .service
            .subscription_namespaces(&f.peer, &f.policy, &session, at(100))
            .unwrap();
        actual.sort_unstable();
        assert_eq!(actual, expected);
        assert_eq!(actual.len(), 7);
        assert!(!actual.contains(&first.metadata.namespace));
        let outside = MediaNamespace {
            call: actual[0].call,
            publisher: [99; 32],
        };
        assert!(
            f.service
                .authorize(
                    &f.peer,
                    &f.policy,
                    &session,
                    outside,
                    MediaDirection::Subscribe,
                    at(100)
                )
                .is_err()
        );
        let device = SigningKey::from_bytes(&[28; 32]);
        f.add_device(&device);
        f.device = device;
        assert!(matches!(
            f.service.challenge(
                &f.peer,
                &f.policy,
                f.request(),
                GrantOperation::Join,
                at(100)
            ),
            Err(MediaError::Capacity)
        ));
    }

    #[test]
    fn current_certificate_expiry_denies_proof_completion_and_forwarding() {
        let mut f = Fixture::with_certificate_expiry(Verb::Write, Some(110_000_000_000));
        let grant = f.grant(f.request(), GrantOperation::Join, 100);
        let session = f.relay(&grant, 100);
        let renewal = f
            .service
            .challenge(
                &f.peer,
                &f.policy,
                f.request(),
                GrantOperation::Renew,
                at(109),
            )
            .unwrap();
        assert!(matches!(
            f.complete(&renewal, 110),
            Err(MediaError::Unauthorized)
        ));
        assert_eq!(
            f.service.authorize(
                &f.peer,
                &f.policy,
                &session,
                grant.metadata.namespace,
                MediaDirection::Publish,
                at(110)
            ),
            Err(MediaError::Unauthorized)
        );
    }

    #[test]
    fn presence_expires_after_fifteen_seconds_without_releasing_grant_authority() {
        let mut f = Fixture::new(Verb::Write);
        let grant = f.grant(f.request(), GrantOperation::Join, 100);
        let session = f.relay(&grant, 100);
        let snapshot = f
            .service
            .presence_snapshot(&f.peer, &f.policy, &session, at(114))
            .unwrap();
        assert_eq!(snapshot.len(), 1);
        assert_eq!(snapshot[0].expires_at_ns, 115_000_000_000);
        assert!(
            f.service
                .presence_snapshot(&f.peer, &f.policy, &session, at(115))
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            f.service.authorize(
                &f.peer,
                &f.policy,
                &session,
                grant.metadata.namespace,
                MediaDirection::Publish,
                at(115)
            ),
            Ok(())
        );
    }

    #[test]
    fn signed_heartbeat_refreshes_presence_without_signing_or_extending_a_grant() {
        let mut f = Fixture::new(Verb::Write);
        let grant = f.grant(f.request(), GrantOperation::Join, 100);
        let session = f.relay(&grant, 100);
        let heartbeat = f
            .service
            .challenge(
                &f.peer,
                &f.policy,
                f.request(),
                GrantOperation::Heartbeat,
                at(105),
            )
            .unwrap();
        let present = f.service.complete(
            &f.peer,
            &f.policy,
            heartbeat.nonce,
            f.device.sign(&heartbeat.signing_digest).to_bytes(),
            at(105),
            |_| panic!("heartbeat must not sign a grant"),
        );
        assert!(matches!(
            present,
            Ok(GrantCompletion::Present {
                expires_at_ns: 120_000_000_000
            })
        ));
        assert_eq!(
            f.service
                .presence_snapshot(&f.peer, &f.policy, &session, at(119))
                .unwrap()[0]
                .expires_at_ns,
            120_000_000_000
        );
        let heartbeat = f
            .service
            .challenge(
                &f.peer,
                &f.policy,
                f.request(),
                GrantOperation::Heartbeat,
                at(155),
            )
            .unwrap();
        assert!(matches!(
            f.complete(&heartbeat, 155),
            Ok(GrantCompletion::Present {
                expires_at_ns: 160_000_000_000
            })
        ));
        assert_eq!(
            f.service.authorize(
                &f.peer,
                &f.policy,
                &session,
                grant.metadata.namespace,
                MediaDirection::Publish,
                at(160)
            ),
            Err(MediaError::Expired)
        );
        assert!(
            f.service
                .challenge(
                    &f.peer,
                    &f.policy,
                    f.request(),
                    GrantOperation::Heartbeat,
                    at(160)
                )
                .is_err()
        );
    }

    #[test]
    fn grant_renewal_does_not_refresh_presence_and_publish_only_cannot_read_it() {
        let mut f = Fixture::new(Verb::Write);
        let grant = f.grant(f.request(), GrantOperation::Join, 100);
        let session = f.relay(&grant, 100);
        f.grant(f.request(), GrantOperation::Renew, 110);
        assert!(
            f.service
                .presence_snapshot(&f.peer, &f.policy, &session, at(115))
                .unwrap()
                .is_empty()
        );
        let mut f = Fixture::new(Verb::Write);
        let mut request = f.request();
        request.directions.subscribe = false;
        let grant = f.grant(request, GrantOperation::Join, 100);
        let session = f.relay(&grant, 100);
        assert!(matches!(
            f.service
                .presence_snapshot(&f.peer, &f.policy, &session, at(101)),
            Err(MediaError::Unauthorized)
        ));
    }

    #[test]
    fn heartbeat_proofs_are_one_use_device_and_room_bound_and_fenced_by_leave() {
        let mut f = Fixture::new(Verb::Write);
        let grant = f.grant(f.request(), GrantOperation::Join, 100);
        let session = f.relay(&grant, 100);
        let heartbeat = f
            .service
            .challenge(
                &f.peer,
                &f.policy,
                f.request(),
                GrantOperation::Heartbeat,
                at(105),
            )
            .unwrap();
        assert!(matches!(
            f.service.complete(
                &f.peer,
                &f.policy,
                heartbeat.nonce,
                f.host.sign(&heartbeat.signing_digest).to_bytes(),
                at(105),
                |_| panic!("wrong device must not sign")
            ),
            Err(MediaError::InvalidProof)
        ));
        assert!(matches!(
            f.complete(&heartbeat, 105),
            Err(MediaError::UnknownChallenge)
        ));
        let mut wrong_room = f.request();
        wrong_room.room = "0190e67f-71e8-7edc-b54e-e47b9fbb6171".into();
        assert!(matches!(
            f.service.challenge(
                &f.peer,
                &f.policy,
                wrong_room,
                GrantOperation::Heartbeat,
                at(105)
            ),
            Err(MediaError::Unauthorized)
        ));
        let heartbeat = f
            .service
            .challenge(
                &f.peer,
                &f.policy,
                f.request(),
                GrantOperation::Heartbeat,
                at(105),
            )
            .unwrap();
        let leave = f
            .service
            .challenge(
                &f.peer,
                &f.policy,
                f.request(),
                GrantOperation::Leave,
                at(105),
            )
            .unwrap();
        assert!(matches!(f.complete(&leave, 105), Ok(GrantCompletion::Left)));
        assert!(matches!(
            f.complete(&heartbeat, 105),
            Err(MediaError::StaleGeneration)
        ));
        assert!(
            f.service
                .presence_snapshot(&f.peer, &f.policy, &session, at(105))
                .is_err()
        );
        assert!(
            f.service
                .challenge(
                    &f.peer,
                    &f.policy,
                    f.request(),
                    GrantOperation::Heartbeat,
                    at(105)
                )
                .is_err()
        );
    }

    #[test]
    fn heartbeat_does_not_reopen_closed_relay_or_ended_epoch() {
        let mut f = Fixture::new(Verb::Write);
        let grant = f.grant(f.request(), GrantOperation::Join, 100);
        let session = f.relay(&grant, 100);
        f.service.close_relay(&session).unwrap();
        let heartbeat = f
            .service
            .challenge(
                &f.peer,
                &f.policy,
                f.request(),
                GrantOperation::Heartbeat,
                at(105),
            )
            .unwrap();
        assert!(matches!(
            f.complete(&heartbeat, 105),
            Ok(GrantCompletion::Present { .. })
        ));
        assert_eq!(
            f.service.authorize(
                &f.peer,
                &f.policy,
                &session,
                grant.metadata.namespace,
                MediaDirection::Publish,
                at(105)
            ),
            Err(MediaError::StaleGeneration)
        );
        let heartbeat = f
            .service
            .challenge(
                &f.peer,
                &f.policy,
                f.request(),
                GrantOperation::Heartbeat,
                at(110),
            )
            .unwrap();
        f.policy.0.epoch += 1;
        assert!(matches!(
            f.complete(&heartbeat, 110),
            Err(MediaError::Unauthorized)
        ));
        assert!(
            f.service
                .challenge(
                    &f.peer,
                    &f.policy,
                    f.request(),
                    GrantOperation::Heartbeat,
                    at(110)
                )
                .is_err()
        );
    }

    #[test]
    fn expired_presence_keeps_eight_granted_resource_slots_reserved() {
        let mut f = Fixture::new(Verb::Write);
        let first = f.grant(f.request(), GrantOperation::Join, 100);
        let session = f.relay(&first, 100);
        for seed in 10..17 {
            let device = SigningKey::from_bytes(&[seed; 32]);
            f.add_device(&device);
            f.device = device;
            f.grant(f.request(), GrantOperation::Join, 100);
        }
        assert!(
            f.service
                .presence_snapshot(&f.peer, &f.policy, &session, at(116))
                .unwrap()
                .is_empty()
        );
        let device = SigningKey::from_bytes(&[17; 32]);
        f.add_device(&device);
        f.device = device;
        assert!(matches!(
            f.service.challenge(
                &f.peer,
                &f.policy,
                f.request(),
                GrantOperation::Join,
                at(116)
            ),
            Err(MediaError::Capacity)
        ));
    }
    #[test]
    fn native_control_reservation_is_idempotent_and_leave_fences_old_intent() {
        let mut f = Fixture::new(Verb::Write);
        f.service = MediaAuthority::new(
            f.host.verifying_key().to_bytes(),
            "https://localhost:9443/moq/v21/voice",
        )
        .unwrap();
        let mut intent = f.request();
        intent.call_id = "ab".repeat(32);
        intent.generation = 0;
        let first = f
            .service
            .reserve_join(&f.peer, &f.policy, intent.clone(), at(100))
            .unwrap();
        assert_eq!(first.generation, 1);
        assert_eq!(
            f.service
                .reserve_join(&f.peer, &f.policy, intent.clone(), at(100))
                .unwrap(),
            first
        );
        let challenge = f
            .service
            .challenge(
                &f.peer,
                &f.policy,
                first.clone(),
                GrantOperation::Join,
                at(100),
            )
            .unwrap();
        assert_eq!(
            challenge.transcript.signing_digest().unwrap(),
            challenge.signing_digest
        );
        let grant = f.complete(&challenge, 100).unwrap();
        assert!(matches!(grant, GrantCompletion::Granted(_)));
        assert_eq!(
            f.service
                .reserve_join(&f.peer, &f.policy, intent.clone(), at(101))
                .unwrap(),
            first
        );
        let leave = f
            .service
            .challenge(&f.peer, &f.policy, first, GrantOperation::Leave, at(101))
            .unwrap();
        f.complete(&leave, 101).unwrap();
        assert!(matches!(
            f.service
                .reserve_join(&f.peer, &f.policy, intent.clone(), at(102)),
            Err(MediaError::StaleGeneration)
        ));
        intent.participation_id = [10; 32];
        assert_eq!(
            f.service
                .reserve_join(&f.peer, &f.policy, intent, at(102))
                .unwrap()
                .generation,
            2
        );
    }

    #[test]
    fn native_control_reservation_denies_direction_changes_and_generation_overflow() {
        let mut f = Fixture::new(Verb::Write);
        let mut intent = f.request();
        intent.generation = 0;
        let first = f
            .service
            .reserve_join(&f.peer, &f.policy, intent.clone(), at(100))
            .unwrap();
        intent.directions.publish = false;
        assert!(
            f.service
                .reserve_join(&f.peer, &f.policy, intent.clone(), at(100))
                .is_err()
        );
        let mut highest = first;
        highest.generation = u64::MAX;
        f.grant(highest, GrantOperation::Join, 100);
        intent.participation_id = [11; 32];
        assert!(
            f.service
                .reserve_join(&f.peer, &f.policy, intent, at(101))
                .is_err()
        );
    }
}

mod control;
