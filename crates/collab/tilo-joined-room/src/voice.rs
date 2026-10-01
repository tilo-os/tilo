//! A device's participation in a call of a room it joined.
//!
//! The room's governor records each call in the room's governance, bound to
//! one voice channel of a chat record; see [`tilo_relay_protocol::voice`]. A
//! [`Participation`] finds the active call bound to a channel (or, for the
//! room's own person, asks the governor to start one), joins it over the
//! room's relay session and keeps the governor's current grant. That grant
//! is the whole admission to the relay's voice carrier: the participation
//! is the carrier's [`ClientAuthority`] while the grant still admits its
//! scope, the device is still a member and the last grant is younger than
//! [`PARTICIPATION_TTL`].
//!
//! The caller routes the session's [`VoiceEvent`]s to [`Participation::apply`],
//! repeats [`Participation::heartbeat`] every
//! [`tilo_relay_protocol::voice::HEARTBEAT`] and, when it dials the room
//! again, [`Participation::rejoin`]s over the new session: the relay ties a
//! participation to the connection it joined over.
//!
//! A room's governance changes (a call starting, a mute) advance its
//! continuity; they do not end a participation. Membership is checked on
//! every carrier authorization and custody on every heartbeat.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant, SystemTime};

use parking_lot::Mutex;
use tilo_desktop_model::RoomId;
use tilo_moq::client::{Client, ClientAuthority, Connection, ConnectionScope};
use tilo_moq::relay::Direction;
use tilo_relay::coedit::{RelayCoeditSession, VoiceEvent};
use tilo_relay_protocol::voice::{CallOperation, Directions, PARTICIPATION_TTL, VoiceGrant};

use crate::{NativeCommittedView, NativeWorkspaceOwner};

/// The voice channel a call is bound to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Channel {
    /// The chat app.
    pub app: String,
    /// The chat record holding the channel.
    pub record: String,
    /// The voice channel.
    pub channel: String,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// Waiting for the governor to start the call this request asked for.
    Starting([u8; 16]),
    /// Waiting for the first grant of the call.
    Joining,
    Joined,
}

struct State {
    phase: Phase,
    call: Option<[u8; 32]>,
    grant: Option<VoiceGrant>,
    /// Advances with every grant.
    revision: u64,
    refused: Option<String>,
    carrier: Option<ConnectionScope>,
    generation: u64,
}

/// Stop hooks use only this independent slot. The weak client avoids a
/// Client -> Authority -> Participation ownership cycle.
#[derive(Default)]
struct Stops {
    client: Weak<Client>,
    capture: Option<Arc<dyn Fn(bool) + Send + Sync>>,
}

/// One device's participation in one call; see the [module docs](self).
pub struct Participation {
    owner: NativeWorkspaceOwner,
    room: RoomId,
    workspace: u64,
    device: [u8; 32],
    alive: AtomicBool,
    leaving: AtomicBool,
    base: Instant,
    /// Nanoseconds after `base` until which the last grant authorizes.
    authorized_until: AtomicU64,
    state: Mutex<State>,
    changed: tokio::sync::Notify,
    stops: Mutex<Stops>,
}

impl Participation {
    /// Join the call bound to `channel` in `room` over the room's relay
    /// `session`, starting it when none is active and this device's person
    /// governs the room. `check` vets the device's custody.
    ///
    /// # Errors
    /// The device is no member, the channel is no voice channel, no call is
    /// active and the device may not start one, or the session is gone.
    pub fn open(
        owner: NativeWorkspaceOwner,
        room: RoomId,
        channel: &Channel,
        session: &RelayCoeditSession,
        check: impl Fn(&NativeCommittedView) -> Result<(), String>,
    ) -> Result<Arc<Self>, String> {
        let (workspace, device, call) = owner.with_current(room, |view| {
            check(view)?;
            view.peer()
                .permissions(view.device().device_public_key, SystemTime::now())?;
            let binding = view.peer().resolve_media_binding(
                &channel.app,
                &channel.record,
                &channel.channel,
            )?;
            let policy = view.peer().validated_media_policy()?;
            let mut calls = policy.iter().flat_map(|policy| {
                policy.calls.iter().filter(|(_, call)| {
                    call.binding == binding
                        && call.phase == tilo_peer_state::room::media::CallPhaseV1::Active
                })
            });
            let call = calls.next().map(|(id, _)| *id);
            if calls.next().is_some() {
                return Err("ambiguous governed call".into());
            }
            if call.is_none() && view.governor().user_public_key != view.device().user_public_key {
                return Err("no call is active on this channel".into());
            }
            Ok((view.workspace(), view.device().device_public_key, call))
        })?;
        if session.workspace_id() != workspace {
            return Err("the session belongs to another room".into());
        }
        let phase = if call.is_some() {
            Phase::Joining
        } else {
            let mut request = [0; 16];
            getrandom::fill(&mut request).map_err(|error| error.to_string())?;
            Phase::Starting(request)
        };
        let participation = Arc::new(Self {
            owner,
            room,
            workspace,
            device,
            alive: AtomicBool::new(true),
            leaving: AtomicBool::new(false),
            base: Instant::now(),
            authorized_until: AtomicU64::new(0),
            state: Mutex::new(State {
                phase,
                call,
                grant: None,
                revision: 0,
                refused: None,
                carrier: None,
                generation: 0,
            }),
            changed: tokio::sync::Notify::new(),
            stops: Mutex::new(Stops::default()),
        });
        match (phase, call) {
            (Phase::Starting(request), _) => session.voice_call(
                request,
                CallOperation::Start {
                    app_id: channel.app.clone(),
                    record_id: channel.record.clone(),
                    channel_id: channel.channel.clone(),
                    allowed: Directions {
                        publish: true,
                        subscribe: true,
                    },
                },
            ),
            (_, Some(call)) => session.voice_join(call, None),
            (_, None) => return Err("governed call missing".into()),
        }
        .map_err(|_| "the room's relay session is gone")?;
        Ok(participation)
    }

