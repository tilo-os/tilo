//! Calls in the workspaces this relay governs.
//!
//! A call is workspace governance: [`WorkspaceMediaV1`] in the workspace document, which the
//! governor authors and every member verifies. Any member opens a voice
//! channel's call to join it ([`CallOperation::Open`]): the relay, as the
//! governor, starts the channel's call when it has none, and restarts it
//! when it was ended. The relay's own user, and any member the workspace's
//! governance grants its `call` action (no role preset below the owner's
//! does), ends and moderates calls; any member device joins an active one
//! and gets a grant naming its participation and the call's roster (see
//! [`tilo_relay_protocol::voice`]).
//!
//! A call has no participant limit. A device takes part in one call at a
//! time: joining another call ends its earlier participation, so a call's
//! participants are bounded by the workspace's member devices, not by a constant.
//!
//! What a participation may do is the workspace's current policy for its device:
//! the call is active, the device is a member, and neither the call's
//! directions nor a denial of its person exclude it. It is re-derived after
//! every governance decision and every second, so removing a member or
//! ending a call ends their participation within one observation of the
//! decision. The members' devices carry the call's media between them on the
//! workspace's live plane (`tilo_live`), by the keys their participation and
//! the workspace's channels give them.
//!
//! A workspace this relay only carries, governed elsewhere or by no one (a
//! person's own workspace, synced across their devices through it), has
//! calls all the same: every workspace has voice. Its calls are held here
//! ([`carried_call_room`]), not in governance this relay cannot author:
//! any device subscribed to the workspace, and so admitted to it, opens a voice
//! channel's call and takes part with both directions, and a call stands
//! until the relay restarts. The workspace's admission is the only authority:
//! each envelope is answered only for a device still subscribed, so one
//! that leaves the workspace stops being granted, and its participation ends
//! within [`PARTICIPATION_TTL`] of its last heartbeat. No one moderates
//! such a call (there is no governor to).
//!
//! Every device that asked for a workspace's calls ([`Workspaces::voice_calls`]) is
//! sent the workspace's call list again whenever it changes: someone joins or
//! leaves a call, a participation ends, or a call starts or ends. A voice
//! channel shows who is in it the moment that changes, without anyone asking
//! again.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ed25519_dalek::SigningKey;
use tilo_peer_state::workspace::media::{self, CallPhaseV1, DirectionsV1, WorkspaceMediaV1};
use tilo_peer_state::{GovernedAccess, StatePeer};
use tilo_relay_protocol::voice::{
    CallOperation, CallState, CallSummary, Directions, PARTICIPATION_TTL, Showing, VoiceGrant,
    VoiceParticipant,
};
use tilo_relay_protocol::{AuthenticatedPeer, Envelope};
use tokio::sync::mpsc;
use web_time::SystemTime;

use tilo_action_policy::roles::governance;

use super::authority::WorkspaceOp;
use super::{GovernedWorkspace, Workspaces};
use tilo_lock::MutexRecover as _;

/// The longest record or channel id a call in a carried workspace is opened on.
const MAX_CARRIED_NAME: usize = 256;
/// How often participations are checked for silence, closed connections and
/// the passage of time.
pub(super) const OBSERVATION: Duration = Duration::from_secs(1);

/// One device in one call.
struct Participation {
    workspace: String,
    workspace_id: u64,
    call: [u8; 32],
    epoch: u64,
    device: [u8; 32],
    user: [u8; 32],
    directions: Directions,
    /// What it shows besides its voice, as it last said.
    showing: Showing,
    seen: Instant,
    outbound: mpsc::Sender<Envelope>,
}

#[derive(Default)]
struct State {
    participations: HashMap<[u8; 32], Participation>,
    watchers: HashMap<String, Vec<Watcher>>,
    /// Workspaces whose call list changed since their watchers were last told.
    changed: std::collections::BTreeSet<String>,
    /// The calls of each workspace this relay carries but does not govern,
    /// by [`carried_call_room`]: the policy it would otherwise read from the
    /// workspace's governance.
    carried: HashMap<String, WorkspaceMediaV1>,
}

/// The name a workspace this relay carries, but does not govern, has among the
/// workspaces with calls: no governed workspace is named so, as those are uuids.
pub(crate) fn carried_call_room(workspace_id: u64) -> String {
    format!("{CARRIED}{workspace_id:016x}")
}

