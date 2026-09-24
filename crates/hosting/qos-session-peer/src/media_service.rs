//! Host-private native authority bridge. No public capability is installed by
//! this module: durable call policy and native control routing are host-owned.
use crate::{
    SessionOwner,
    committed_authority::{CommittedAuthority, Observation},
    media_authority::*,
};
use ed25519_dalek::Signer;
use qos_moq::relay::{
    Admission, AuthorizationStatus, Denied, Direction, RelayAuthority, SessionAuthorization,
};
use qos_peer_state::StatePeer;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    sync::{
        Arc, Mutex, TryLockError, Weak,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};
use tilo_user_identity::canonical::{canonical_bytes, from_canonical_bytes};
use web_time::SystemTime;

mod authority;
pub use authority::{
    MediaAuthoritySource, MediaAuthorityTurn, MediaObservation, MediaSigningInput,
    NativeAuthoritySource, RoomMediaPolicy,
};

const MAX_SESSIONS: usize = 8;
const PROOF_BYTES: usize = 129;
/// Bound for synchronous waits on transient committed-authority contention.
const CONTENTION_WAIT: Duration = Duration::from_millis(100);
const CONTENTION_RETRY: Duration = Duration::from_micros(250);
#[cfg(test)]
thread_local! { static CURRENT_TURNS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) }; }
#[cfg(test)]
fn record_current_turn() {
    CURRENT_TURNS.with(|count| count.set(count.get() + 1));
}
#[derive(Serialize, Deserialize)]
struct Submission {
    version: u8,
    session: [u8; 32],
    nonce: [u8; 32],
    signature: [[u8; 32]; 2],
}
/// Untrusted wire submission, never evidence of verified identity.
pub(crate) fn encode_relay_proof(
    session: [u8; 32],
    nonce: [u8; 32],
    signature: [u8; 64],
) -> Result<Vec<u8>, MediaError> {
    let mut halves = [[0; 32]; 2];
    halves[0].copy_from_slice(&signature[..32]);
    halves[1].copy_from_slice(&signature[32..]);
    canonical_bytes(&Submission {
        version: 1,
        session,
        nonce,
        signature: halves,
    })
    .map_err(|_| MediaError::InvalidProof)
}
fn decode(bytes: &[u8]) -> Result<Submission, MediaError> {
    if bytes.len() != PROOF_BYTES {
        return Err(MediaError::InvalidProof);
    }
    let value: Submission = from_canonical_bytes(bytes).map_err(|_| MediaError::InvalidProof)?;
    if value.version != 1 || canonical_bytes(&value).map_err(|_| MediaError::InvalidProof)? != bytes
    {
        return Err(MediaError::InvalidProof);
    }
    Ok(value)
}
fn signature(value: &Submission) -> [u8; 64] {
    let mut bytes = [0; 64];
    bytes[..32].copy_from_slice(&value.signature[0]);
    bytes[32..].copy_from_slice(&value.signature[1]);
    bytes
}
fn namespace(value: MediaNamespace) -> String {
    format!(
        "{}/{}",
        hex::encode(value.call),
        hex::encode(value.publisher)
    )
}
fn parse_namespace(value: &str) -> Option<MediaNamespace> {
    if value.len() != 129
        || value.as_bytes()[64] != b'/'
        || !value
            .bytes()
            .enumerate()
            .all(|(i, b)| i == 64 || b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return None;
    }
    let mut call = [0; 32];
    let mut publisher = [0; 32];
    hex::decode_to_slice(&value[..64], &mut call).ok()?;
    hex::decode_to_slice(&value[65..], &mut publisher).ok()?;
    Some(MediaNamespace { call, publisher })
}
struct Lease {
    deadline_ns: AtomicU64,
}
struct Slot {
    session: RelaySession,
    alive: Weak<Lease>,
}
struct Core {
    inventories: BTreeMap<[u8; 32], (u64, Vec<MediaNamespace>)>,
    continuity: u64,
    authority: MediaAuthority,
    sessions: BTreeMap<[u8; 32], Slot>,
}
struct Shared<S: MediaAuthoritySource> {
    source: S,
    host_certificate: tilo_user_identity::DeviceCert,
    workspace: u64,
    core: Mutex<Core>,
    host: [u8; 32],
    audience: String,
    room: String,
    clock: Instant,
}
pub struct MediaService<S: MediaAuthoritySource = NativeAuthoritySource>(Arc<Shared<S>>);
impl<S: MediaAuthoritySource> Clone for MediaService<S> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}
pub(crate) struct RelayChallenge {
    pub session_id: [u8; 32],
    pub challenge: MediaChallenge,
}
/// A subscribing session is scoped to its own call component and observed
/// against every namespace the relay currently offers beneath it, so a roster
/// change needs no new session. Only publishers admitted into the same call
/// can publish there, and each offered namespace is re-authorized every
/// relay observation.
struct Permit<S: MediaAuthoritySource> {
    shared: Arc<Shared<S>>,
    continuity: u64,
    id: [u8; 32],
    call: [u8; 32],
    publish: Option<String>,
    subscribe_prefix: Option<String>,
    lease: Arc<Lease>,
    /// Logs one content-free reason per contention episode, not per tick.
    unavailable: std::sync::atomic::AtomicBool,
}

