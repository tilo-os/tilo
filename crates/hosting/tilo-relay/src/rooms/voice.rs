//! Calls in the rooms this relay governs.
//!
//! A call is room governance: [`RoomMediaV1`] in the room document, which the
//! governor authors and every member verifies. Any member opens a voice
//! channel's call to join it ([`CallOperation::Open`]): the relay, as the
//! governor, starts the channel's call when it has none, and restarts it
//! when it was ended. The relay's own user, and any member the room's
//! governance grants its `call` action (no role preset below the owner's
//! does), ends and moderates calls; any member device joins an active one
//! and gets a grant for the relay's `moq-lite` voice carrier (see
//! [`tilo_relay_protocol::voice`]).
//!
//! A call has no participant limit. A device takes part in one call at a
//! time: joining another call ends its earlier participation, so a call's
//! participants, and the carrier's sessions, are bounded by the room's member
//! devices, not by a constant.
//!
//! What a participation may do is the room's current policy for its device:
//! the call is active, the device is a member, and neither the call's
//! directions nor a denial of its person exclude it. It is re-derived after
//! every governance decision and every second, and the carrier asks the
//! resulting snapshot on each of its observations, so removing a member or
//! ending a call closes their voice within one carrier observation of the
//! decision.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use ed25519_dalek::SigningKey;
#[cfg(feature = "web")]
use tilo_moq::relay::{
    AuthorizationStatus, Denied, Direction, RelayAuthority, SessionAuthorization,
};
use tilo_peer_state::room::media::{self, CallPhaseV1, DirectionsV1, RoomMediaV1};
use tilo_peer_state::{GovernedAccess, StatePeer};
use tilo_relay_protocol::voice::{
    self as protocol, CallOperation, CallState, CallSummary, Directions, PARTICIPATION_TTL,
    VoiceGrant, VoiceParticipant,
};
use tilo_relay_protocol::{AuthenticatedPeer, Envelope};
use tokio::sync::mpsc;
use web_time::SystemTime;

use tilo_action_policy::roles::governance;

use super::authority::RoomOp;
use super::{GovernedRoom, Rooms};
use tilo_lock::MutexRecover as _;

/// How long a grant's token admits new carrier sessions.
const GRANT_TTL: Duration = Duration::from_secs(60);
/// A heartbeat renews a token with less than this left.
const GRANT_RENEWAL: Duration = Duration::from_secs(30);
/// How often participations are checked for silence, closed connections and
/// the passage of time (certificate expiry).
pub(super) const OBSERVATION: Duration = Duration::from_secs(1);

/// Where the relay serves its voice carrier.
#[derive(Clone, Debug)]
struct Carrier {
    endpoint: String,
    fingerprints: Vec<String>,
}

/// One device in one call.
struct Participation {
    room: String,
    workspace_id: u64,
    call: [u8; 32],
    epoch: u64,
    device: [u8; 32],
    user: [u8; 32],
    directions: Directions,
    seen: Instant,
    outbound: mpsc::Sender<Envelope>,
    token: [u8; 32],
    token_expires: Instant,
    token_expires_ns: u64,
}

#[derive(Default)]
struct State {
    participations: HashMap<[u8; 32], Participation>,
    tokens: HashMap<[u8; 32], [u8; 32]>,
}

impl State {
    fn in_call(&self, call: &[u8; 32]) -> impl Iterator<Item = (&[u8; 32], &Participation)> {
        self.participations
            .iter()
            .filter(move |(_, participation)| participation.call == *call)
    }

    fn remove(&mut self, id: &[u8; 32]) -> Option<Participation> {
        let participation = self.participations.remove(id)?;
        self.tokens.remove(&participation.token);
        Some(participation)
    }

    fn renew_token(&mut self, id: [u8; 32]) {
        let Some(participation) = self.participations.get_mut(&id) else {
            return;
        };
        self.tokens.remove(&participation.token);
        participation.token = rand::random();
        let now = Instant::now();
        participation.token_expires = now.checked_add(GRANT_TTL).unwrap_or(now);
        participation.token_expires_ns = tilo_clock::unix_now_ns()
            .saturating_add(u64::try_from(GRANT_TTL.as_nanos()).unwrap_or(u64::MAX));
        self.tokens.insert(participation.token, id);
    }
}

/// The calls of every room this relay governs.
#[derive(Default)]
pub(super) struct Calls {
    carrier: OnceLock<Carrier>,
    state: Mutex<State>,
}

