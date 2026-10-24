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
//! Each room also has its window carrier
//! ([`tilo_relay_protocol::voice::windows_call`]): the members' devices
//! publish the windows of host programs they run and view each other's on
//! it. It is no governed call: it stands while the room shares its layout,
//! any member device joins it, and a device holds it beside its voice call
//! (one window participation per room), so neither ends the other. A room
//! whose layout turns personal refuses every window participation on the
//! next observation, which closes its carrier.
//!
//! What a participation may do is the room's current policy for its device:
//! the call is active, the device is a member, and neither the call's
//! directions nor a denial of its person exclude it. It is re-derived after
//! every governance decision and every second, and the carrier asks the
//! resulting snapshot on each of its observations, so removing a member or
//! ending a call closes their voice within one carrier observation of the
//! decision.
//!
//! A bridge (a room's Discord bot) holds participations that stand for
//! someone else ([`Bridged`]): one of its own that only hears the call, to
//! carry it out, and one per person on the other side that only speaks,
//! each with its own publication. The host running the bridge joins them
//! in-process ([`Rooms::voice_join_bridged`]), never over the wire; the
//! bot's device must be a bot the room lets show other people's names, and
//! a person a guest is linked to must still be a member, whose denials hold
//! for the guest as for them.
//!
//! Every device that asked for a room's calls ([`Rooms::voice_calls`]) is
//! sent the room's call list again whenever it changes: someone joins or
//! leaves a call, a participation ends, or a call starts or ends. A voice
//! channel shows who is in it the moment that changes, without anyone asking
//! again.

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
    self as protocol, Bridged, CallOperation, CallState, CallSummary, Directions,
    PARTICIPATION_TTL, Showing, VoiceGrant, VoiceParticipant,
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
    /// Whether `call` is the room's window carrier, not a voice call.
    windows: bool,
    epoch: u64,
    device: [u8; 32],
    user: [u8; 32],
    directions: Directions,
    /// What it shows besides its voice, as it last said.
    showing: Showing,
    /// Who it stands for, when a bridge holds it.
    bridged: Option<Bridged>,
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
    watchers: HashMap<String, Vec<Watcher>>,
    /// Rooms whose call list changed since their watchers were last told.
    changed: std::collections::BTreeSet<String>,
}

/// A device told a room's call list whenever it changes.
struct Watcher {
    workspace_id: u64,
    device: [u8; 32],
    user: [u8; 32],
    outbound: mpsc::Sender<Envelope>,
}

impl Participation {
    /// It as the participants of its call see it, published under `id`.
    fn participant(&self, id: &[u8; 32]) -> VoiceParticipant {
        VoiceParticipant {
            device_public_key: self.device,
            user_public_key: self.user,
            namespace: protocol::namespace(&self.call, id),
            publishes: self.directions.publish,
            showing: self.shown(),
            bridged: self.bridged.clone(),
        }
    }

    /// What it shows that the others see: nothing, and muted, while it may
    /// not publish; whether it listens is its own either way.
    const fn shown(&self) -> Showing {
        if self.directions.publish {
            self.showing
        } else {
            Showing {
                camera: false,
                screen: false,
                muted: true,
                deafened: self.showing.deafened,
            }
        }
    }
}

/// The place a participation holds for its device. A device holds each
/// place in one call at a time, and the places never end each other.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Seat<'a> {
    /// The device's own voice.
    Own,
    /// A bridge's own participation, which hears the call to carry it out.
    Bridge,
    /// One person, by account, a bridge carries into the call.
    Carried(&'a str),
}