const CARRIED: &str = "carried-";

/// Whether `workspace` names a workspace this relay only carries.
fn carried(workspace: &str) -> bool {
    workspace.starts_with(CARRIED)
}

/// A device told a workspace's call list whenever it changes.
struct Watcher {
    workspace_id: u64,
    device: [u8; 32],
    user: [u8; 32],
    outbound: mpsc::Sender<Envelope>,
}

impl Participation {
    /// It as the participants of its call see it.
    fn participant(&self) -> VoiceParticipant {
        VoiceParticipant {
            device_public_key: self.device,
            user_public_key: self.user,
            publishes: self.directions.publish,
            showing: self.shown(),
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

impl State {
    fn in_call(&self, call: &[u8; 32]) -> impl Iterator<Item = (&[u8; 32], &Participation)> {
        self.participations
            .iter()
            .filter(move |(_, participation)| participation.call == *call)
    }

    fn remove(&mut self, id: &[u8; 32]) -> Option<Participation> {
        self.participations.remove(id)
    }

    /// End the participations of `device` its join of `call` replaces. The
    /// other calls it left, whose participants hear the change.
    fn end_device(&mut self, device: [u8; 32], call: &[u8; 32]) -> Vec<[u8; 32]> {
        let earlier: Vec<[u8; 32]> = self
            .participations
            .iter()
            .filter(|(_, existing)| existing.device == device)
            .map(|(id, _)| *id)
            .collect();
        let mut left = Vec::new();
        for id in earlier {
            if let Some(existing) = self.remove(&id) {
                self.changed.insert(existing.workspace.clone());
                if existing.call != *call {
                    left.push(existing.call);
                }
            }
        }
        left.sort_unstable();
        left.dedup();
        left
    }
}

/// The calls of every workspace this relay governs.
#[derive(Default)]
pub(super) struct Calls {
    state: Mutex<State>,
    /// A watch observes the participations ([`Workspaces::watch_calls`]): only
    /// while there are any.
    watching: std::sync::atomic::AtomicBool,
}

impl Calls {
    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock_recover()
    }

    fn grant(state: &State, id: &[u8; 32]) -> Option<VoiceGrant> {
        let own = state.participations.get(id)?;
        let mut participants: Vec<VoiceParticipant> = state
            .in_call(&own.call)
            .map(|(_, participation)| participation.participant())
            .collect();
        participants.sort_by_key(|participant| participant.device_public_key);
        Some(VoiceGrant {
            call: own.call,
            epoch: own.epoch,
            participation: *id,
            directions: own.directions,
            participants,
        })
    }

    /// Send every participant of `calls` its current grant.
    fn announce(state: &State, calls: &[[u8; 32]]) {
        for (id, participation) in &state.participations {
            if !calls.contains(&participation.call) {
                continue;
            }
            if let Some(grant) = Self::grant(state, id) {
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
    fn announce_to_others(state: &State, call: [u8; 32], joined: [u8; 32]) {
        for (other, participation) in &state.participations {
            if *other == joined || participation.call != call {
                continue;
            }
            if let Some(grant) = Self::grant(state, other) {
                let _ = participation.outbound.try_send(Envelope::VoiceGranted {
                    workspace_id: participation.workspace_id,
                    grant: Box::new(grant),
                });
            }
        }
    }

    /// Re-derive every participation in `workspace` from its committed policy:
    /// refuse those that may no longer take part, update the others, and
    /// send the participants of every call that changed their new grants.
    fn observe(&self, workspace: &GovernedWorkspace) {
        let policy = Policy::read(workspace);
        let mut state = self.state();
        let mut changed = Vec::new();
        let mut refused = Vec::new();
        for (id, participation) in &mut state.participations {
            if participation.workspace != workspace.workspace() {
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
        Self::announce(&state, &changed);
        if !changed.is_empty() {
            state.changed.insert(workspace.workspace().to_owned());
        }
    }

    /// End participations not heard from for [`PARTICIPATION_TTL`] or whose
    /// connection closed. Returns the workspaces that still have participants.
    fn expire(&self) -> Vec<String> {
        let mut state = self.state();
        let now = tilo_clock::now();
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
                    workspace = %participation.workspace,
                    call = %hex::encode(participation.call),
                    unheard_ms = now.duration_since(participation.seen).as_millis(),
                    connection_closed = participation.outbound.is_closed(),
                    "voice participation expired"
                );
                changed.push(participation.call);
                state.changed.insert(participation.workspace);
            }
        }
        changed.sort_unstable();
        changed.dedup();
        Self::announce(&state, &changed);
        let mut workspaces: Vec<String> = state
            .participations
            .values()
            .map(|participation| participation.workspace.clone())
            .collect();
        drop(state);
        workspaces.sort_unstable();
        workspaces.dedup();
        workspaces
    }
}

/// The workspace's committed call policy and who it admits, read once per
/// observation.
struct Policy {
    calls: Option<WorkspaceMediaV1>,
    access: GovernedAccess,
    /// Whether the workspace is one this relay carries: its admission, which
    /// every call envelope is checked against, decides who is a member.
    carried: bool,
}

impl Policy {
    /// `workspace`'s policy. A relay that takes part in the workspace's channels resolved
    /// every call's binding through their keys when it opened the call, so
    /// a call bound to a Chat its members write sealed still stands; any
    /// other replica keeps such a Chat as ciphertext and fails closed.
    fn read(workspace: &GovernedWorkspace) -> Result<Self, String> {
        let peer = workspace.peer();
        let calls = if workspace.seals() {
            peer.media().keyed_media_policy()?
        } else {
            peer.validated_media_policy()?
        };
        let now = tilo_clock::wall();
        Ok(Self {
            calls,
            access: peer.governance().governed_access(now)?,
            carried: false,
        })
    }

    /// The policy of `workspace`, one this relay carries: the calls it holds for
    /// it.
    fn carried(calls: Option<WorkspaceMediaV1>) -> Self {
        Self {
            calls,
            access: GovernedAccess::default(),
            carried: true,
        }
    }
}

/// What `device` of person `user` may do in `call` under `policy`: the
/// call's epoch and directions, or why it may not take part. A person's
/// every certified device takes part, as the relay admits them all to the
/// workspace; the relay verified the certificate when the device connected.
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
    if !member(policy, device, user) {
        return Err("not a member of the room".into());
    }
    let denied = |pick: fn(&DirectionsV1) -> bool| call.denied.get(&user).is_some_and(pick);
    Ok((
        call.epoch,
        Directions {
            publish: call.allowed.publish && !denied(|value| value.publish),
            subscribe: call.allowed.subscribe && !denied(|value| value.subscribe),
        },
    ))
}

impl GovernedWorkspace {
    /// The call binding of voice channel `channel` of `record` of `app`. A
    /// Chat its members write sealed is read through the channel keys this
    /// relay holds as a member of the workspace's channels: members of a hosted workspace seal
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
    /// keys this relay holds as a member.
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

    /// A relay built without the browser listener takes part in no channels, so
    /// it cannot read a sealed Chat.
    #[cfg(not(any(feature = "agents", feature = "web")))]
    #[expect(
        clippy::unused_async,
        reason = "the same call as the build's that takes part in channels"
    )]
    async fn sealed_call_binding(
        &self,
        _app: &str,
        _record: &str,
        _channel: &str,
    ) -> Result<media::ChatCallBindingV1, String> {
        Err("the Chat is sealed and this relay does not take part in its channel".into())
    }
}

/// Whether `device` of person `user` is a member of the workspace under `policy`.
fn member(policy: &Policy, device: [u8; 32], user: [u8; 32]) -> bool {
    policy.carried
        || policy.access.member_devices.contains(&device)
        || policy.access.member_user_roots.contains(&user)
}

/// The workspace's calls under `policy`, with who is in each now.
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
                .map(|(_, participation)| participation.participant())
                .collect();
            participants.sort_by_key(|participant| participant.device_public_key);
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

/// Apply `operation` to the workspace policy of `candidate`, authored by the
/// governor. Returns the call it changed. Moderation that changes nothing
/// leaves the workspace unchanged.
fn transition(
    candidate: &mut StatePeer,
    governor: &SigningKey,
    operation: &CallOperation,
    binding: Option<media::ChatCallBindingV1>,
    now: SystemTime,
) -> Result<[u8; 32], String> {
    let current = media::parse(candidate.admitted_workspace().ok_or("missing room")?)
        .map_err(|error| error.to_string())?;
    let mut policy = match current.clone() {
        Some(policy) => policy,
        None => WorkspaceMediaV1 {
            version: 1,
            workspace: *uuid::Uuid::parse_str(candidate.workspace_id())
                .map_err(|error| error.to_string())?
                .as_bytes(),
            authorization_revision: 1,
            authorization_basis: media::security_basis(
                candidate.admitted_workspace().ok_or("missing room")?,
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
fn open(
    policy: &mut WorkspaceMediaV1,
    binding: media::ChatCallBindingV1,
) -> Result<[u8; 32], String> {
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
    policy: &mut WorkspaceMediaV1,
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
    policy: &mut WorkspaceMediaV1,
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

impl Workspaces {
    /// Open, start, end or moderate a call of `workspace` for `caller`: opening a channel's call takes a member, everything else a
    /// device of the relay's user, or of a member the workspace lets run its
    /// calls. In a workspace this relay carries, the caller is a member by its
    /// subscription and opening is all there is.
    pub(crate) async fn voice_call(
        &self,
        workspace: &str,
        caller: &AuthenticatedPeer,
        operation: &CallOperation,
    ) -> Result<CallState, String> {
        if carried(workspace) {
            return self.carried_voice_call(workspace, operation);
        }
        self.voice_call_as(workspace, &caller.cert, operation).await
    }

    /// Open the call of a voice channel of `workspace`, one this relay carries:
    /// started when the channel has none, restarted when it ended. Its
    /// Chat is the members' to read, sealed with keys this relay does not
    /// hold, so the call is bound to the channel as named.
    fn carried_voice_call(
        &self,
        workspace: &str,
        operation: &CallOperation,
    ) -> Result<CallState, String> {
        let CallOperation::Open {
            app_id,
            record_id,
            channel_id,
        } = operation
        else {
            return Err("no one moderates the calls of a room without a governor here".into());
        };
        if app_id != "so.tilo.chat"
            || record_id.len() > MAX_CARRIED_NAME
            || channel_id.is_empty()
            || channel_id.len() > MAX_CARRIED_NAME
            || [record_id, channel_id]
                .iter()
                .any(|name| name.chars().any(char::is_control))
        {
            return Err("calls are opened on a Chat's voice channel".into());
        }
        let binding = media::ChatCallBindingV1 {
            app_id: app_id.clone(),
            record_id: record_id.clone(),
            channel_id: channel_id.clone(),
            channel_object: String::new(),
            chat_workspace_id: String::new(),
        };
        let calls = &self.inner.calls;
        let mut state = calls.state();
        let policy = state
            .carried
            .entry(workspace.to_owned())
            .or_insert_with(|| WorkspaceMediaV1 {
                version: 1,
                workspace: [0; 16],
                authorization_revision: 0,
                authorization_basis: [0; 32],
                calls: BTreeMap::default(),
            });
        let before = policy.clone();
        let call = open(policy, binding)?;
        let entry = policy.calls.get(&call).ok_or("the call vanished")?;
        let opened = CallState {
            call,
            epoch: entry.epoch,
            active: entry.phase == CallPhaseV1::Active,
            allowed: from_v1(entry.allowed),
        };
        if *policy != before {
            tracing::info!(%workspace, call = %hex::encode(call), "call opened in a carried room");
            state.changed.insert(workspace.to_owned());
        }
        drop(state);
        self.tell_call_lists();
        Ok(opened)
    }

    /// The workspace of `workspace` and the call policy it is under now: its
    /// governance's when this relay governs it, else the calls it holds
    /// for a workspace it carries.
    async fn call_policy(&self, workspace: &str) -> Result<(u64, Policy), String> {
        if let Some(workspace_id) = workspace
            .strip_prefix(CARRIED)
            .and_then(|id| u64::from_str_radix(id, 16).ok())
        {
            let calls = self.inner.calls.state().carried.get(workspace).cloned();
            return Ok((workspace_id, Policy::carried(calls)));
        }
        let governed = self
            .workspace(workspace)
            .await
            .map_err(|error| error.to_string())?;
        let governed = governed.lock().await;
        Ok((governed.workspace_id(), Policy::read(&governed)?))
    }

    /// [`Self::voice_call`] for the device `caller` certifies, however it
    /// reached the relay: the envelope and the workspace's `call` action share
    /// it, and with it the one authorization.
    pub async fn voice_call_as(
        &self,
        workspace: &str,
        caller: &tilo_user_identity::DeviceCert,
        operation: &CallOperation,
    ) -> Result<CallState, String> {
        let workspace = self
            .workspace(workspace)
            .await
            .map_err(|error| error.to_string())?;
        let mut workspace = workspace.lock().await;
        if matches!(operation, CallOperation::Open { .. }) {
            // Joining a voice channel is any member's: who may speak or
            // hear in it is the call's directions and denials.
            let access = workspace
                .peer()
                .governance()
                .governed_access(tilo_clock::wall())?;
            if !access.member_devices.contains(&caller.device_public_key)
                && !access.member_user_roots.contains(&caller.user_public_key)
            {
                return Err("not a member of the room".into());
            }
        } else {
            self.authorize(
                caller,
                Some(&workspace),
                WorkspaceOp::Govern(governance::CALL),
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
            } => Some(
                workspace
                    .call_binding(app_id, record_id, channel_id)
                    .await?,
            ),
            // A restarted call keeps its channel, which must still be one.
            CallOperation::Restart { call, .. } => {
                let bound = media::parse(
                    workspace
                        .peer()
                        .admitted_workspace()
                        .ok_or("missing room")?,
                )
                .map_err(|error| error.to_string())?
                .and_then(|policy| policy.calls.get(call).map(|call| call.binding.clone()));
                match bound {
                    Some(bound) => Some(
                        workspace
                            .call_binding(&bound.app_id, &bound.record_id, &bound.channel_id)
                            .await?,
                    ),
                    None => None,
                }
            }
            _ => None,
        };
        let now = tilo_clock::wall();
        let mut candidate = workspace.peer().clone();
        super::prepare_governance(&mut candidate, &self.inner.host.key, now)?;
        let call = transition(
            &mut candidate,
            &self.inner.host.key,
            operation,
            binding,
            now,
        )?;
        workspace.commit(candidate, &self.inner.host.key, now, |record, _| {
            Ok(record.collaboration.clone())
        })?;
        self.settle(&mut workspace).await?;
        let policy = media::parse(
            workspace
                .peer()
                .admitted_workspace()
                .ok_or("missing room")?,
        )
        .map_err(|error| error.to_string())?
        .ok_or("the room has no calls")?;
        let entry = policy.calls.get(&call).ok_or("the call vanished")?;
        tracing::info!(workspace = %workspace.workspace(), call = %hex::encode(call), epoch = entry.epoch, "call changed");
        self.inner
            .calls
            .state()
            .changed
            .insert(workspace.workspace().to_owned());
        drop(workspace);
        self.tell_call_lists();
        Ok(CallState {
            call,
            epoch: entry.epoch,
            active: entry.phase == CallPhaseV1::Active,
            allowed: from_v1(entry.allowed),
        })
    }

    /// Join `call` of `workspace` as `peer`, or continue `participation`. The
    /// answer is the participation's grant or why it may not take part;
    /// later grants go to `outbound` as the call changes.
    pub(crate) async fn voice_join(
        &self,
        workspace: &str,
        peer: &AuthenticatedPeer,
        call: [u8; 32],
        participation: Option<[u8; 32]>,
        outbound: mpsc::Sender<Envelope>,
    ) -> Result<VoiceGrant, String> {
        self.join(
            workspace,
            (peer.device_public_key, peer.user_public_key),
            call,
            participation,
            outbound,
        )
        .await
    }

    /// [`Self::voice_join`] for `(device, user)`.
    async fn join(
        &self,
        workspace: &str,
        (device, user): ([u8; 32], [u8; 32]),
        call: [u8; 32],
        participation: Option<[u8; 32]>,
        outbound: mpsc::Sender<Envelope>,
    ) -> Result<VoiceGrant, String> {
        let calls = &self.inner.calls;
        let (workspace_id, policy) = self.call_policy(workspace).await?;
        let (epoch, directions) = current(&policy, device, user, &call)?;
        let mut state = calls.state();
        let mut changed = participation.is_none();
        let id = if let Some(id) = participation {
            let existing = state
                .participations
                .get_mut(&id)
                .filter(|existing| existing.call == call && existing.device == device)
                .ok_or("the participation ended; join afresh")?;
            existing.seen = tilo_clock::now();
            existing.outbound = outbound;
            id
        } else {
            // A device is in one call at a time: its earlier participation,
            // in this call or another, ends with this join.
            let left = state.end_device(device, &call);
            Calls::announce(&state, &left);
            let id = loop {
                let id: [u8; 32] = rand::random();
                if !state.participations.contains_key(&id) {
                    break id;
                }
            };
            state.participations.insert(
                id,
                Participation {
                    workspace: workspace.to_owned(),
                    workspace_id,
                    call,
                    epoch,
                    device,
                    user,
                    directions,
                    showing: Showing::default(),
                    seen: tilo_clock::now(),
                    outbound,
                },
            );
            tracing::info!(%workspace, call = %hex::encode(call), "voice participant joined");
            id
        };
        changed |= state.participations.get(&id).is_some_and(|participation| {
            (participation.epoch, participation.directions) != (epoch, directions)
        });
        if let Some(existing) = state.participations.get_mut(&id) {
            existing.epoch = epoch;
            existing.directions = directions;
        }
        let grant = Calls::grant(&state, &id).ok_or("the participation ended")?;
        if changed {
            state.changed.insert(workspace.to_owned());
            Calls::announce_to_others(&state, call, id);
        }
        drop(state);
        self.watch_calls();
        self.tell_call_lists();
        Ok(grant)
    }

    /// The calls of `workspace` for `peer`, a member, with who is in each. From
    /// now on `outbound` is sent the list again whenever it changes, while
    /// the device is a member and its connection lasts.
    pub(crate) async fn voice_calls(
        &self,
        workspace: &str,
        peer: &AuthenticatedPeer,
        outbound: mpsc::Sender<Envelope>,
    ) -> Result<Vec<CallSummary>, String> {
        self.watch_call_list(
            workspace,
            (peer.device_public_key, peer.user_public_key),
            outbound,
        )
        .await
    }

    async fn watch_call_list(
        &self,
        workspace: &str,
        (device, user): ([u8; 32], [u8; 32]),
        outbound: mpsc::Sender<Envelope>,
    ) -> Result<Vec<CallSummary>, String> {
        let (workspace_id, policy) = self.call_policy(workspace).await?;
        if !member(&policy, device, user) {
            return Err("not a member of the room".into());
        }
        let mut state = self.inner.calls.state();
        let watchers = state.watchers.entry(workspace.to_owned()).or_default();
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

    /// Tell the watchers of every workspace whose call list changed the new
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
        for workspace in changed {
            let Ok((_, policy)) = self.call_policy(&workspace).await else {
                continue;
            };
            let mut state = self.inner.calls.state();
            let list = call_list(&state, &policy);
            let Some(watchers) = state.watchers.get_mut(&workspace) else {
                continue;
            };
            // A device that left the workspace, or whose connection closed, is
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
                state.watchers.remove(&workspace);
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

    fn leave(&self, device: [u8; 32], call: [u8; 32], participation: [u8; 32]) {
        let calls = &self.inner.calls;
        let mut state = calls.state();
        let owned = state
            .participations
            .get(&participation)
            .is_some_and(|existing| existing.call == call && existing.device == device);
        if owned && let Some(left) = state.remove(&participation) {
            Calls::announce(&state, &[call]);
            state.changed.insert(left.workspace);
            drop(state);
            self.tell_call_lists();
        }
    }

    /// Record what `participation` of `peer` in `call` shows besides its
    /// voice; when that changed, the call's participants get their grants
    /// again and the workspace's call list watchers the new list.
    pub(crate) fn voice_show(
        &self,
        peer: &AuthenticatedPeer,
        call: [u8; 32],
        participation: [u8; 32],
        showing: Showing,
    ) {
        self.show(peer.device_public_key, call, participation, showing);
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
        let workspace = existing.workspace.clone();
        Calls::announce(&state, &[call]);
        state.changed.insert(workspace);
        drop(state);
        self.tell_call_lists();
    }

    /// Re-derive the participations of `workspace` after a decision.
    pub(super) fn observe_calls(&self, workspace: &GovernedWorkspace) {
        self.inner.calls.observe(workspace);
    }

    /// Every [`OBSERVATION`]: end silent participations and re-derive the
    /// rest, while there are participations and the workspaces exist. Started by
    /// a join; with nobody in a call nothing runs.
    pub(super) fn watch_calls(&self) {
        let calls = &self.inner.calls;
        if calls
            .watching
            .swap(true, std::sync::atomic::Ordering::AcqRel)
        {
            return;
        }
        let workspaces = Arc::downgrade(&self.inner);
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(OBSERVATION);
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            // The first tick is at once; a participation just joined.
            tick.tick().await;
            loop {
                tick.tick().await;
                let Some(inner) = workspaces.upgrade() else {
                    break;
                };
                let this = Self { inner };
                this.observe_expired_calls().await;
                if !this.inner.calls.state().participations.is_empty() {
                    continue;
                }
                this.inner
                    .calls
                    .watching
                    .store(false, std::sync::atomic::Ordering::Release);
                // A join between the check and the store found the watch
                // still running and started none: take it up again.
                let joined = !this.inner.calls.state().participations.is_empty();
                if !joined
                    || this
                        .inner
                        .calls
                        .watching
                        .swap(true, std::sync::atomic::Ordering::AcqRel)
                {
                    break;
                }
            }
        });
    }

    /// Observe the calls whose sessions expired, then send each workspace its
    /// call list.
    async fn observe_expired_calls(&self) {
        for workspace in self.inner.calls.expire() {
            let Ok(workspace) = self.workspace(&workspace).await else {
                continue;
            };
            self.observe_calls(&*workspace.lock().await);
        }
        self.send_call_lists().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One participation of `device` in `call`, for `State` fixtures.
    fn participation(device: [u8; 32], call: [u8; 32]) -> Participation {
        let (outbound, _) = mpsc::channel(1);
        Participation {
            workspace: "room".into(),
            workspace_id: 1,
            call,
            epoch: 0,
            device,
            user: device,
            directions: Directions {
                publish: true,
                subscribe: true,
            },
            showing: Showing::default(),
            seen: tilo_clock::now(),
            outbound,
        }
    }

    /// A device is in one call at a time: joining another call ends its
    /// earlier participation, wherever it was.
    #[test]
    fn a_device_is_in_one_call_at_a_time() {
        let device = [9; 32];
        let mut state = State::default();
        state
            .participations
            .insert([1; 32], participation(device, [3; 32]));
        state
            .participations
            .insert([7; 32], participation([8; 32], [5; 32]));

        assert_eq!(state.end_device(device, &[4; 32]), [[3; 32]]);
        assert_eq!(
            state.participations.keys().copied().collect::<Vec<_>>(),
            [[7; 32]],
            "another device's participation stands"
        );
    }

    /// A policy with one active call `[3; 32]` in which members publish and
    /// subscribe, whose members are `people` (device = person).
    fn policy(people: &[[u8; 32]]) -> Policy {
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
            calls: Some(WorkspaceMediaV1 {
                version: 1,
                workspace: [0; 16],
                authorization_revision: 1,
                authorization_basis: [0; 32],
                calls,
            }),
            access: GovernedAccess {
                member_devices: people.to_vec(),
                member_user_roots: people.to_vec(),
            },
            carried: false,
        }
    }

    /// Only a member takes part, and a denial of a person holds in the
    /// call's directions.
    #[test]
    fn a_call_admits_its_members_and_holds_their_denials() {
        let (ada, bo) = ([1; 32], [2; 32]);
        let call = [3; 32];
        let mut policy = policy(&[ada]);
        assert_eq!(
            current(&policy, ada, ada, &call),
            Ok((
                2,
                Directions {
                    publish: true,
                    subscribe: true,
                }
            ))
        );
        assert_eq!(
            current(&policy, bo, bo, &call),
            Err("not a member of the room".into())
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
            current(&policy, ada, ada, &call),
            Ok((
                2,
                Directions {
                    publish: false,
                    subscribe: true,
                }
            ))
        );
    }
}