impl Calls {
    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock_recover()
    }

    fn grant(&self, state: &State, id: &[u8; 32]) -> Option<VoiceGrant> {
        let carrier = self.carrier.get()?;
        let own = state.participations.get(id)?;
        let mut participants: Vec<VoiceParticipant> = state
            .in_call(&own.call)
            .map(|(other, participation)| VoiceParticipant {
                device_public_key: participation.device,
                user_public_key: participation.user,
                namespace: protocol::namespace(&participation.call, other),
                publishes: participation.directions.publish,
            })
            .collect();
        participants.sort_by(|a, b| a.namespace.cmp(&b.namespace));
        let namespace = protocol::namespace(&own.call, id);
        let subscriptions = if own.directions.subscribe {
            participants
                .iter()
                .filter(|participant| participant.publishes && participant.namespace != namespace)
                .map(|participant| participant.namespace.clone())
                .collect()
        } else {
            Vec::new()
        };
        Some(VoiceGrant {
            call: own.call,
            epoch: own.epoch,
            participation: *id,
            token: own.token.to_vec(),
            endpoint: carrier.endpoint.clone(),
            fingerprints: carrier.fingerprints.clone(),
            directions: own.directions,
            publication: own.directions.publish.then_some(namespace),
            subscriptions,
            participants,
            expires_at_ns: own.token_expires_ns,
        })
    }

    /// Send every participant of `calls` its current grant.
    fn announce(&self, state: &State, calls: &[[u8; 32]]) {
        for (id, participation) in &state.participations {
            if !calls.contains(&participation.call) {
                continue;
            }
            if let Some(grant) = self.grant(state, id) {
                // A participant whose queue is full hears the roster at its
                // next heartbeat.
                let _ = participation.outbound.try_send(Envelope::VoiceGranted {
                    workspace_id: participation.workspace_id,
                    grant: Box::new(grant),
                });
            }
        }
    }

    /// Re-derive every participation in `room` from its committed policy:
    /// refuse those that may no longer take part, update the others, and
    /// send the participants of every call that changed their new grants.
    fn observe(&self, room: &GovernedRoom) {
        let policy = Policy::read(room.peer());
        let mut state = self.state();
        let mut changed = Vec::new();
        let mut refused = Vec::new();
        for (id, participation) in &mut state.participations {
            if participation.room != room.room() {
                continue;
            }
            let current = policy.as_ref().map_err(Clone::clone).and_then(|policy| {
                current(
                    policy,
                    participation.device,
                    participation.user,
                    &participation.call,
                )
            });
            match current {
                Ok((epoch, directions)) => {
                    if (epoch, directions) != (participation.epoch, participation.directions) {
                        participation.epoch = epoch;
                        participation.directions = directions;
                        changed.push(participation.call);
                    }
                }
                Err(reason) => refused.push((*id, reason)),
            }
        }
        for (id, reason) in refused {
            if let Some(participation) = state.remove(&id) {
                let _ = participation.outbound.try_send(Envelope::VoiceRefused {
                    workspace_id: participation.workspace_id,
                    call: participation.call,
                    reason,
                });
                changed.push(participation.call);
            }
        }
        changed.sort_unstable();
        changed.dedup();
        self.announce(&state, &changed);
    }

    /// End participations not heard from for [`PARTICIPATION_TTL`] or whose
    /// connection closed. Returns the rooms that still have participants.
    fn expire(&self) -> Vec<String> {
        let mut state = self.state();
        let now = Instant::now();
        let gone: Vec<[u8; 32]> = state
            .participations
            .iter()
            .filter(|(_, participation)| {
                participation.outbound.is_closed()
                    || now.duration_since(participation.seen) > PARTICIPATION_TTL
            })
            .map(|(id, _)| *id)
            .collect();
        let mut changed: Vec<[u8; 32]> = gone
            .iter()
            .filter_map(|id| state.remove(id).map(|participation| participation.call))
            .collect();
        changed.sort_unstable();
        changed.dedup();
        self.announce(&state, &changed);
        let mut rooms: Vec<String> = state
            .participations
            .values()
            .map(|participation| participation.room.clone())
            .collect();
        drop(state);
        rooms.sort_unstable();
        rooms.dedup();
        rooms
    }

    /// The participation, call and publishing of an unexpired `token`.
    #[cfg(feature = "web")]
    fn admission(&self, token: &[u8; 32]) -> Option<([u8; 32], [u8; 32], bool)> {
        let state = self.state();
        let id = *state.tokens.get(token)?;
        state
            .participations
            .get(&id)
            .filter(|participation| Instant::now() < participation.token_expires)
            .map(|participation| (id, participation.call, participation.directions.publish))
    }

    /// Whether participation `id` may use `direction` on `namespace` now.
    #[cfg(feature = "web")]
    fn permits(&self, id: &[u8; 32], direction: Direction, namespace: &str) -> bool {
        let state = self.state();
        let Some(own) = state.participations.get(id) else {
            return false;
        };
        match direction {
            Direction::Publish => {
                own.directions.publish && namespace == protocol::namespace(&own.call, id)
            }
            Direction::Subscribe => {
                let Some(publisher) = parse_namespace(&own.call, namespace) else {
                    return false;
                };
                own.directions.subscribe
                    && state
                        .participations
                        .get(&publisher)
                        .is_some_and(|other| other.call == own.call && other.directions.publish)
            }
        }
    }
}