impl MediaService {
    pub(crate) fn for_owner(
        owner: &SessionOwner,
        committed: CommittedAuthority,
        audience: &str,
    ) -> Result<Self, MediaError> {
        if !owner.permits_publication() {
            return Err(MediaError::Unauthorized);
        }
        Self::from_source(NativeAuthoritySource::new(owner, committed), audience)
    }
}
impl<S: MediaAuthoritySource> MediaService<S> {
    /// Construction alone is not a capability/installation receipt. The owner
    /// supplies a live guarded source; the configured origin is checked again
    /// against the original authenticated HTTP binding during control.
    pub fn from_source(source: S, audience: &str) -> Result<Self, MediaError> {
        let MediaObservation::Current((room, certificate, workspace, continuity)) = source
            .with_current(|turn| {
                (
                    turn.peer().room().to_owned(),
                    turn.peer().authority().clone(),
                    turn.workspace(),
                    turn.continuity(),
                )
            })
        else {
            return Err(MediaError::Unauthorized);
        };
        let host = certificate.device_public_key;
        let authority = MediaAuthority::new(host, audience)?;
        Ok(Self(Arc::new(Shared {
            source,
            host_certificate: certificate,
            workspace,
            core: Mutex::new(Core {
                inventories: BTreeMap::new(),
                continuity,
                authority,
                sessions: BTreeMap::new(),
            }),
            host,
            audience: audience.into(),
            room,
            clock: Instant::now(),
        })))
    }
    fn turn<T>(
        &self,
        action: impl FnOnce(
            &StatePeer,
            &dyn MediaCallPolicy,
            &mut Core,
            SystemTime,
        ) -> Result<T, MediaError>,
    ) -> Result<T, MediaError> {
        self.turn_observed(action, true)
            .map_err(|failure| match failure {
                TurnFailure::Busy => {
                    tracing::debug!(reason = "owner-contended", "media turn unavailable");
                    MediaError::Unauthorized
                }
                TurnFailure::Media(error) => error,
            })
    }
    /// Like `turn`, but distinguishes transient contention (the committed
    /// authority or media core was held by another reader/commit) from a
    /// decision. `Busy` is only returned before `action` runs.
    fn turn_classified<T>(
        &self,
        action: impl FnOnce(
            &StatePeer,
            &dyn MediaCallPolicy,
            &mut Core,
            SystemTime,
        ) -> Result<T, MediaError>,
    ) -> Result<T, TurnFailure> {
        self.turn_observed(action, false)
    }
    /// Reader contention on the committed authority (another request or a
    /// relay observation holding it for a moment, or an owner commit in
    /// flight) is transient. Synchronous callers wait a short bound for a
    /// Current view instead of converting contention into a denial; the
    /// asynchronous relay admission retries on `Busy` itself.
    pub(crate) fn with_current_patient<R>(
        &self,
        action: impl FnOnce(&S::Turn<'_>) -> R,
    ) -> MediaObservation<R> {
        let deadline = Instant::now() + CONTENTION_WAIT;
        let mut action = Some(action);
        loop {
            let observed = self
                .0
                .source
                .with_current(|turn| (action.take().expect("observed once"))(turn));
            match observed {
                MediaObservation::Busy if action.is_some() && Instant::now() < deadline => {
                    std::thread::sleep(CONTENTION_RETRY);
                }
                other => return other,
            }
        }
    }
    fn turn_observed<T>(
        &self,
        action: impl FnOnce(
            &StatePeer,
            &dyn MediaCallPolicy,
            &mut Core,
            SystemTime,
        ) -> Result<T, MediaError>,
        patient: bool,
    ) -> Result<T, TurnFailure> {
        let observe = |turn: &S::Turn<'_>| self.turn_in_classified(turn, action);
        let observed = if patient {
            self.with_current_patient(observe)
        } else {
            self.0.source.with_current(observe)
        };
        match observed {
            MediaObservation::Current(result) => result,
            MediaObservation::Busy => Err(TurnFailure::Busy),
            MediaObservation::Denied => {
                tracing::debug!(reason = "owner-denied", "media turn unavailable");
                Err(TurnFailure::Media(MediaError::Unauthorized))
            }
        }
    }
    fn turn_in<T>(
        &self,
        turn: &S::Turn<'_>,
        action: impl FnOnce(
            &StatePeer,
            &dyn MediaCallPolicy,
            &mut Core,
            SystemTime,
        ) -> Result<T, MediaError>,
    ) -> Result<T, MediaError> {
        self.turn_in_classified(turn, action)
            .map_err(|failure| match failure {
                TurnFailure::Busy => MediaError::Unauthorized,
                TurnFailure::Media(error) => error,
            })
    }
    fn turn_in_classified<T>(
        &self,
        turn: &S::Turn<'_>,
        action: impl FnOnce(
            &StatePeer,
            &dyn MediaCallPolicy,
            &mut Core,
            SystemTime,
        ) -> Result<T, MediaError>,
    ) -> Result<T, TurnFailure> {
        if !self.0.matches(turn) {
            tracing::debug!(reason = "owner-mismatch", "media turn unavailable");
            return Err(TurnFailure::Media(MediaError::Unauthorized));
        }
        let mut core = match self.0.core.try_lock() {
            Ok(core) => core,
            Err(TryLockError::WouldBlock) => return Err(TurnFailure::Busy),
            Err(TryLockError::Poisoned(_)) => {
                return Err(TurnFailure::Media(MediaError::Unauthorized));
            }
        };
        if core.continuity != turn.continuity() {
            let authority =
                MediaAuthority::new(self.0.host, &self.0.audience).map_err(TurnFailure::Media)?;
            *core = Core {
                inventories: BTreeMap::new(),
                continuity: turn.continuity(),
                authority,
                sessions: BTreeMap::new(),
            };
        }
        action(
            turn.peer(),
            turn.policy(),
            &mut core,
            SystemTime::UNIX_EPOCH + Duration::from_nanos(turn.now_ns()),
        )
        .map_err(TurnFailure::Media)
    }
    pub(crate) fn challenge(
        &self,
        request: GrantRequest,
        operation: GrantOperation,
    ) -> Result<MediaChallenge, MediaError> {
        self.turn(|peer, policy, core, now| {
            core.authority
                .challenge(peer, policy, request, operation, now)
        })
    }
    pub(crate) fn relay_challenge(&self, grant: &[u8]) -> Result<RelayChallenge, MediaError> {
        let mut session_id = [0; 32];
        rand::rngs::OsRng
            .try_fill_bytes(&mut session_id)
            .map_err(|_| MediaError::EntropyUnavailable)?;
        self.turn(|peer, policy, core, now| {
            Ok(RelayChallenge {
                session_id,
                challenge: core
                    .authority
                    .relay_challenge(peer, policy, grant, session_id, now)?,
            })
        })
    }
    pub(crate) fn refresh_challenge(
        &self,
        id: [u8; 32],
        grant: &[u8],
    ) -> Result<MediaChallenge, MediaError> {
        self.turn(|peer, policy, core, now| {
            let slot = core.sessions.get(&id).ok_or(MediaError::StaleGeneration)?;
            core.authority
                .refresh_challenge(peer, policy, &slot.session, grant, now)
        })
    }
    pub(crate) fn refresh(
        &self,
        id: [u8; 32],
        nonce: [u8; 32],
        proof: [u8; 64],
    ) -> Result<(), MediaError> {
        self.turn(|peer, policy, core, now| {
            let slot = core
                .sessions
                .get_mut(&id)
                .ok_or(MediaError::StaleGeneration)?;
            let alive = slot.alive.upgrade().ok_or(MediaError::StaleGeneration)?;
            let session =
                core.authority
                    .refresh_relay(peer, policy, &slot.session, nonce, proof, now)?;
            alive.deadline_ns.store(
                self.0.deadline(session.metadata().expires_at_ns, now)?,
                Ordering::Release,
            );
            slot.session = session;
            Ok(())
        })
    }
    /// Trusted native-control close. No public bearer close-by-ID endpoint is
    /// exposed; the eventual control router must bind caller and session.
    pub(crate) fn close(&self, id: [u8; 32]) -> Result<(), MediaError> {
        self.turn(|_, _, core, _| {
            let slot = core
                .sessions
                .remove(&id)
                .ok_or(MediaError::StaleGeneration)?;
            core.authority.close_relay(&slot.session)
        })
    }
}
impl MediaService {
    /// Called only by the serialized owner control turn. The key never leaves
    /// native custody and is requested only after device proof validation.
    pub(crate) fn complete(
        &self,
        owner: &SessionOwner,
        nonce: [u8; 32],
        proof: [u8; 64],
    ) -> Result<GrantCompletion, MediaError> {
        if !owner.permits_publication()
            || owner.peer().room() != self.0.room
            || owner.identity.native.verifying_key().to_bytes() != self.0.host
        {
            return Err(MediaError::Unauthorized);
        }
        self.turn(|peer, policy, core, now| {
            core.authority
                .complete(peer, policy, nonce, proof, now, |digest| {
                    Ok(owner.identity.native.sign(digest).to_bytes())
                })
        })
    }
    /// Same lock order as reads; the projection is unavailable throughout the
    /// owner turn. Publish the owner's actual durability state even when an
    /// operation rejects input without changing the committed generation.
    pub(crate) fn owner_update<T>(
        &self,
        owner: &mut SessionOwner,
        update: impl FnOnce(&mut SessionOwner) -> anyhow::Result<T>,
    ) -> anyhow::Result<T> {
        anyhow::ensure!(
            owner.peer().room() == self.0.room
                && owner.peer().authority().device_public_key == self.0.host,
            "media owner mismatch"
        );
        let update_guard = self.0.source.committed.begin(owner)?;
        let result = update(owner);
        // Validation rejection is not a failed durability barrier. Publish
        // according to the real owner's health and retain the operation error.
        update_guard.publish(owner)?;
        result
    }
    pub(crate) fn invalidate(&self) {
        self.0.source.committed.invalidate();
    }
}
impl<S: MediaAuthoritySource> Shared<S> {
    fn matches(&self, turn: &S::Turn<'_>) -> bool {
        turn.peer().room() == self.room
            && turn.peer().authority() == &self.host_certificate
            && turn.workspace() == self.workspace
    }
    fn deadline(&self, expires: u64, _validated_at: SystemTime) -> Result<u64, MediaError> {
        // Pair the monotonic observation before the wall clock sample, after
        // verification: validation time can never extend the signed expiry.
        let observed = Instant::now();
        let now_ns: u64 = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_err(|_| MediaError::Expired)?
            .as_nanos()
            .try_into()
            .map_err(|_| MediaError::Expired)?;
        let remaining = expires.checked_sub(now_ns).ok_or(MediaError::Expired)?;
        let elapsed: u64 = observed
            .duration_since(self.clock)
            .as_nanos()
            .try_into()
            .map_err(|_| MediaError::Expired)?;
        elapsed.checked_add(remaining).ok_or(MediaError::Expired)
    }
}
enum TurnFailure {
    Busy,
    Media(MediaError),
}
impl<S: MediaAuthoritySource> RelayAuthority for MediaService<S> {
    fn admit(&self, bytes: &[u8]) -> Result<Arc<dyn SessionAuthorization>, Denied> {
        self.try_admit(bytes).map_err(|_| Denied)
    }
    fn try_admit(&self, bytes: &[u8]) -> Result<Arc<dyn SessionAuthorization>, Admission> {
        let submission = decode(bytes).map_err(|_| {
            tracing::debug!(reason = "undecodable", "media relay admission denied");
            Admission::Denied
        })?;
        // Busy is reported only before the turn runs, i.e. before the
        // single-use relay challenge is taken; the relay may then retry.
        self.turn_classified(|peer, policy, core, now| {
            let stale: Vec<_> = core
                .sessions
                .iter()
                .filter(|(_, slot)| slot.alive.strong_count() == 0)
                .map(|(id, _)| *id)
                .collect();
            for id in stale {
                if let Some(slot) = core.sessions.remove(&id) {
                    let _ = core.authority.close_relay(&slot.session);
                }
            }
            if core.sessions.len() >= MAX_SESSIONS {
                return Err(MediaError::Capacity);
            }
            let session = core.authority.authenticate_relay(
                peer,
                policy,
                submission.session,
                submission.nonce,
                signature(&submission),
                now,
            )?;
            let publish = session
                .metadata()
                .request
                .directions
                .publish
                .then(|| namespace(session.metadata().namespace));
            let call = session.metadata().namespace.call;
            let subscribe_prefix = session
                .metadata()
                .request
                .directions
                .subscribe
                .then(|| hex::encode(call));
            let lease = Arc::new(Lease {
                deadline_ns: AtomicU64::new(
                    self.0.deadline(session.metadata().expires_at_ns, now)?,
                ),
            });
            core.sessions.insert(
                submission.session,
                Slot {
                    session,
                    alive: Arc::downgrade(&lease),
                },
            );
            Ok(Arc::new(Permit {
                shared: self.0.clone(),
                continuity: core.continuity,
                id: submission.session,
                call,
                publish,
                subscribe_prefix,
                lease,
                unavailable: std::sync::atomic::AtomicBool::new(false),
            }) as Arc<dyn SessionAuthorization>)
        })
        .map_err(|failure| match failure {
            TurnFailure::Busy => Admission::Busy,
            TurnFailure::Media(error) => {
                // Content-free reason only; never the proof, namespace or grant.
                tracing::debug!(reason = ?error, "media relay admission denied");
                Admission::Denied
            }
        })
    }
}
impl<S: MediaAuthoritySource> Permit<S> {
    /// An offered subscribe namespace: exact, syntactically valid, in this
    /// session's own call, and never its own publication.
    fn offered(&self, name: &str) -> Option<MediaNamespace> {
        self.subscribe_prefix.as_ref()?;
        parse_namespace(name).filter(|namespace| {
            namespace.call == self.call && self.publish.as_deref() != Some(name)
        })
    }
    fn contended(&self, reason: &'static str, expires_at: Instant) -> AuthorizationStatus {
        if !self.unavailable.swap(true, Ordering::Relaxed) {
            tracing::debug!(reason, "media relay observation unavailable");
        }
        AuthorizationStatus::Unavailable { expires_at }
    }
    /// One guarded turn over this session's own authority, its publication and
    /// `offered`. With `lapsed`, an offered namespace whose publisher authority
    /// lapsed is reported there instead of denying this session; this
    /// session's own authority is always checked first in the same turn.
    fn observe(
        &self,
        publication: bool,
        offered: &[&str],
        mut lapsed: Option<&mut Vec<String>>,
    ) -> AuthorizationStatus {
        let expires_at = self.shared.clock
            + Duration::from_nanos(self.lease.deadline_ns.load(Ordering::Acquire));
        // Content-free denial reasons (debug) diagnose relay session aborts on
        // impaired links without logging namespaces, proofs or identities.
        if Instant::now() >= expires_at {
            tracing::debug!(reason = "lease-expired", "media relay observation denied");
            return AuthorizationStatus::Denied;
        }
        let observed = self.shared.source.with_current(|turn| {
            if !self.shared.matches(turn) || turn.continuity() != self.continuity {
                tracing::debug!(
                    reason = "owner-continuity",
                    "media relay observation denied"
                );
                return AuthorizationStatus::Denied;
            }
            let core = match self.shared.core.try_lock() {
                Ok(core) => core,
                Err(TryLockError::WouldBlock) => {
                    return self.contended("core-contended", expires_at);
                }
                Err(_) => return AuthorizationStatus::Denied,
            };
            let Some(slot) = core.sessions.get(&self.id) else {
                tracing::debug!(reason = "session-closed", "media relay observation denied");
                return AuthorizationStatus::Denied;
            };
            let now = SystemTime::UNIX_EPOCH + Duration::from_nanos(turn.now_ns());
            let (peer, policy) = (turn.peer(), turn.policy());
            if core
                .authority
                .authorize_session(peer, policy, &slot.session, now)
                .is_err()
            {
                tracing::debug!(
                    reason = "session-unauthorized",
                    "media relay observation denied"
                );
                return AuthorizationStatus::Denied;
            }
            if publication
                && let Some(name) = self.publish.as_deref()
                && parse_namespace(name).is_none_or(|namespace| {
                    core.authority
                        .authorize(
                            peer,
                            policy,
                            &slot.session,
                            namespace,
                            MediaDirection::Publish,
                            now,
                        )
                        .is_err()
                })
            {
                tracing::debug!(
                    reason = "publication-unauthorized",
                    "media relay observation denied"
                );
                return AuthorizationStatus::Denied;
            }
            for name in offered {
                let Some(namespace) = self.offered(name) else {
                    tracing::debug!(reason = "scope-invalid", "media relay observation denied");
                    return AuthorizationStatus::Denied;
                };
                if core
                    .authority
                    .authorize(
                        peer,
                        policy,
                        &slot.session,
                        namespace,
                        MediaDirection::Subscribe,
                        now,
                    )
                    .is_err()
                {
                    // Own authority is current in this same turn, so only the
                    // remote publisher's authority lapsed.
                    let Some(lapsed) = lapsed.as_deref_mut() else {
                        tracing::debug!(
                            reason = "scope-unauthorized",
                            "media relay observation denied"
                        );
                        return AuthorizationStatus::Denied;
                    };
                    tracing::debug!(
                        reason = "publisher-lapsed",
                        "media relay offered scope withdrawn"
                    );
                    lapsed.push((*name).to_owned());
                }
            }
            self.unavailable.store(false, Ordering::Relaxed);
            AuthorizationStatus::Current
        });
        // A slow restore cannot carry a successful observation past its lease.
        if Instant::now() >= expires_at {
            tracing::debug!(reason = "lease-expired", "media relay observation denied");
            return AuthorizationStatus::Denied;
        }
        match observed {
            MediaObservation::Current(status) => status,
            MediaObservation::Busy => self.contended("owner-updating", expires_at),
            MediaObservation::Denied => {
                tracing::debug!(reason = "owner-denied", "media relay observation denied");
                AuthorizationStatus::Denied
            }
        }
    }
}
impl<S: MediaAuthoritySource> SessionAuthorization for Permit<S> {
    fn publication(&self) -> Option<&str> {
        self.publish.as_deref()
    }
    fn subscriptions(&self) -> &[String] {
        &[]
    }
    fn subscription_prefix(&self) -> Option<&str> {
        self.subscribe_prefix.as_deref()
    }
    fn current(&self, direction: Direction, name: &str) -> bool {
        matches!(self.status(direction, name), AuthorizationStatus::Current)
    }
    fn status(&self, direction: Direction, name: &str) -> AuthorizationStatus {
        // Definite local denial precedes temporary owner/core contention.
        // Only an exact, syntactically valid scope may use the outage grace.
        match direction {
            Direction::Publish if self.publish.as_deref() == Some(name) => {
                self.observe(true, &[], None)
            }
            Direction::Subscribe if self.offered(name).is_some() => {
                self.observe(false, &[name], None)
            }
            _ => AuthorizationStatus::Denied,
        }
    }
    fn status_all(&self) -> AuthorizationStatus {
        self.observe(true, &[], None)
    }
    fn status_offered(&self, offered: &[&str], lapsed: &mut Vec<String>) -> AuthorizationStatus {
        self.observe(true, offered, Some(lapsed))
    }
}
#[cfg(test)]
mod client_tests;
#[cfg(test)]
mod native_control_tests;
#[cfg(test)]
mod tests;

mod control;