    /// The room's relay workspace.
    #[must_use]
    pub const fn workspace(&self) -> u64 {
        self.workspace
    }

    /// This participation's id, once granted.
    #[must_use]
    pub fn id(&self) -> Option<[u8; 32]> {
        self.state
            .lock()
            .grant
            .as_ref()
            .map(|grant| grant.participation)
    }

    /// Apply what the governor said; whether it was about this
    /// participation. `claimed` tells whether another participation of this
    /// device already holds a participation id.
    pub fn apply(
        &self,
        event: &VoiceEvent,
        session: Option<&RelayCoeditSession>,
        claimed: impl Fn([u8; 32]) -> bool,
    ) -> bool {
        if self.live().is_err() {
            return false;
        }
        let mut state = self.state.lock();
        match event {
            VoiceEvent::Result { request, result } => {
                if state.phase != Phase::Starting(*request) {
                    return false;
                }
                match result {
                    Ok(call) if call.active => {
                        state.phase = Phase::Joining;
                        state.call = Some(call.call);
                        drop(state);
                        if session
                            .is_none_or(|session| session.voice_join(call.call, None).is_err())
                        {
                            self.refuse("the room's relay session is gone".into());
                        }
                    }
                    Ok(_) => {
                        drop(state);
                        self.refuse("the call did not start".into());
                    }
                    Err(reason) => {
                        drop(state);
                        self.refuse(reason.clone());
                    }
                }
                true
            }
            VoiceEvent::Granted(grant) => {
                let ours = state.call == Some(grant.call)
                    && (state
                        .grant
                        .as_ref()
                        .is_some_and(|current| current.participation == grant.participation)
                        || (state.phase == Phase::Joining && !claimed(grant.participation)));
                if !ours {
                    return false;
                }
                state.phase = Phase::Joined;
                state.grant = Some((**grant).clone());
                state.revision = state.revision.saturating_add(1);
                drop(state);
                self.extend();
                self.changed.notify_waiters();
                true
            }
            VoiceEvent::Refused { call, reason } => {
                if state.call != Some(*call) || matches!(state.phase, Phase::Starting(_)) {
                    return false;
                }
                drop(state);
                self.refuse(reason.clone());
                true
            }
        }
    }

    /// Repeat the join as this participation's heartbeat, after `check`
    /// vets the device's custody again; a failed check ends it.
    ///
    /// # Errors
    /// The participation ended, the check failed or the session is gone.
    pub fn heartbeat(
        &self,
        session: &RelayCoeditSession,
        check: impl Fn(&NativeCommittedView) -> Result<(), String>,
    ) -> Result<(), String> {
        self.live()?;
        if let Err(error) = self.owner.with_current(self.room, |view| {
            check(view)?;
            view.peer()
                .permissions(self.device, SystemTime::now())
                .map(|_| ())
        }) {
            self.stop();
            return Err(error);
        }
        // Only a granted participation of this session has an id to repeat.
        let (call, id) = {
            let state = self.state.lock();
            (
                state.call.filter(|_| state.phase == Phase::Joined),
                state.grant.as_ref().map(|grant| grant.participation),
            )
        };
        if let (Some(call), Some(id)) = (call, id) {
            session
                .voice_join(call, Some(id))
                .map_err(|_| "the room's relay session is gone")?;
        }
        Ok(())
    }