/// The participation publishing `namespace` in `call`.
#[cfg(any(feature = "web", test))]
fn parse_namespace(call: &[u8; 32], namespace: &str) -> Option<[u8; 32]> {
    let (prefix, publisher) = namespace.split_once('/')?;
    if prefix != hex::encode(call) {
        return None;
    }
    let mut id = [0; 32];
    hex::decode_to_slice(publisher, &mut id).ok()?;
    Some(id)
}

/// The room's committed call policy and who it admits, read once per
/// observation.
struct Policy {
    calls: Option<RoomMediaV1>,
    access: GovernedAccess,
}

impl Policy {
    fn read(peer: &StatePeer) -> Result<Self, String> {
        Ok(Self {
            calls: peer.validated_media_policy()?,
            access: peer.governance().governed_access(SystemTime::now())?,
        })
    }
}

/// What `device` of person `user` may do in `call` under `policy`: the
/// call's epoch and directions, or why it may not take part. A person's
/// every certified device takes part, as the relay admits them all to the
/// room; the relay verified the certificate when the device connected.
fn current(
    policy: &Policy,
    device: [u8; 32],
    user: [u8; 32],
    call: &[u8; 32],
) -> Result<(u64, Directions), String> {
    let call = policy
        .calls
        .as_ref()
        .and_then(|calls| calls.calls.get(call))
        .ok_or("the room has no such call")?;
    if call.phase != CallPhaseV1::Active {
        return Err("the call has ended".into());
    }
    if !policy.access.member_devices.contains(&device)
        && !policy.access.member_user_roots.contains(&user)
    {
        return Err("not a member of the room".into());
    }
    let denied = call.denied.get(&user);
    Ok((
        call.epoch,
        Directions {
            publish: call.allowed.publish && !denied.is_some_and(|value| value.publish),
            subscribe: call.allowed.subscribe && !denied.is_some_and(|value| value.subscribe),
        },
    ))
}

const fn directions(value: Directions) -> DirectionsV1 {
    DirectionsV1 {
        publish: value.publish,
        subscribe: value.subscribe,
    }
}

const fn from_v1(value: DirectionsV1) -> Directions {
    Directions {
        publish: value.publish,
        subscribe: value.subscribe,
    }
}

/// Apply `operation` to the room policy of `candidate`, authored by the
/// governor. Returns the call it changed. Moderation that changes nothing
/// leaves the room unchanged.
fn transition(
    candidate: &mut StatePeer,
    governor: &SigningKey,
    operation: &CallOperation,
    now: SystemTime,
) -> Result<[u8; 32], String> {
    let current = media::parse(candidate.admitted_room().ok_or("missing room")?)
        .map_err(|error| error.to_string())?;
    let mut policy = match current.clone() {
        Some(policy) => policy,
        None => RoomMediaV1 {
            version: 1,
            room: *uuid::Uuid::parse_str(candidate.room())
                .map_err(|error| error.to_string())?
                .as_bytes(),
            authorization_revision: 1,
            authorization_basis: media::security_basis(
                candidate.admitted_room().ok_or("missing room")?,
                &candidate.revocations(),
            )
            .map_err(|error| error.to_string())?,
            calls: BTreeMap::default(),
        },
    };
    let id = match operation {
        CallOperation::Open {
            app_id,
            record_id,
            channel_id,
        } => {
            let binding = candidate
                .media()
                .resolve_media_binding(app_id, record_id, channel_id)?;
            open(&mut policy, binding)?
        }
        CallOperation::Start {
            app_id,
            record_id,
            channel_id,
            allowed,
        } => {
            let binding = candidate
                .media()
                .resolve_media_binding(app_id, record_id, channel_id)?;
            start(&mut policy, binding, *allowed)?
        }
        CallOperation::End { call, epoch }
        | CallOperation::Restart { call, epoch }
        | CallOperation::SetDirections { call, epoch, .. }
        | CallOperation::SetDenial { call, epoch, .. } => {
            moderate(candidate, &mut policy, operation, call, *epoch)?;
            *call
        }
    };
    if current.as_ref() != Some(&policy) {
        candidate
            .media_mut()
            .author_media_policy(&policy, governor, now)?;
    }
    Ok(id)
}