/// The seat of a participation standing for `bridged`.
fn seat(bridged: Option<&Bridged>) -> Seat<'_> {
    match bridged {
        None => Seat::Own,
        Some(Bridged { account: None, .. }) => Seat::Bridge,
        Some(Bridged {
            account: Some(account),
            ..
        }) => Seat::Carried(account),
    }
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

    /// End the participations of `device` its join of `call` in `seat`
    /// replaces: of a voice call, its participation in that seat; of a
    /// room's window carrier (`windows`), its earlier one in that carrier.
    /// The other calls it left, whose participants hear the change.
    fn end_device(
        &mut self,
        device: [u8; 32],
        call: &[u8; 32],
        windows: bool,
        seat: Seat<'_>,
    ) -> Vec<[u8; 32]> {
        let earlier: Vec<[u8; 32]> = self
            .participations
            .iter()
            .filter(|(_, existing)| {
                existing.device == device
                    && existing.windows == windows
                    && self::seat(existing.bridged.as_ref()) == seat
                    && (!windows || existing.call == *call)
            })
            .map(|(id, _)| *id)
            .collect();
        let mut left = Vec::new();
        for id in earlier {
            if let Some(existing) = self.remove(&id) {
                self.changed.insert(existing.room.clone());
                if existing.call != *call {
                    left.push(existing.call);
                }
            }
        }
        left.sort_unstable();
        left.dedup();
        left
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
            .map(|(other, participation)| participation.participant(other))
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

    /// Send every participant of `call` but `joined` its current grant:
    /// `joined` has its own as the answer to its join.
    fn announce_to_others(&self, state: &State, call: [u8; 32], joined: [u8; 32]) {
        for (other, participation) in &state.participations {
            if *other == joined || participation.call != call {
                continue;
            }
            if let Some(grant) = self.grant(state, other) {
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
        let policy = Policy::read(room);
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
                    participation.bridged.as_ref(),
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
        if !changed.is_empty() {
            state.changed.insert(room.room().to_owned());
        }
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
        let mut changed: Vec<[u8; 32]> = Vec::new();
        for id in &gone {
            if let Some(participation) = state.remove(id) {
                tracing::info!(
                    room = %participation.room,
                    call = %hex::encode(participation.call),
                    unheard_ms = now.duration_since(participation.seen).as_millis(),
                    connection_closed = participation.outbound.is_closed(),
                    "voice participation expired"
                );
                changed.push(participation.call);
                state.changed.insert(participation.room);
            }
        }
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
    fn admission(&self, token: &[u8; 32]) -> Option<([u8; 32], [u8; 32], Directions)> {
        let state = self.state();
        let id = *state.tokens.get(token)?;
        state
            .participations
            .get(&id)
            .filter(|participation| Instant::now() < participation.token_expires)
            .map(|participation| (id, participation.call, participation.directions))
    }

    /// Whether participation `id` may use `direction` on `namespace` now.
    #[cfg(feature = "web")]
    fn permits(&self, id: &[u8; 32], direction: Direction, namespace: &str) -> bool {
        self.state().permits(id, direction, namespace)
    }

    /// Participation `id`'s carrier session is still connected: as good as
    /// a heartbeat. A browser runs a hidden, silent tab's timers once a
    /// minute, its heartbeats with them, while its network stack keeps the
    /// carrier alive; the carrier's idle timeout still ends a device that
    /// went away.
    #[cfg(feature = "web")]
    fn carrier_connected(&self, id: &[u8; 32]) {
        if let Some(participation) = self.state().participations.get_mut(id) {
            participation.seen = Instant::now();
        }
    }
}

impl State {
    /// Whether participation `id` may use `direction` on `namespace`.
    #[cfg(feature = "web")]
    fn permits(&self, id: &[u8; 32], direction: Direction, namespace: &str) -> bool {
        let Some(own) = self.participations.get(id) else {
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
                    && self
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
    /// The room's window carrier ([`protocol::windows_call`]).
    windows: [u8; 32],
    /// Whether the room shares its members' layout, and with it their
    /// windows.
    shares_layout: bool,
    /// The devices of the bots acting in the room that may show other
    /// people's names: those that may bridge a call.
    bridges: Vec<[u8; 32]>,
}

impl Policy {
    /// `room`'s policy. A relay that stewards the room's channels resolved
    /// every call's binding through their keys when it opened the call, so
    /// a call bound to a Chat its members write sealed still stands; any
    /// other replica keeps such a Chat as ciphertext and fails closed.
    fn read(room: &GovernedRoom) -> Result<Self, String> {
        let peer = room.peer();
        let calls = if room.stewardship.is_some() {
            peer.media().stewarded_media_policy()?
        } else {
            peer.validated_media_policy()?
        };
        let now = SystemTime::now();
        let bridges = peer
            .governance()
            .installations(now)
            .unwrap_or_default()
            .into_iter()
            .filter(|bot| {
                bot.acting
                    && bot.withdrawn_at_ns.is_none()
                    && matches!(
                        bot.installation.appearance,
                        tilo_peer_state::AppearancePolicy::Personas
                    )
            })
            .map(|bot| bot.device_public_key)
            .collect();
        Ok(Self {
            calls,
            access: peer.governance().governed_access(now)?,
            windows: protocol::windows_call(room.room()),
            shares_layout: !peer.governance().room_layout().is_personal(),
            bridges,
        })
    }
}

/// What `device` of person `user` may do in `call` under `policy`,
/// standing for `bridged` when a bridge holds it: the call's epoch and
/// directions, or why it may not take part. A person's every certified
/// device takes part, as the relay admits them all to the room; the relay
/// verified the certificate when the device connected.
fn current(
    policy: &Policy,
    device: [u8; 32],
    user: [u8; 32],
    call: &[u8; 32],
    bridged: Option<&Bridged>,
) -> Result<(u64, Directions), String> {
    if *call == policy.windows {
        if bridged.is_some() {
            return Err("a bridge carries voice calls, not windows".into());
        }
        // The room's window carrier: every member device publishes the
        // windows it runs and views the others', while the room shares its
        // layout.
        if !member(policy, device, user) {
            return Err("not a member of the room".into());
        }
        if !policy.shares_layout {
            return Err("the room keeps each member's windows on their device".into());
        }
        return Ok((
            0,
            Directions {
                publish: true,
                subscribe: true,
            },
        ));
    }
    let call = policy
        .calls
        .as_ref()
        .and_then(|calls| calls.calls.get(call))
        .ok_or("the room has no such call")?;
    if call.phase != CallPhaseV1::Active {
        return Err("the call has ended".into());
    }
    if !member(policy, device, user) {
        return Err("not a member of the room".into());
    }
    let linked = match bridged {
        None => None,
        Some(bridged) => {
            if !policy.bridges.contains(&device) {
                return Err(
                    "only a bot the room lets show other people's names bridges a call".into(),
                );
            }
            if let Some(linked) = bridged.linked
                && !policy.access.member_user_roots.contains(&linked)
            {
                return Err("the person this account is linked to is no member of the room".into());
            }
            bridged.linked
        }
    };
    // A guest linked to a person is that person in the call: a denial of
    // either holds.
    let denied = |pick: fn(&DirectionsV1) -> bool| {
        call.denied.get(&user).is_some_and(pick)
            || linked.is_some_and(|linked| call.denied.get(&linked).is_some_and(pick))
    };
    let mut directions = Directions {
        publish: call.allowed.publish && !denied(|value| value.publish),
        subscribe: call.allowed.subscribe && !denied(|value| value.subscribe),
    };
    // A bridge's own participation only hears the call, to carry it out; a
    // person it carries only speaks, and hears the call on their service.
    match seat(bridged) {
        Seat::Own => {}
        Seat::Bridge => directions.publish = false,
        Seat::Carried(_) => directions.subscribe = false,
    }
    Ok((call.epoch, directions))
}

/// Whether a participant may be shown as `bridged`: a lowercase service,
/// and a name, avatar and account an appearance may carry
/// (`tilo_actions::appearance`).
#[cfg(any(feature = "agents", test))]
fn validate_bridged(bridged: &Bridged) -> Result<(), String> {
    let service = bridged.service.chars().count();
    if !(1..=32).contains(&service)
        || !bridged
            .service
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    {
        return Err("a bridged service is 1 to 32 lowercase letters, digits or '-'".into());
    }
    tilo_actions::appearance::Appearance {
        name: bridged.name.clone(),
        avatar: bridged.avatar.clone(),
        persona: bridged
            .account
            .as_ref()
            .map(|account| tilo_actions::appearance::Persona {
                service: bridged.service.clone(),
                account: account.clone(),
                linked: bridged.linked.map(hex::encode),
            }),
    }
    .validate()
    .map_err(|error| error.to_string())
}

impl GovernedRoom {
    /// The call binding of voice channel `channel` of `record` of `app`. A
    /// Chat its members write sealed is read through the channel keys this
    /// relay holds as the room's steward: members of a hosted room seal
    /// every write, so without that no one could open a call in any Chat
    /// somebody had written to.
    async fn call_binding(
        &self,
        app: &str,
        record: &str,
        channel: &str,
    ) -> Result<media::ChatCallBindingV1, String> {
        if !self.peer().publication().is_sealed_record(app, record) {
            return self
                .peer()
                .media()
                .resolve_media_binding(app, record, channel);
        }
        self.sealed_call_binding(app, record, channel).await
    }

    /// [`Self::call_binding`] of a sealed Chat, opened with the channel's
    /// keys this relay holds as steward.
    #[cfg(any(feature = "agents", feature = "web"))]
    async fn sealed_call_binding(
        &self,
        app: &str,
        record: &str,
        channel: &str,
    ) -> Result<media::ChatCallBindingV1, String> {
        let saved = self
            .readable_document(app, record)
            .await?
            .ok_or("exact admitted Chat missing")?;
        let doc = automerge::AutoCommit::load(&saved).map_err(|error| error.to_string())?;
        media::resolve_chat_binding(app, record, channel, &doc).map_err(|error| error.to_string())
    }

    /// A relay built without the browser listener stewards no channels, so
    /// it cannot read a sealed Chat.
    #[cfg(not(any(feature = "agents", feature = "web")))]
    #[expect(
        clippy::unused_async,
        reason = "the same call as the stewarding build's"
    )]
    async fn sealed_call_binding(
        &self,
        _app: &str,
        _record: &str,
        _channel: &str,
    ) -> Result<media::ChatCallBindingV1, String> {
        Err("the Chat is sealed and this relay does not steward its channel".into())
    }
}

/// Whether `device` of person `user` is a member of the room under `policy`.
fn member(policy: &Policy, device: [u8; 32], user: [u8; 32]) -> bool {
    policy.access.member_devices.contains(&device)
        || policy.access.member_user_roots.contains(&user)
}

/// The room's calls under `policy`, with who is in each now.
fn call_list(state: &State, policy: &Policy) -> Vec<CallSummary> {
    let Some(calls) = &policy.calls else {
        return Vec::new();
    };
    calls
        .calls
        .iter()
        .map(|(call, entry)| {
            let mut participants: Vec<VoiceParticipant> = state
                .in_call(call)
                .map(|(id, participation)| participation.participant(id))
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
        .collect()
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
    binding: Option<media::ChatCallBindingV1>,
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
        CallOperation::Open { .. } => open(
            &mut policy,
            binding.ok_or("opening a call needs its channel's binding")?,
        )?,
        CallOperation::Start { allowed, .. } => start(
            &mut policy,
            binding.ok_or("starting a call needs its channel's binding")?,
            *allowed,
        )?,
        CallOperation::End { call, epoch }
        | CallOperation::Restart { call, epoch }
        | CallOperation::SetDirections { call, epoch, .. }
        | CallOperation::SetDenial { call, epoch, .. } => {
            moderate(&mut policy, operation, binding, call, *epoch)?;
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
    policy: &mut RoomMediaV1,
    operation: &CallOperation,
    binding: Option<media::ChatCallBindingV1>,
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
            let binding = binding.ok_or("the call's channel no longer resolves")?;
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
    prefix: Option<String>,
}

#[cfg(feature = "web")]
impl Session {
    /// Whether this session still holds every direction it was admitted with.
    fn status(&self, state: &State) -> AuthorizationStatus {
        let Some(own) = state.participations.get(&self.participation) else {
            return AuthorizationStatus::Denied;
        };
        let subscribes = self.prefix.is_none() || own.directions.subscribe;
        let publishes = self
            .publication
            .as_deref()
            .is_none_or(|name| state.permits(&self.participation, Direction::Publish, name));
        if subscribes && publishes {
            AuthorizationStatus::Current
        } else {
            AuthorizationStatus::Denied
        }
    }

    /// Its status and, while current, the `offered` publications it may no
    /// longer hear, read from one `state`: a lapse of this session's own
    /// authority must never be reported as its publishers' lapse, which
    /// would close their publications.
    fn status_offered_in(
        &self,
        state: &State,
        offered: &[&str],
        lapsed: &mut Vec<String>,
    ) -> AuthorizationStatus {
        let status = self.status(state);
        if matches!(status, AuthorizationStatus::Current) {
            lapsed.extend(
                offered
                    .iter()
                    .filter(|name| !state.permits(&self.participation, Direction::Subscribe, name))
                    .map(|name| (*name).to_owned()),
            );
        }
        status
    }
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
        self.prefix.as_deref()
    }

    fn current(&self, direction: Direction, namespace: &str) -> bool {
        self.calls
            .upgrade()
            .is_some_and(|calls| calls.permits(&self.participation, direction, namespace))
    }

    fn status_all(&self) -> AuthorizationStatus {
        self.calls
            .upgrade()
            .map_or(AuthorizationStatus::Denied, |calls| {
                self.status(&calls.state())
            })
    }

    fn status_offered(&self, offered: &[&str], lapsed: &mut Vec<String>) -> AuthorizationStatus {
        self.calls
            .upgrade()
            .map_or(AuthorizationStatus::Denied, |calls| {
                self.status_offered_in(&calls.state(), offered, lapsed)
            })
    }

    fn connected(&self) {
        if let Some(calls) = self.calls.upgrade() {
            calls.carrier_connected(&self.participation);
        }
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
        let (id, call, directions) = calls.admission(&token).ok_or(Denied)?;
        Ok(Arc::new(Session {
            calls: Arc::downgrade(&calls),
            participation: id,
            publication: directions.publish.then(|| protocol::namespace(&call, &id)),
            prefix: directions.subscribe.then(|| hex::encode(call)),
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
    pub(crate) async fn voice_call_as(
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
        let binding = match operation {
            CallOperation::Open {
                app_id,
                record_id,
                channel_id,
            }
            | CallOperation::Start {
                app_id,
                record_id,
                channel_id,
                ..
            } => Some(room.call_binding(app_id, record_id, channel_id).await?),
            // A restarted call keeps its channel, which must still be one.
            CallOperation::Restart { call, .. } => {
                let bound = media::parse(room.peer().admitted_room().ok_or("missing room")?)
                    .map_err(|error| error.to_string())?
                    .and_then(|policy| policy.calls.get(call).map(|call| call.binding.clone()));
                match bound {
                    Some(bound) => Some(
                        room.call_binding(&bound.app_id, &bound.record_id, &bound.channel_id)
                            .await?,
                    ),
                    None => None,
                }
            }
            _ => None,
        };
        let now = SystemTime::now();
        let mut candidate = room.peer().clone();
        super::prepare_governance(&mut candidate, &self.inner.host.key, now)?;
        let call = transition(
            &mut candidate,
            &self.inner.host.key,
            operation,
            binding,
            now,
        )?;
        room.commit(candidate, &self.inner.host.key, now, |record, _| {
            Ok(record.collaboration.clone())
        })?;
        self.settle(&mut room).await?;
        let policy = media::parse(room.peer().admitted_room().ok_or("missing room")?)
            .map_err(|error| error.to_string())?
            .ok_or("the room has no calls")?;
        let entry = policy.calls.get(&call).ok_or("the call vanished")?;
        tracing::info!(room = %room.room(), call = %hex::encode(call), epoch = entry.epoch, "call changed");
        self.inner
            .calls
            .state()
            .changed
            .insert(room.room().to_owned());
        drop(room);
        self.tell_call_lists();
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
        self.join(
            room,
            (peer.device_public_key, peer.user_public_key),
            call,
            participation,
            None,
            outbound,
        )
        .await
    }

    /// Join `call` of `room` with the bot device `cert` standing for
    /// `bridged`, or continue `participation` (a changed name, avatar or
    /// link is taken as it is). A device holds the bridge's own seat and
    /// one per account it carries, each in one call at a time. Only the
    /// host running the bridge calls this: no envelope reaches it.
    ///
    /// # Errors
    /// `bridged` is malformed, the device is no bot the room lets show
    /// other people's names, the person an account is linked to is no
    /// member, or the call refuses it as anyone.
    #[cfg(feature = "agents")]
    pub(crate) async fn voice_join_bridged(
        &self,
        room: &str,
        cert: &tilo_user_identity::DeviceCert,
        call: [u8; 32],
        participation: Option<[u8; 32]>,
        bridged: Bridged,
        outbound: mpsc::Sender<Envelope>,
    ) -> Result<VoiceGrant, String> {
        validate_bridged(&bridged)?;
        tilo_user_identity::verify_device_cert_at(cert, tilo_clock::unix_now_ns())
            .map_err(|error| error.to_string())?;
        self.join(
            room,
            (cert.device_public_key, cert.user_public_key),
            call,
            participation,
            Some(bridged),
            outbound,
        )
        .await
    }

    /// [`Self::voice_join`] for `(device, user)`, standing for `bridged`.
    async fn join(
        &self,
        room: &str,
        (device, user): ([u8; 32], [u8; 32]),
        call: [u8; 32],
        participation: Option<[u8; 32]>,
        bridged: Option<Bridged>,
        outbound: mpsc::Sender<Envelope>,
    ) -> Result<VoiceGrant, String> {
        let calls = &self.inner.calls;
        if !self.serves_voice() {
            return Err("this relay serves no voice carrier".into());
        }
        let governed = self.room(room).await.map_err(|error| error.to_string())?;
        let (workspace_id, epoch, directions, windows) = {
            let governed = governed.lock().await;
            let policy = Policy::read(&governed)?;
            let (epoch, directions) = current(&policy, device, user, &call, bridged.as_ref())?;
            (
                governed.workspace_id(),
                epoch,
                directions,
                call == policy.windows,
            )
        };
        let mut state = calls.state();
        let mut changed = participation.is_none();
        let id = if let Some(id) = participation {
            let existing = state
                .participations
                .get_mut(&id)
                .filter(|existing| {
                    existing.call == call
                        && existing.device == device
                        && seat(existing.bridged.as_ref()) == seat(bridged.as_ref())
                })
                .ok_or("the participation ended; join afresh")?;
            existing.seen = Instant::now();
            existing.outbound = outbound;
            if existing.bridged != bridged {
                existing.bridged = bridged;
                changed = true;
            }
            if existing
                .token_expires
                .saturating_duration_since(Instant::now())
                < GRANT_RENEWAL
            {
                state.renew_token(id);
            }
            id
        } else {
            // A device is in one call at a time in each seat: its earlier
            // participation there, in this call or another, ends with this
            // join. Its window carrier stands beside it.
            let left = state.end_device(device, &call, windows, seat(bridged.as_ref()));
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
                    windows,
                    epoch,
                    device,
                    user,
                    directions,
                    showing: Showing::default(),
                    bridged,
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
        changed |= state.participations.get(&id).is_some_and(|participation| {
            (participation.epoch, participation.directions) != (epoch, directions)
        });
        if let Some(existing) = state.participations.get_mut(&id) {
            existing.epoch = epoch;
            existing.directions = directions;
        }
        let grant = calls.grant(&state, &id).ok_or("the participation ended")?;
        if changed {
            state.changed.insert(room.to_owned());
            calls.announce_to_others(&state, call, id);
        }
        drop(state);
        self.tell_call_lists();
        Ok(grant)
    }

    /// The calls of `room` for `peer`, a member, with who is in each. From
    /// now on `outbound` is sent the list again whenever it changes, while
    /// the device is a member and its connection lasts.
    pub(crate) async fn voice_calls(
        &self,
        room: &str,
        peer: &AuthenticatedPeer,
        outbound: mpsc::Sender<Envelope>,
    ) -> Result<Vec<CallSummary>, String> {
        self.watch_call_list(
            room,
            (peer.device_public_key, peer.user_public_key),
            outbound,
        )
        .await
    }

    /// [`Self::voice_calls`] for the device `cert` certifies, in-process:
    /// how the host running a bridge follows who is in the room's calls.
    #[cfg(feature = "agents")]
    pub(crate) async fn voice_calls_as(
        &self,
        room: &str,
        cert: &tilo_user_identity::DeviceCert,
        outbound: mpsc::Sender<Envelope>,
    ) -> Result<Vec<CallSummary>, String> {
        self.watch_call_list(
            room,
            (cert.device_public_key, cert.user_public_key),
            outbound,
        )
        .await
    }

    async fn watch_call_list(
        &self,
        room: &str,
        (device, user): ([u8; 32], [u8; 32]),
        outbound: mpsc::Sender<Envelope>,
    ) -> Result<Vec<CallSummary>, String> {
        let governed = self.room(room).await.map_err(|error| error.to_string())?;
        let (workspace_id, policy) = {
            let governed = governed.lock().await;
            (governed.workspace_id(), Policy::read(&governed)?)
        };
        if !member(&policy, device, user) {
            return Err("not a member of the room".into());
        }
        let mut state = self.inner.calls.state();
        let watchers = state.watchers.entry(room.to_owned()).or_default();
        watchers.retain(|watcher| !watcher.outbound.is_closed() && watcher.device != device);
        watchers.push(Watcher {
            workspace_id,
            device,
            user,
            outbound,
        });
        let list = call_list(&state, &policy);
        drop(state);
        Ok(list)
    }

    /// Tell the watchers of every room whose call list changed the new
    /// list, off the caller's path.
    fn tell_call_lists(&self) {
        if self.inner.calls.state().changed.is_empty() {
            return;
        }
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let this = self.clone();
        runtime.spawn(async move { this.send_call_lists().await });
    }

    async fn send_call_lists(&self) {
        let changed = std::mem::take(&mut self.inner.calls.state().changed);
        for room in changed {
            let Ok(governed) = self.room(&room).await else {
                continue;
            };
            let Ok(policy) = Policy::read(&*governed.lock().await) else {
                continue;
            };
            let mut state = self.inner.calls.state();
            let list = call_list(&state, &policy);
            let Some(watchers) = state.watchers.get_mut(&room) else {
                continue;
            };
            // A device that left the room, or whose connection closed, is
            // told nothing more.
            watchers.retain(|watcher| {
                !watcher.outbound.is_closed() && member(&policy, watcher.device, watcher.user)
            });
            for watcher in watchers.iter() {
                // A full queue hears the next change, or asks again.
                let _ = watcher.outbound.try_send(Envelope::VoiceCallList {
                    workspace_id: watcher.workspace_id,
                    result: Ok(list.clone()),
                });
            }
            if watchers.is_empty() {
                state.watchers.remove(&room);
            }
        }
    }

    /// End `participation` of `peer` in `call`.
    pub(crate) fn voice_leave(
        &self,
        peer: &AuthenticatedPeer,
        call: [u8; 32],
        participation: [u8; 32],
    ) {
        self.leave(peer.device_public_key, call, participation);
    }

    /// End `participation` in `call` the bot device `cert` holds for a
    /// bridge ([`Self::voice_join_bridged`]).
    #[cfg(feature = "agents")]
    pub(crate) fn voice_leave_bridged(
        &self,
        cert: &tilo_user_identity::DeviceCert,
        call: [u8; 32],
        participation: [u8; 32],
    ) {
        self.leave(cert.device_public_key, call, participation);
    }

    fn leave(&self, device: [u8; 32], call: [u8; 32], participation: [u8; 32]) {
        let calls = &self.inner.calls;
        let mut state = calls.state();
        let owned = state
            .participations
            .get(&participation)
            .is_some_and(|existing| existing.call == call && existing.device == device);
        if owned && let Some(left) = state.remove(&participation) {
            calls.announce(&state, &[call]);
            state.changed.insert(left.room);
            drop(state);
            self.tell_call_lists();
        }
    }

    /// Record what `participation` of `peer` in `call` shows besides its
    /// voice; when that changed, the call's participants get their grants
    /// again and the room's call list watchers the new list.
    pub(crate) fn voice_show(
        &self,
        peer: &AuthenticatedPeer,
        call: [u8; 32],
        participation: [u8; 32],
        showing: Showing,
    ) {
        self.show(peer.device_public_key, call, participation, showing);
    }

    /// [`Self::voice_show`] for a participation a bridge on this host holds
    /// for a person on another service: whether that person is muted there.
    /// The host running the bridge calls this: no envelope reaches it.
    #[cfg(feature = "agents")]
    pub(crate) fn voice_show_bridged(
        &self,
        cert: &tilo_user_identity::DeviceCert,
        call: [u8; 32],
        participation: [u8; 32],
        showing: Showing,
    ) {
        self.show(cert.device_public_key, call, participation, showing);
    }

    /// Set what participation `participation` of `device` in `call` shows.
    fn show(&self, device: [u8; 32], call: [u8; 32], participation: [u8; 32], showing: Showing) {
        let calls = &self.inner.calls;
        let mut state = calls.state();
        let Some(existing) = state
            .participations
            .get_mut(&participation)
            .filter(|existing| existing.call == call && existing.device == device)
        else {
            return;
        };
        if existing.showing == showing {
            return;
        }
        existing.showing = showing;
        let room = existing.room.clone();
        calls.announce(&state, &[call]);
        state.changed.insert(room);
        drop(state);
        self.tell_call_lists();
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
                Self { inner }.observe_expired_calls().await;
            }
        });
    }

    /// Observe the calls whose sessions expired, then send each room its
    /// call list.
    async fn observe_expired_calls(&self) {
        for room in self.inner.calls.expire() {
            let Ok(room) = self.room(&room).await else {
                continue;
            };
            self.observe_calls(&*room.lock().await);
        }
        self.send_call_lists().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two participations of one call, both publishing and subscribing,
    /// admitted as carrier sessions.
    #[cfg(feature = "web")]
    fn two_in_a_call() -> (Arc<Calls>, Arc<dyn SessionAuthorization>, String) {
        let calls = Arc::new(Calls::default());
        let call = [3; 32];
        for id in [[4; 32], [5; 32]] {
            let (outbound, _) = mpsc::channel(1);
            let mut state = calls.state();
            state.participations.insert(
                id,
                Participation {
                    room: "room".into(),
                    workspace_id: 1,
                    call,
                    windows: false,
                    epoch: 1,
                    device: id,
                    user: id,
                    directions: Directions {
                        publish: true,
                        subscribe: true,
                    },
                    showing: Showing::default(),
                    bridged: None,
                    seen: Instant::now(),
                    outbound,
                    token: id,
                    token_expires: Instant::now().checked_add(GRANT_TTL).unwrap(),
                    token_expires_ns: 0,
                },
            );
            state.tokens.insert(id, id);
        }
        let subscriber = Authority(Arc::downgrade(&calls)).admit(&[4; 32]).unwrap();
        (calls, subscriber, protocol::namespace(&call, &[5; 32]))
    }

    #[cfg(feature = "web")]
    fn directions(calls: &Calls, id: [u8; 32], directions: Directions) {
        calls
            .state()
            .participations
            .get_mut(&id)
            .unwrap()
            .directions = directions;
    }

    #[cfg(feature = "web")]
    #[test]
    fn only_a_removed_publisher_lapses() {
        let (calls, subscriber, publisher) = two_in_a_call();
        let mut lapsed = Vec::new();
        assert!(matches!(
            subscriber.status_offered(&[&publisher], &mut lapsed),
            AuthorizationStatus::Current
        ));
        assert!(lapsed.is_empty());
        calls.state().remove(&[5; 32]);
        assert!(matches!(
            subscriber.status_offered(&[&publisher], &mut lapsed),
            AuthorizationStatus::Current
        ));
        assert_eq!(lapsed, [publisher]);
    }

    #[cfg(feature = "web")]
    #[test]
    fn losing_its_own_authority_never_lapses_its_publishers() {
        let (calls, subscriber, publisher) = two_in_a_call();
        for (publish, subscribe) in [(true, false), (false, true), (false, false)] {
            directions(&calls, [4; 32], Directions { publish, subscribe });
            let mut lapsed = Vec::new();
            assert!(matches!(
                subscriber.status_offered(&[&publisher], &mut lapsed),
                AuthorizationStatus::Denied
            ));
            assert!(lapsed.is_empty(), "{publish} {subscribe}: {lapsed:?}");
        }
        calls.state().remove(&[4; 32]);
        let mut lapsed = Vec::new();
        assert!(matches!(
            subscriber.status_offered(&[&publisher], &mut lapsed),
            AuthorizationStatus::Denied
        ));
        assert!(lapsed.is_empty());
    }

    #[cfg(feature = "web")]
    #[test]
    fn a_session_is_scoped_to_the_directions_it_was_admitted_with() {
        let (calls, _, publisher) = two_in_a_call();
        let authority = Authority(Arc::downgrade(&calls));

        directions(
            &calls,
            [4; 32],
            Directions {
                publish: true,
                subscribe: false,
            },
        );
        let publishing = authority.admit(&[4; 32]).unwrap();
        assert!(publishing.subscription_prefix().is_none());
        assert!(publishing.publication().is_some());
        assert!(matches!(
            publishing.status_all(),
            AuthorizationStatus::Current
        ));

        directions(
            &calls,
            [4; 32],
            Directions {
                publish: false,
                subscribe: true,
            },
        );
        let hearing = authority.admit(&[4; 32]).unwrap();
        assert!(hearing.publication().is_none());
        let mut lapsed = Vec::new();
        assert!(matches!(
            hearing.status_offered(&[&publisher], &mut lapsed),
            AuthorizationStatus::Current
        ));
        assert!(lapsed.is_empty());
        assert!(matches!(
            publishing.status_all(),
            AuthorizationStatus::Denied
        ));
    }

    /// A hidden browser tab beats once a minute; its connected carrier keeps
    /// its participation, and one with no carrier still ends on time.
    #[cfg(feature = "web")]
    #[test]
    fn a_connected_carrier_keeps_a_participation_whose_heartbeats_are_late() {
        let calls = Arc::new(Calls::default());
        let unheard = Instant::now()
            .checked_sub(PARTICIPATION_TTL.saturating_add(Duration::from_secs(1)))
            .unwrap();
        let mut connections = Vec::new();
        for id in [[4; 32], [5; 32]] {
            let (outbound, connection) = mpsc::channel(1);
            connections.push(connection);
            let mut fixture = participation(id, [3; 32], false);
            fixture.outbound = outbound;
            fixture.seen = unheard;
            fixture.token = id;
            fixture.token_expires = Instant::now().checked_add(GRANT_TTL).unwrap();
            let mut state = calls.state();
            state.participations.insert(id, fixture);
            state.tokens.insert(id, id);
        }
        let carrier = Authority(Arc::downgrade(&calls)).admit(&[4; 32]).unwrap();
        carrier.connected();
        calls.expire();
        let remaining: Vec<[u8; 32]> = calls.state().participations.keys().copied().collect();
        assert_eq!(remaining, [[4; 32]]);
        drop(connections);
    }

    /// One participation of `device` in `call`, for `State` fixtures.
    fn participation(device: [u8; 32], call: [u8; 32], windows: bool) -> Participation {
        let (outbound, _) = mpsc::channel(1);
        Participation {
            room: "room".into(),
            workspace_id: 1,
            call,
            windows,
            epoch: 0,
            device,
            user: device,
            directions: Directions {
                publish: true,
                subscribe: true,
            },
            showing: Showing::default(),
            bridged: None,
            seen: Instant::now(),
            outbound,
            token: [0; 32],
            token_expires: Instant::now(),
            token_expires_ns: 0,
        }
    }

    /// A device's window carrier stands beside its voice call: joining a
    /// call ends only its earlier voice participation, and joining a room's
    /// window carrier only its earlier one in that carrier.
    #[test]
    fn a_window_carrier_stands_beside_a_voice_call() {
        let device = [9; 32];
        let windows = protocol::windows_call("room");
        let elsewhere = protocol::windows_call("other room");
        let mut state = State::default();
        state
            .participations
            .insert([1; 32], participation(device, [3; 32], false));
        state
            .participations
            .insert([2; 32], participation(device, windows, true));
        state
            .participations
            .insert([7; 32], participation(device, elsewhere, true));

        assert_eq!(
            state.end_device(device, &[4; 32], false, Seat::Own),
            [[3; 32]]
        );
        assert_eq!(
            state
                .participations
                .keys()
                .copied()
                .collect::<std::collections::BTreeSet<_>>(),
            [[2; 32], [7; 32]].into(),
            "both window carriers stand"
        );
        assert!(
            state
                .end_device(device, &windows, true, Seat::Own)
                .is_empty()
        );
        assert_eq!(
            state.participations.keys().copied().collect::<Vec<_>>(),
            [[7; 32]],
            "a fresh join of one room's carrier replaces only its own"
        );
    }

    /// Any member device may publish and view windows on a room's window
    /// carrier while the room shares its layout, and nobody may once it
    /// keeps each member's windows on their device.
    #[test]
    fn a_rooms_window_carrier_admits_its_members_while_it_shares_its_layout() {
        let member_device = [1; 32];
        let mut policy = Policy {
            calls: None,
            access: GovernedAccess {
                member_devices: vec![member_device],
                member_user_roots: Vec::new(),
            },
            windows: protocol::windows_call("room"),
            shares_layout: true,
            bridges: Vec::new(),
        };
        let windows = policy.windows;
        assert_eq!(
            current(&policy, member_device, [2; 32], &windows, None),
            Ok((
                0,
                Directions {
                    publish: true,
                    subscribe: true,
                }
            ))
        );
        assert!(
            current(&policy, [3; 32], [4; 32], &windows, None).is_err(),
            "a stranger"
        );
        assert!(
            current(
                &policy,
                member_device,
                [2; 32],
                &protocol::windows_call("other room"),
                None
            )
            .is_err(),
            "another room's carrier is no call of this one"
        );
        policy.shares_layout = false;
        assert!(current(&policy, member_device, [2; 32], &windows, None).is_err());
    }

    /// Who a bridge's participation in a fixture stands for: the bridge
    /// itself without `account`.
    fn bridged(account: Option<&str>, linked: Option<[u8; 32]>) -> Bridged {
        Bridged {
            service: "discord".into(),
            account: account.map(str::to_owned),
            name: account.map_or("Discord", |_| "Grace").into(),
            avatar: None,
            linked,
        }
    }

    /// A bridge's own participation and each person it carries are their
    /// own seats: joining one ends only the device's earlier participation
    /// in that seat, never another, nor the device's own voice.
    #[test]
    fn a_bridge_holds_its_own_seat_and_one_per_person_it_carries() {
        let device = [9; 32];
        let (call, other) = ([3; 32], [4; 32]);
        let mut state = State::default();
        let mut seated = |id: [u8; 32], call: [u8; 32], who: Option<Bridged>| {
            let mut held = participation(device, call, false);
            held.bridged = who;
            state.participations.insert(id, held);
        };
        seated([1; 32], call, Some(bridged(None, None)));
        seated([2; 32], call, Some(bridged(Some("100"), None)));
        seated([5; 32], call, Some(bridged(Some("200"), None)));
        seated([6; 32], other, None);

        assert_eq!(
            state.end_device(device, &other, false, Seat::Carried("100")),
            [call],
            "Grace moves to the other call"
        );
        assert_eq!(
            state.end_device(device, &other, false, Seat::Bridge),
            [call],
            "the bridge's own seat moves alone"
        );
        let left: std::collections::BTreeSet<_> = state.participations.keys().copied().collect();
        assert_eq!(
            left,
            [[5; 32], [6; 32]].into(),
            "the other person and the device's own voice stand"
        );
    }

    /// A policy with one active call `[3; 32]` in which members publish and
    /// subscribe, whose members are `people` (device = person), and whose
    /// one bot that may show other names is `bridge`.
    fn bridging_policy(people: &[[u8; 32]], bridge: [u8; 32]) -> Policy {
        let mut calls = BTreeMap::new();
        calls.insert(
            [3; 32],
            media::CallV1 {
                binding: media::ChatCallBindingV1 {
                    app_id: "so.tilo.chat".into(),
                    record_id: String::new(),
                    channel_id: "voice".into(),
                    channel_object: "voice".into(),
                    chat_workspace_id: "main".into(),
                },
                epoch: 2,
                phase: CallPhaseV1::Active,
                allowed: DirectionsV1 {
                    publish: true,
                    subscribe: true,
                },
                denied: BTreeMap::new(),
            },
        );
        Policy {
            calls: Some(RoomMediaV1 {
                version: 1,
                room: [0; 16],
                authorization_revision: 1,
                authorization_basis: [0; 32],
                calls,
            }),
            access: GovernedAccess {
                member_devices: people.iter().copied().chain([bridge]).collect(),
                member_user_roots: people.to_vec(),
            },
            windows: protocol::windows_call("room"),
            shares_layout: true,
            bridges: vec![bridge],
        }
    }

    /// A bridge's own participation only hears, a person it carries only
    /// speaks; only a bot the room lets show other names bridges, and only
    /// into voice calls.
    #[test]
    fn a_bridge_hears_the_call_and_the_people_it_carries_speak() {
        let (bridge, root, ada) = ([7; 32], [8; 32], [1; 32]);
        let policy = bridging_policy(&[ada], bridge);
        let call = [3; 32];
        let own = bridged(None, None);
        let grace = bridged(Some("100"), None);
        assert_eq!(
            current(&policy, bridge, root, &call, Some(&own)),
            Ok((
                2,
                Directions {
                    publish: false,
                    subscribe: true
                }
            ))
        );
        assert_eq!(
            current(&policy, bridge, root, &call, Some(&grace)),
            Ok((
                2,
                Directions {
                    publish: true,
                    subscribe: false
                }
            ))
        );
        assert!(
            current(&policy, ada, ada, &call, Some(&grace)).is_err(),
            "a member's own device bridges nobody"
        );
        assert!(current(&policy, bridge, root, &policy.windows, Some(&own)).is_err());
    }

    /// A guest linked to a person stands or falls with them: refused once
    /// the person is no member (removed or banned), and a denial of the
    /// person holds for the guest.
    #[test]
    fn a_guest_linked_to_a_person_is_held_to_that_persons_standing() {
        let (bridge, root, ada, bo) = ([7; 32], [8; 32], [1; 32], [2; 32]);
        let call = [3; 32];
        let linked = bridged(Some("100"), Some(ada));
        let mut policy = bridging_policy(&[ada], bridge);
        assert!(current(&policy, bridge, root, &call, Some(&linked)).is_ok());

        let removed = bridging_policy(&[bo], bridge);
        assert_eq!(
            current(&removed, bridge, root, &call, Some(&linked)),
            Err("the person this account is linked to is no member of the room".into())
        );

        policy
            .calls
            .as_mut()
            .unwrap()
            .calls
            .get_mut(&call)
            .unwrap()
            .denied
            .insert(
                ada,
                DirectionsV1 {
                    publish: true,
                    subscribe: false,
                },
            );
        assert_eq!(
            current(&policy, bridge, root, &call, Some(&linked)),
            Ok((
                2,
                Directions {
                    publish: false,
                    subscribe: false
                }
            )),
            "Ada may not speak, so neither may her Discord account"
        );
        assert!(
            current(
                &policy,
                bridge,
                root,
                &call,
                Some(&bridged(Some("200"), None))
            )
            .is_ok_and(|(_, directions)| directions.publish),
            "an unlinked guest is not Ada"
        );
    }

    #[test]
    fn a_bridged_participant_is_shown_only_as_an_appearance_may_be() {
        assert!(validate_bridged(&bridged(None, None)).is_ok());
        assert!(validate_bridged(&bridged(Some("100"), Some([1; 32]))).is_ok());
        for broken in [
            Bridged {
                service: "Discord".into(),
                ..bridged(None, None)
            },
            Bridged {
                name: " Grace".into(),
                ..bridged(Some("100"), None)
            },
            Bridged {
                name: "Gr\u{202E}ace".into(),
                ..bridged(Some("100"), None)
            },
            Bridged {
                avatar: Some("not a hash".into()),
                ..bridged(Some("100"), None)
            },
            Bridged {
                account: Some("1 00".into()),
                ..bridged(None, None)
            },
        ] {
            assert!(validate_bridged(&broken).is_err(), "{broken:?}");
        }
    }

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