    /// Join again over `session`, a new connection to the room's relay. The
    /// current grant stays until the governor grants the new participation;
    /// its carrier then no longer matches and is replaced.
    ///
    /// # Errors
    /// The participation ended or the session is gone.
    pub fn rejoin(&self, session: &RelayCoeditSession) -> Result<(), String> {
        self.live()?;
        let call = {
            let mut state = self.state.lock();
            let call = state.call;
            if call.is_some() {
                state.phase = Phase::Joining;
            }
            call
        };
        let Some(call) = call else {
            // The start went over the old connection; its answer is lost.
            self.refuse("the room reconnected while the call was starting".into());
            return Err("the room reconnected while the call was starting".into());
        };
        session
            .voice_join(call, None)
            .map_err(|_| "the room's relay session is gone".into())
    }

    /// Stop locally, then tell the governor this participation ends. Only
    /// the first call sends.
    pub fn leave(&self, session: Option<&RelayCoeditSession>) {
        self.stop();
        if self.leaving.swap(true, Ordering::SeqCst) {
            return;
        }
        let (call, id) = {
            let state = self.state.lock();
            (
                state.call,
                state.grant.as_ref().map(|grant| grant.participation),
            )
        };
        if let (Some(call), Some(id), Some(session)) = (call, id, session) {
            let _ = session.voice_leave(call, id);
        }
    }

    /// Wait for the governor's first grant, or its refusal.
    ///
    /// # Errors
    /// The governor refused, the participation ended or `deadline` passed.
    pub async fn granted(&self, deadline: Duration) -> Result<(), String> {
        tokio::time::timeout(deadline, async {
            loop {
                let changed = self.changed.notified();
                tokio::pin!(changed);
                changed.as_mut().enable();
                {
                    let state = self.state.lock();
                    if let Some(reason) = &state.refused {
                        return Err(reason.clone());
                    }
                    if state.grant.is_some() {
                        return Ok(());
                    }
                }
                self.live()?;
                changed.await;
            }
        })
        .await
        .map_err(|_| "the governor did not grant the call in time".to_string())?
    }

    /// Wakes on every grant and on the end of the participation.
    #[must_use]
    pub const fn changed(&self) -> &tokio::sync::Notify {
        &self.changed
    }

    /// The governor's current grant.
    #[must_use]
    pub fn grant(&self) -> Option<VoiceGrant> {
        self.state.lock().grant.clone()
    }

    /// Advances with every grant, heartbeat answers included.
    #[must_use]
    pub fn revision(&self) -> u64 {
        self.state.lock().revision
    }

    /// Why the governor refused, if it did.
    #[must_use]
    pub fn refused(&self) -> Option<String> {
        self.state.lock().refused.clone()
    }

    /// The carrier connection the current grant admits, recorded as the
    /// current carrier; `None` when the grant admits no route or its token
    /// expired (the next heartbeat brings a fresh one).
    ///
    /// # Errors
    /// No grant yet, or the carrier generations are exhausted.
    pub fn next_carrier(&self) -> Result<Option<Connection>, String> {
        let now = tilo_clock::try_unix_now_ns().ok_or("clock precedes the epoch")?;
        let mut state = self.state.lock();
        let grant = state.grant.as_ref().ok_or("no grant")?;
        let route = (grant.directions.publish && grant.publication.is_some())
            || (grant.directions.subscribe && !grant.subscriptions.is_empty());
        if !route || grant.expires_at_ns <= now {
            return Ok(None);
        }
        let generation = state
            .generation
            .checked_add(1)
            .ok_or("carrier generations exhausted")?;
        let connection = Connection {
            scope: scope(grant, generation),
            authorization: grant.token.clone(),
        };
        state.generation = generation;
        state.carrier = Some(connection.scope.clone());
        Ok(Some(connection))
    }

    /// The generation of the current carrier.
    #[must_use]
    pub fn carrier_generation(&self) -> Option<u64> {
        self.state
            .lock()
            .carrier
            .as_ref()
            .map(|carrier| carrier.carrier_generation)
    }

    /// Whether the current carrier's scope is no longer the one the current
    /// grant admits (the roster or directions changed).
    #[must_use]
    pub fn stale(&self) -> bool {
        let state = self.state.lock();
        match (&state.grant, &state.carrier) {
            (Some(grant), Some(carrier)) => !admits(grant, carrier),
            _ => false,
        }
    }