/// The call of `binding`'s channel in `policy`, active: its active call as
/// it is, its ended call restarted, or a new call where participants publish
/// and subscribe.
fn open(policy: &mut RoomMediaV1, binding: media::ChatCallBindingV1) -> Result<[u8; 32], String> {
    let existing = policy
        .calls
        .iter_mut()
        .find(|(_, call)| call.binding.same_channel(&binding));
    if let Some((id, call)) = existing {
        if call.phase != CallPhaseV1::Active {
            // A call's binding never changes; it names the channel by its
            // id, which the current resolution shares.
            call.phase = CallPhaseV1::Active;
            call.epoch = media::next_counter(call.epoch).map_err(|error| error.to_string())?;
        }
        return Ok(*id);
    }
    start(
        policy,
        binding,
        Directions {
            publish: true,
            subscribe: true,
        },
    )
}

/// Add an active call on `binding` to `policy`.
fn start(
    policy: &mut RoomMediaV1,
    binding: media::ChatCallBindingV1,
    allowed: Directions,
) -> Result<[u8; 32], String> {
    if policy
        .calls
        .values()
        .any(|call| call.binding.same_channel(&binding))
    {
        return Err("that channel already has a call; restart it".into());
    }
    if policy.calls.len() >= media::MAX_CALLS {
        return Err("the room has no room for another call".into());
    }
    let id = loop {
        let id: [u8; 32] = rand::random();
        if id != [0; 32] && !policy.calls.contains_key(&id) {
            break id;
        }
    };
    policy.calls.insert(
        id,
        media::CallV1 {
            binding,
            epoch: 1,
            phase: CallPhaseV1::Active,
            allowed: directions(allowed),
            denied: BTreeMap::default(),
        },
    );
    Ok(id)
}

/// Apply moderation `operation` to `call` in `policy`, if the caller saw its
/// current `epoch`. A change advances the epoch.
fn moderate(
    candidate: &StatePeer,
    policy: &mut RoomMediaV1,
    operation: &CallOperation,
    call: &[u8; 32],
    epoch: u64,
) -> Result<(), String> {
    let total_denials = policy
        .calls
        .values()
        .map(|call| call.denied.len())
        .sum::<usize>();
    let entry = policy.calls.get_mut(call).ok_or("unknown call")?;
    if epoch != entry.epoch {
        return Err("the call changed since that epoch".into());
    }
    let changed = match operation {
        CallOperation::Open { .. } | CallOperation::Start { .. } => {
            return Err("not a moderation".into());
        }
        CallOperation::End { .. } => {
            if entry.phase != CallPhaseV1::Active {
                return Err("the call already ended".into());
            }
            entry.phase = CallPhaseV1::Ended;
            true
        }
        CallOperation::Restart { .. } => {
            if entry.phase != CallPhaseV1::Ended {
                return Err("the call is active".into());
            }
            let binding = candidate.media().resolve_media_binding(
                &entry.binding.app_id,
                &entry.binding.record_id,
                &entry.binding.channel_id,
            )?;
            if !binding.same_channel(&entry.binding) {
                return Err("the call's channel changed".into());
            }
            entry.phase = CallPhaseV1::Active;
            true
        }
        CallOperation::SetDirections { allowed, .. } => {
            let allowed = directions(*allowed);
            let changed = entry.allowed != allowed;
            entry.allowed = allowed;
            changed
        }
        CallOperation::SetDenial { person, denied, .. } => {
            let denied = directions(*denied);
            if !denied.publish && !denied.subscribe {
                entry.denied.remove(person).is_some()
            } else {
                if !entry.denied.contains_key(person)
                    && (entry.denied.len() >= media::MAX_DENIED_PER_CALL
                        || total_denials >= media::MAX_DENIED_TOTAL)
                {
                    return Err("the call has no room for another denial".into());
                }
                entry.denied.insert(*person, denied) != Some(denied)
            }
        }
    };
    if changed {
        entry.epoch = media::next_counter(entry.epoch).map_err(|error| error.to_string())?;
    }
    Ok(())
}

/// The carrier's view of one admitted session.
#[cfg(feature = "web")]
struct Session {
    calls: std::sync::Weak<Calls>,
    participation: [u8; 32],
    publication: Option<String>,
    prefix: String,
}

#[cfg(feature = "web")]
impl SessionAuthorization for Session {
    fn publication(&self) -> Option<&str> {
        self.publication.as_deref()
    }

    fn subscriptions(&self) -> &[String] {
        &[]
    }

    fn subscription_prefix(&self) -> Option<&str> {
        Some(&self.prefix)
    }

    fn current(&self, direction: Direction, namespace: &str) -> bool {
        self.calls
            .upgrade()
            .is_some_and(|calls| calls.permits(&self.participation, direction, namespace))
    }

    fn status_all(&self) -> AuthorizationStatus {
        let Some(calls) = self.calls.upgrade() else {
            return AuthorizationStatus::Denied;
        };
        let current = calls
            .state()
            .participations
            .contains_key(&self.participation);
        let publishes = self
            .publication
            .as_deref()
            .is_none_or(|name| calls.permits(&self.participation, Direction::Publish, name));
        if current && publishes {
            AuthorizationStatus::Current
        } else {
            AuthorizationStatus::Denied
        }
    }

    fn status_offered(&self, offered: &[&str], lapsed: &mut Vec<String>) -> AuthorizationStatus {
        let status = self.status_all();
        if matches!(status, AuthorizationStatus::Current) {
            lapsed.extend(
                offered
                    .iter()
                    .filter(|name| !self.current(Direction::Subscribe, name))
                    .map(|name| (*name).to_owned()),
            );
        }
        status
    }
}

/// Admits a carrier session presenting a current grant's token.
#[cfg(feature = "web")]
struct Authority(std::sync::Weak<Calls>);

#[cfg(feature = "web")]
impl RelayAuthority for Authority {
    fn admit(&self, proof: &[u8]) -> Result<Arc<dyn SessionAuthorization>, Denied> {
        let calls = self.0.upgrade().ok_or(Denied)?;
        let token: [u8; 32] = proof.try_into().map_err(|_| Denied)?;
        let (id, call, publishes) = calls.admission(&token).ok_or(Denied)?;
        Ok(Arc::new(Session {
            calls: Arc::downgrade(&calls),
            participation: id,
            publication: publishes.then(|| protocol::namespace(&call, &id)),
            prefix: hex::encode(call),
        }))
    }
}

impl Rooms {
    /// Serve this relay's calls on a voice carrier at `endpoint`, whose
    /// certificate `fingerprints` pin, and return the carrier to mount.
    /// Only the first call has effect on where grants point.
    #[cfg(feature = "web")]
    pub(crate) fn voice_carrier(
        &self,
        endpoint: String,
        fingerprints: Vec<String>,
    ) -> tilo_moq::relay::Relay {
        let calls = &self.inner.calls;
        let _ = calls.carrier.set(Carrier {
            endpoint,
            fingerprints,
        });
        tilo_moq::relay::Relay::new(Arc::new(Authority(Arc::downgrade(calls))))
    }

    /// Whether this relay grants voice: it serves a voice carrier.
    pub(crate) fn serves_voice(&self) -> bool {
        self.inner.calls.carrier.get().is_some()
    }

    /// Open, start, end or moderate a call of `room` for `caller`: opening
    /// a channel's call takes a member, everything else a device of the
    /// relay's user, or of a member the room lets run its calls.
    pub(crate) async fn voice_call(
        &self,
        room: &str,
        caller: &AuthenticatedPeer,
        operation: &CallOperation,
    ) -> Result<CallState, String> {
        self.voice_call_as(room, &caller.cert, operation).await
    }