    /// Install `client` as the current carrier, closed by every stop.
    ///
    /// # Errors
    /// The participation no longer authorizes a carrier.
    pub fn set_client(&self, client: &Arc<Client>) -> Result<(), String> {
        self.inspect_current(|_, _| {
            self.stops.lock().client = Arc::downgrade(client);
            Ok(())
        })
    }

    /// Forget the current carrier and close it.
    pub fn clear_carrier(&self) {
        self.state.lock().carrier = None;
        self.pause();
    }

    /// Run `stop` on every stop: `true` when the participation ends.
    pub fn set_capture_stop(&self, stop: Arc<dyn Fn(bool) + Send + Sync>) {
        self.stops.lock().capture = Some(stop);
    }

    /// Close the carrier and pause capture; the participation lives on.
    pub fn pause(&self) {
        self.stop_resources(false);
    }

    /// End the participation locally.
    pub fn stop(&self) {
        self.alive.store(false, Ordering::SeqCst);
        self.stop_resources(true);
        self.changed.notify_waiters();
    }

    /// Whether the participation has not ended.
    ///
    /// # Errors
    /// It ended.
    pub fn live(&self) -> Result<(), String> {
        self.alive
            .load(Ordering::SeqCst)
            .then_some(())
            .ok_or_else(|| "the participation ended".into())
    }

    /// Whether the participation is leaving.
    #[must_use]
    pub fn leaving(&self) -> bool {
        self.leaving.load(Ordering::SeqCst)
    }

    /// Whether the last grant is younger than [`PARTICIPATION_TTL`].
    #[must_use]
    pub fn authorized(&self) -> bool {
        self.base.elapsed().as_nanos() < u128::from(self.authorized_until.load(Ordering::Acquire))
    }

    /// Read the current grant while the participation is live, its device
    /// still a member and its last grant unexpired.
    ///
    /// # Errors
    /// Any of those no longer holds, or `read` fails.
    pub fn inspect_current<T>(
        &self,
        read: impl FnOnce(u64, &VoiceGrant) -> Result<T, String>,
    ) -> Result<T, String> {
        self.live()?;
        if self.leaving() {
            return Err("the participation is leaving".into());
        }
        if !self.authorized() {
            return Err("the participation lapsed".into());
        }
        self.owner.with_current(self.room, |view| {
            view.peer().permissions(self.device, SystemTime::now())?;
            let (revision, grant) = {
                let state = self.state.lock();
                (state.revision, state.grant.clone().ok_or("no grant")?)
            };
            read(revision, &grant)
        })
    }

    fn refuse(&self, reason: String) {
        self.state.lock().refused = Some(reason);
        self.stop();
    }

    fn extend(&self) {
        let until = self.base.elapsed().saturating_add(PARTICIPATION_TTL);
        self.authorized_until.store(
            u64::try_from(until.as_nanos()).unwrap_or(u64::MAX),
            Ordering::Release,
        );
    }

    fn stop_resources(&self, terminal: bool) {
        let stops = self.stops.lock();
        if let Some(client) = stops.client.upgrade() {
            client.close();
        }
        if let Some(stop) = &stops.capture {
            stop(terminal);
        }
    }
}

impl Drop for Participation {
    fn drop(&mut self) {
        self.stop();
    }
}

impl ClientAuthority for Participation {
    fn current(&self, scope: &ConnectionScope, direction: Direction, name: &str) -> bool {
        let carrier = self.state.lock().carrier.clone();
        if carrier.as_ref() != Some(scope) {
            return false;
        }
        self.inspect_current(|_, grant| {
            if !admits(grant, scope) {
                return Err("the grant no longer admits this carrier".into());
            }
            match direction {
                Direction::Publish if scope.publication.as_deref() == Some(name) => Ok(()),
                Direction::Subscribe
                    if scope.subscriptions.iter().any(|allowed| allowed == name) =>
                {
                    Ok(())
                }
                _ => Err("namespace not granted in this direction".into()),
            }
        })
        .is_ok()
    }
}

/// The carrier scope `grant` admits, as carrier `generation`.
fn scope(grant: &VoiceGrant, generation: u64) -> ConnectionScope {
    ConnectionScope {
        endpoint: grant.endpoint.clone(),
        fingerprints: grant.fingerprints.clone(),
        owner_epoch: grant.participation,
        carrier_generation: generation,
        publication: grant
            .directions
            .publish
            .then(|| grant.publication.clone())
            .flatten(),
        subscriptions: if grant.directions.subscribe {
            grant.subscriptions.clone()
        } else {
            Vec::new()
        },
    }
}

/// Whether carrier `scope` is still the one `grant` admits.
fn admits(grant: &VoiceGrant, scope: &ConnectionScope) -> bool {
    self::scope(grant, scope.carrier_generation) == *scope
}