    /// [`Self::voice_call`] for the device `caller` certifies, however it
    /// reached the relay: the envelope and the room's `call` action share
    /// it, and with it the one authorization.
    pub(super) async fn voice_call_as(
        &self,
        room: &str,
        caller: &tilo_user_identity::DeviceCert,
        operation: &CallOperation,
    ) -> Result<CallState, String> {
        let room = self.room(room).await.map_err(|error| error.to_string())?;
        let mut room = room.lock().await;
        if matches!(operation, CallOperation::Open { .. }) {
            // Joining a voice channel is any member's: who may speak or
            // hear in it is the call's directions and denials.
            let access = room
                .peer()
                .governance()
                .governed_access(SystemTime::now())?;
            if !access.member_devices.contains(&caller.device_public_key)
                && !access.member_user_roots.contains(&caller.user_public_key)
            {
                return Err("not a member of the room".into());
            }
        } else {
            self.authorize(
                caller,
                Some(&room),
                RoomOp::Govern(governance::CALL),
                tilo_clock::unix_now_ns(),
            )
            .map_err(|error| error.to_string())?;
        }
        let now = SystemTime::now();
        let mut candidate = room.peer().clone();
        super::prepare_governance(&mut candidate, &self.inner.host.key, now)?;
        let call = transition(&mut candidate, &self.inner.host.key, operation, now)?;
        room.commit(candidate, &self.inner.host.key, now, |record, _| {
            Ok(record.collaboration.clone())
        })?;
        self.settle(&mut room).await?;
        let policy = media::parse(room.peer().admitted_room().ok_or("missing room")?)
            .map_err(|error| error.to_string())?
            .ok_or("the room has no calls")?;
        let entry = policy.calls.get(&call).ok_or("the call vanished")?;
        tracing::info!(room = %room.room(), call = %hex::encode(call), epoch = entry.epoch, "call changed");
        Ok(CallState {
            call,
            epoch: entry.epoch,
            active: entry.phase == CallPhaseV1::Active,
            allowed: from_v1(entry.allowed),
        })
    }

    /// Join `call` of `room` as `peer`, or continue `participation`. The
    /// answer is the participation's grant or why it may not take part;
    /// later grants go to `outbound` as the call changes.
    pub(crate) async fn voice_join(
        &self,
        room: &str,
        peer: &AuthenticatedPeer,
        call: [u8; 32],
        participation: Option<[u8; 32]>,
        outbound: mpsc::Sender<Envelope>,
    ) -> Result<VoiceGrant, String> {
        let calls = &self.inner.calls;
        if !self.serves_voice() {
            return Err("this relay serves no voice carrier".into());
        }
        let governed = self.room(room).await.map_err(|error| error.to_string())?;
        let (workspace_id, epoch, directions) = {
            let governed = governed.lock().await;
            let (epoch, directions) = current(
                &Policy::read(governed.peer())?,
                peer.device_public_key,
                peer.user_public_key,
                &call,
            )?;
            (governed.workspace_id(), epoch, directions)
        };
        let mut state = calls.state();
        let id = if let Some(id) = participation {
            let existing = state
                .participations
                .get_mut(&id)
                .filter(|existing| {
                    existing.call == call && existing.device == peer.device_public_key
                })
                .ok_or("the participation ended; join afresh")?;
            existing.seen = Instant::now();
            existing.outbound = outbound;
            if existing
                .token_expires
                .saturating_duration_since(Instant::now())
                < GRANT_RENEWAL
            {
                state.renew_token(id);
            }
            id
        } else {
            // A device is in one call at a time: its earlier participation,
            // in this call or another, ends with this join.
            let earlier: Vec<[u8; 32]> = state
                .participations
                .iter()
                .filter(|(_, existing)| existing.device == peer.device_public_key)
                .map(|(id, _)| *id)
                .collect();
            let mut left = Vec::new();
            for id in earlier {
                if let Some(existing) = state.remove(&id)
                    && existing.call != call
                {
                    left.push(existing.call);
                }
            }
            left.sort_unstable();
            left.dedup();
            calls.announce(&state, &left);
            let id = loop {
                let id: [u8; 32] = rand::random();
                if !state.participations.contains_key(&id) {
                    break id;
                }
            };
            state.participations.insert(
                id,
                Participation {
                    room: room.to_owned(),
                    workspace_id,
                    call,
                    epoch,
                    device: peer.device_public_key,
                    user: peer.user_public_key,
                    directions,
                    seen: Instant::now(),
                    outbound,
                    token: [0; 32],
                    token_expires: Instant::now(),
                    token_expires_ns: 0,
                },
            );
            state.renew_token(id);
            tracing::info!(%room, call = %hex::encode(call), "voice participant joined");
            id
        };
        let changed = state.participations.get(&id).is_some_and(|participation| {
            (participation.epoch, participation.directions) != (epoch, directions)
        }) || participation.is_none();
        if let Some(existing) = state.participations.get_mut(&id) {
            existing.epoch = epoch;
            existing.directions = directions;
        }
        let grant = calls.grant(&state, &id).ok_or("the participation ended")?;
        if changed {
            for (other, participation) in &state.participations {
                if *other == id || participation.call != call {
                    continue;
                }
                if let Some(grant) = calls.grant(&state, other) {
                    let _ = participation.outbound.try_send(Envelope::VoiceGranted {
                        workspace_id: participation.workspace_id,
                        grant: Box::new(grant),
                    });
                }
            }
        }
        Ok(grant)
    }

    /// The calls of `room` for `peer`, a member, with who is in each.
    pub(crate) async fn voice_calls(
        &self,
        room: &str,
        peer: &AuthenticatedPeer,
    ) -> Result<Vec<CallSummary>, String> {
        let governed = self.room(room).await.map_err(|error| error.to_string())?;
        let policy = Policy::read(governed.lock().await.peer())?;
        if !policy
            .access
            .member_devices
            .contains(&peer.device_public_key)
            && !policy
                .access
                .member_user_roots
                .contains(&peer.user_public_key)
        {
            return Err("not a member of the room".into());
        }
        let Some(calls) = policy.calls else {
            return Ok(Vec::new());
        };
        let state = self.inner.calls.state();
        Ok(calls
            .calls
            .iter()
            .map(|(call, entry)| {
                let mut participants: Vec<VoiceParticipant> = state
                    .in_call(call)
                    .map(|(id, participation)| VoiceParticipant {
                        device_public_key: participation.device,
                        user_public_key: participation.user,
                        namespace: protocol::namespace(call, id),
                        publishes: participation.directions.publish,
                    })
                    .collect();
                participants.sort_by(|a, b| a.namespace.cmp(&b.namespace));
                CallSummary {
                    state: CallState {
                        call: *call,
                        epoch: entry.epoch,
                        active: entry.phase == CallPhaseV1::Active,
                        allowed: from_v1(entry.allowed),
                    },
                    app_id: entry.binding.app_id.clone(),
                    record_id: entry.binding.record_id.clone(),
                    channel_id: entry.binding.channel_id.clone(),
                    participants,
                }
            })
            .collect())
    }

    /// End `participation` of `peer` in `call`.
    pub(crate) fn voice_leave(
        &self,
        peer: &AuthenticatedPeer,
        call: [u8; 32],
        participation: [u8; 32],
    ) {
        let calls = &self.inner.calls;
        let mut state = calls.state();
        let owned = state
            .participations
            .get(&participation)
            .is_some_and(|existing| {
                existing.call == call && existing.device == peer.device_public_key
            });
        if owned && state.remove(&participation).is_some() {
            calls.announce(&state, &[call]);
        }
    }

    /// Re-derive the participations of `room` after a decision.
    pub(super) fn observe_calls(&self, room: &GovernedRoom) {
        self.inner.calls.observe(room);
    }

    /// Every [`OBSERVATION`]: end silent participations and re-derive the
    /// rest, for as long as the rooms exist.
    pub(super) fn watch_calls(&self) {
        let rooms = Arc::downgrade(&self.inner);
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(OBSERVATION);
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tick.tick().await;
                let Some(inner) = rooms.upgrade() else { break };
                let this = Self { inner };
                for room in this.inner.calls.expire() {
                    let Ok(room) = this.room(&room).await else {
                        continue;
                    };
                    this.observe_calls(&*room.lock().await);
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn namespaces_name_their_call_and_participation() {
        let call = [3; 32];
        let participation = [4; 32];
        let namespace = protocol::namespace(&call, &participation);
        assert_eq!(parse_namespace(&call, &namespace), Some(participation));
        assert_eq!(parse_namespace(&[5; 32], &namespace), None);
        assert_eq!(parse_namespace(&call, "nonsense"), None);
    }
}
