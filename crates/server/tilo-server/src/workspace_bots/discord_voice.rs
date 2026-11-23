//! The Discord bridge's calls: a Discord voice channel and the Tilo voice
//! channel it is mapped to, carried as one call by the workspace's host.
//!
//! The host takes part in the workspace's live session as its own device
//! ([`LiveSession::local`] on the relay it serves): it hears who is in which
//! call from everyone's presence, and in the carried call it says it is the
//! bridge to Discord ([`CallPresence::bridge`]), so every call view shows
//! the call as bridged there and the bridge as no one's face. What the Tilo
//! callers say is handed to the connector's worker speaker by speaker, which
//! mixes it into the Discord channel; what Discord people say is published
//! on the bridge's own audio track. One voice is carried at a time that way:
//! a Discord speaker holds the track while they talk, and the next one takes
//! it when they pause ([`FLOOR`]). Nothing Discord said goes back to
//! Discord, and a person blocked in the bridge or muted in Discord is not
//! carried.
//!
//! Which call is carried follows who is in it: a pair is carried while its
//! Discord channel or its Tilo call has anyone besides the bridge, and let
//! go when both are empty. Discord lets a bot into one voice channel of a
//! server at a time, so one pair is carried at a time, the first occupied
//! one by Discord id, kept until it empties.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use ::discord::voice::VoicePair;
use collab_types::presence::CallPresence;
use tilo_agent_session::AppCatalogue;
use tilo_connector::{CallLink, CallsReport, ConnectorProcess, OutsideAudio};
use tilo_live::live::Track;
use tilo_live::live_session::{Join, LiveSession};
use tilo_live::presence::{self, Board, Call, LiveCall, Own};
use tilo_live::relay::Relay;
use tilo_live::roster::Roster;
use tilo_live::scope::Scope;
use tilo_live::voice::{FRAME_SAMPLES, SAMPLE_RATE, VoiceObject};
use tilo_user_identity::DeviceCert;
use tokio::sync::broadcast;

use super::discord;
use crate::workspaces::Workspaces;
use tilo_workspace_service::agents::workspace_resource;

/// The service the bridge carries calls to, as call views name it.
pub const SERVICE: &str = "discord";
/// The name the bridge's device shows in the workspace.
const BRIDGE_NAME: &str = "Discord";
/// How often the live session is read and the presence said: half a voice
/// frame, so a frame waits at most that long either way.
const HEARING: Duration = Duration::from_millis(10);
/// How often the workspace's devices and keys are read again, and who is
/// where looked at once more.
const TICK: Duration = Duration::from_secs(1);
/// How long a Discord speaker keeps the bridge's voice after their last
/// frame: a pause this long hands it to whoever speaks next.
pub const FLOOR: Duration = Duration::from_millis(200);

/// The host's place in the workspace's live session: its session, and the
/// presence it reads and says there.
struct Taking {
    live: LiveSession,
    board: Board,
    /// The roster and key epoch the session was last given.
    roster: Roster,
    epoch: u64,
    /// The calls as the board last said them: only a change of who is in
    /// which call has the bridge look again.
    calls: Vec<LiveCall>,
}

/// The Discord speaker who holds the bridge's voice, and the bridge's audio
/// track as it stands.
struct Voice {
    holder: Option<(String, Instant)>,
    sequence: u32,
    timestamp: u64,
    started: Instant,
}

impl Voice {
    fn new() -> Self {
        Self {
            holder: None,
            sequence: 0,
            timestamp: 0,
            started: Instant::now(),
        }
    }

    /// The next object of the bridge's track for `opus` from `speaker`, or
    /// `None` while someone else holds it. Its timestamp follows the clock,
    /// so a receiver hears a pause as one.
    fn take(&mut self, speaker: &str, opus: Vec<u8>, now: Instant) -> Option<(u32, VoiceObject)> {
        if let Some((holder, at)) = &self.holder
            && holder != speaker
            && now.saturating_duration_since(*at) < FLOOR
        {
            return None;
        }
        self.holder = Some((speaker.to_owned(), now));
        let elapsed = now.saturating_duration_since(self.started);
        let by_clock = u64::try_from(
            elapsed
                .as_micros()
                .saturating_mul(u128::from(SAMPLE_RATE))
                .checked_div(1_000_000)
                .unwrap_or_default(),
        )
        .unwrap_or(u64::MAX);
        let timestamp = if self.sequence == 0 {
            by_clock
        } else {
            by_clock.max(self.timestamp.saturating_add(u64::from(FRAME_SAMPLES)))
        };
        self.timestamp = timestamp;
        let sequence = self.sequence;
        self.sequence = self.sequence.wrapping_add(1);
        Some((
            sequence,
            VoiceObject {
                sequence: u64::from(sequence),
                timestamp,
                duration: FRAME_SAMPLES,
                payload: opus,
            },
        ))
    }
}

/// The Discord bridge's call carrier in one workspace this host governs.
pub struct VoiceBridge {
    workspaces: Workspaces,
    workspace: String,
    /// This host's device: the bridge's place in the call.
    host: DeviceCert,
    relay: Relay,
    /// The connector's worker, which is in the Discord call.
    lane: Arc<ConnectorProcess>,
    catalogue: AppCatalogue,
    taking: Option<Taking>,
    /// The pair carried now, and whether the bridge is in its Tilo call.
    carried: Option<VoicePair>,
    in_tilo: bool,
    /// What the worker was last asked: the outside call and its status.
    asked: Option<(Option<String>, Option<String>)>,
    /// The Discord people whose voice is carried: those in the carried
    /// channel, the muted and the blocked left out.
    heard_from: BTreeSet<String>,
    voice: Voice,
}

impl VoiceBridge {
    /// The bridge of `workspace`, governed here by `host`, taking part in
    /// its calls on `relay` and reaching Discord through `lane`.
    #[must_use]
    pub fn new(
        workspaces: Workspaces,
        workspace: String,
        host: DeviceCert,
        relay: Relay,
        lane: Arc<ConnectorProcess>,
    ) -> Self {
        Self {
            workspaces,
            workspace,
            host,
            relay,
            lane,
            catalogue: AppCatalogue::native(),
            taking: None,
            carried: None,
            in_tilo: false,
            asked: None,
            heard_from: BTreeSet::new(),
            voice: Voice::new(),
        }
    }

    /// Carry the workspace's calls for as long as the worker's lane stands;
    /// leaves both sides on the way out.
    pub async fn run(mut self) {
        let mut calls = self.lane.calls();
        let mut audio = self.lane.audio();
        let mut writes = self.workspaces.document_writes();
        let mut tick = tokio::time::interval(TICK);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut hearing = tokio::time::interval(HEARING);
        hearing.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            let mut reconcile = false;
            tokio::select! {
                changed = calls.changed() => {
                    if changed.is_err() {
                        break;
                    }
                    reconcile = true;
                }
                frame = audio.recv() => match frame {
                    Ok(frame) => self.carry_in(frame),
                    Err(broadcast::error::RecvError::Lagged(_)) => {}
                    Err(broadcast::error::RecvError::Closed) => break,
                },
                write = writes.recv() => match write {
                    Ok(write) => reconcile = write.workspace == self.workspace && write.app == discord::APP,
                    Err(broadcast::error::RecvError::Lagged(_)) => reconcile = true,
                    Err(broadcast::error::RecvError::Closed) => break,
                },
                _ = hearing.tick() => {
                    if self.hear().await {
                        reconcile = true;
                    }
                }
                _ = tick.tick() => {
                    self.keep_taking().await;
                    reconcile = true;
                }
            }
            if reconcile {
                let report = calls.borrow_and_update().clone();
                self.reconcile(&report).await;
            }
        }
        self.let_go().await;
    }

    /// Join the workspace's live session once its keys are here, and keep
    /// its roster and keys current.
    async fn keep_taking(&mut self) {
        let Some(devices) = self.devices().await else {
            return;
        };
        let Some(keys) = self.keys().await else {
            return;
        };
        let roster = Roster::new(devices);
        let epoch = keys.epoch();
        // What the workspace's calls allow: the bridge carries to Discord no voice a call refuses.
        let (rules, persons) = self.call_gate().await.unwrap_or_default();
        if self
            .taking
            .as_ref()
            .is_some_and(|taking| taking.live.ended())
        {
            self.taking = None;
        }
        if let Some(taking) = &mut self.taking {
            taking.live.set_calls(rules, persons);
            hand_call_keys(&self.workspaces, &self.workspace, &taking.live).await;
            if taking.roster != roster {
                taking.live.set_roster(roster.clone());
                taking.roster = roster;
            }
            if taking.epoch != epoch {
                taking.live.set_keys(keys);
                taking.epoch = epoch;
            }
            return;
        }
        let join = Join {
            scope: Scope::workspace(self.workspace.clone()),
            device: self.host.device_public_key,
            keys: keys.into(),
            roster: roster.clone(),
            publish: true,
            presence: true,
        };
        match LiveSession::local(&self.relay, &join) {
            Ok(live) => {
                live.set_calls(rules, persons);
                hand_call_keys(&self.workspaces, &self.workspace, &live).await;
                let mut board = Board::new(self.workspace.clone(), self.host.device_public_key);
                board.say(&self.own());
                self.taking = Some(Taking {
                    live,
                    board,
                    roster,
                    epoch,
                    calls: Vec::new(),
                });
            }
            Err(error) => {
                tracing::warn!(workspace = %self.workspace, %error, "the Discord bridge cannot take part in the workspace's calls");
            }
        }
    }

    /// The devices the workspace admits now, this host's among them.
    async fn devices(&self) -> Option<Vec<[u8; 32]>> {
        let id = crate::WorkspaceId::parse(&self.workspace)?;
        let (_, peer) = self.workspaces.governed_copy(id).await?;
        let access = peer.governance().governed_access(tilo_clock::wall()).ok()?;
        Some(access.member_devices)
    }

    /// What the workspace's calls allow, and the person each member device is.
    async fn call_gate(&self) -> Option<tilo_peer_state::CallGate> {
        let id = crate::WorkspaceId::parse(&self.workspace)?;
        let (_, peer) = self.workspaces.governed_copy(id).await?;
        peer.governance().call_gate(tilo_clock::wall()).ok()
    }

    /// The workspace's keys, which seal its live session.
    async fn keys(&self) -> Option<key_group::ContentKeys> {
        let keys = self.workspaces.device_keys()?;
        match keys
            .group_keys(&self.workspace, collab_types::EVERY_MEMBER, true)
            .await?
        {
            Ok(keys) => Some(keys),
            Err(error) => {
                tracing::debug!(workspace = %self.workspace, %error, "the workspace's keys are not here yet");
                None
            }
        }
    }

    /// What the bridge says of itself: its name, and the call it bridges.
    fn own(&self) -> Own {
        Own {
            label: BRIDGE_NAME.to_owned(),
            device: BRIDGE_NAME.to_owned(),
            user: Some(hex::encode(self.host.user_public_key)),
            call: self
                .carried
                .as_ref()
                .filter(|_| self.in_tilo)
                .map(|pair| Call {
                    call: CallPresence {
                        channel: pair.tilo.clone(),
                        name: String::new(),
                        bridge: Some(SERVICE.to_owned()),
                    },
                    camera: false,
                    screen: false,
                    muted: false,
                    deafened: false,
                }),
            ..Own::default()
        }
    }

    /// Read what the live session heard: presence into the board, the
    /// carried call's voices to Discord. Say what the bridge owes. Whether
    /// who is in which call changed.
    async fn hear(&mut self) -> bool {
        let Some(taking) = &mut self.taking else {
            return false;
        };
        let now_ms = tilo_session_client::presence::now_ms();
        let own_device = self.host.device_public_key;
        let mut voices = Vec::new();
        while let Some(heard) = taking.live.try_recv() {
            if heard.sender == own_device {
                continue;
            }
            match heard.track {
                Track::Presence => {
                    taking.board.heard(heard.sender, &heard.body, now_ms);
                }
                Track::Audio => voices.push(heard),
                _ => {}
            }
        }
        if let Some(frames) = taking.board.due(now_ms)
            && let Err(error) = presence::say(&taking.live, &frames)
        {
            tracing::debug!(workspace = %self.workspace, %error, "the Discord bridge's presence was not said");
        }
        // A pointer or a status moving says nothing of the calls: only who
        // is in which call has the bridge look again.
        let calls = taking.board.calls();
        let moved = calls != taking.calls;
        // Only the carried call's people are handed to Discord, and only
        // while the bridge is in it.
        let speakers: BTreeSet<String> = match (&self.carried, self.in_tilo) {
            (Some(pair), true) => tilo_callers(&calls, &pair.tilo).into_iter().collect(),
            _ => BTreeSet::new(),
        };
        taking.calls = calls;
        for heard in voices {
            let speaker = hex::encode(heard.sender);
            if !speakers.contains(&speaker) {
                continue;
            }
            let Ok(object) = VoiceObject::decode(&heard.body) else {
                continue;
            };
            if self.lane.send_audio(speaker, object.payload).await.is_err() {
                // The worker is not running: what it missed is gone.
                break;
            }
        }
        moved
    }

    /// One frame Discord person `speaker` said: the bridge says it in the
    /// Tilo call while they hold its voice.
    fn carry_in(&mut self, frame: OutsideAudio) {
        if !self.in_tilo || !self.heard_from.contains(&frame.speaker) {
            return;
        }
        let Some(taking) = &self.taking else {
            return;
        };
        let Some((sequence, object)) = self.voice.take(&frame.speaker, frame.opus, Instant::now())
        else {
            return;
        };
        let Ok(body) = object.encode() else {
            return;
        };
        if let Err(error) = taking.live.publish(Track::Audio, sequence, &body) {
            tracing::debug!(workspace = %self.workspace, %error, "a Discord frame was not carried");
        }
    }

    /// The guild record as the workspace holds it now.
    async fn guild(&self) -> Option<::discord::Guild> {
        let record = workspace_resource(&self.catalogue, discord::APP);
        let saved = self
            .workspaces
            .resource_document(&self.workspace, discord::APP, &record)
            .await
            .ok()
            .flatten()?;
        let mut doc = automerge::AutoCommit::load(&saved).ok()?;
        tilo_workspace_service::agents::opened::<::discord::Guild>(&mut doc).ok()
    }

    /// Carry the pair who is in calls asks for, and let go of the rest.
    async fn reconcile(&mut self, report: &CallsReport) {
        let Some(guild) = self.guild().await else {
            return self.let_go().await;
        };
        let live = self
            .taking
            .as_ref()
            .map(|taking| taking.calls.clone())
            .unwrap_or_default();
        let pairs = guild.voice_pairs();
        let current = self.carried.clone();
        let wanted = choose(&pairs, current.as_ref(), |pair| {
            !discord_people(report, &pair.discord, &guild).is_empty()
                || !tilo_callers(&live, &pair.tilo).is_empty()
        });
        let Some(pair) = wanted else {
            return self.let_go().await;
        };
        if current.as_ref() != Some(&pair) {
            self.let_go().await;
            self.carried = Some(pair.clone());
        }
        self.heard_from = discord_people(report, &pair.discord, &guild)
            .into_iter()
            .collect();
        let status = status(&live, &pair.tilo);
        self.ask(Some(pair.discord.clone()), Some(status)).await;
        // Nothing crosses until the worker is in the Discord channel.
        let joined = matches!(&report.link, CallLink::Joined { call } if *call == pair.discord);
        self.set_in_tilo(joined);
    }

    /// Be in the carried pair's Tilo call, or not: what the bridge's
    /// presence says, and so what every call view shows.
    fn set_in_tilo(&mut self, in_tilo: bool) {
        if self.in_tilo == in_tilo {
            return;
        }
        self.in_tilo = in_tilo;
        let own = self.own();
        if let Some(taking) = &mut self.taking {
            taking.board.say(&own);
        }
    }

    /// Ask the worker into `call` showing `status`, unless that is what it
    /// was last asked.
    async fn ask(&mut self, call: Option<String>, status: Option<String>) {
        let wanted = (call, status);
        if self.asked.as_ref() == Some(&wanted) {
            return;
        }
        let (call, status) = wanted.clone();
        if let Err(error) = self.lane.set_call(call, status).await {
            // A worker that starts later is told; asked all the same.
            tracing::debug!(%error, "the Discord worker is not running to take the call");
        }
        self.asked = Some(wanted);
    }

    /// Carry no call: leave both sides.
    async fn let_go(&mut self) {
        self.set_in_tilo(false);
        self.carried = None;
        self.heard_from.clear();
        self.ask(None, None).await;
    }
}

/// The pair to carry: `current` while `occupied`, else the first occupied
/// one, else none.
fn choose(
    pairs: &[VoicePair],
    current: Option<&VoicePair>,
    occupied: impl Fn(&VoicePair) -> bool,
) -> Option<VoicePair> {
    if let Some(current) = current
        && pairs.contains(current)
        && occupied(current)
    {
        return Some(current.clone());
    }
    pairs.iter().find(|pair| occupied(pair)).cloned()
}

/// Who is in Discord voice channel `channel`, by account: the people the
/// bridge keeps out, and the muted, left out.
fn discord_people(report: &CallsReport, channel: &str, guild: &::discord::Guild) -> Vec<String> {
    report
        .calls
        .iter()
        .filter(|call| call.call == channel)
        .flat_map(|call| &call.people)
        .filter(|person| !person.muted && !guild.blocks(&person.account))
        .map(|person| person.account.clone())
        .collect()
}

/// The devices in the Tilo call of voice channel `channel`, bridges left
/// out, by their publisher.
fn tilo_callers(live: &[LiveCall], channel: &str) -> Vec<String> {
    live.iter()
        .filter(|call| call.channel == channel)
        .flat_map(|call| &call.people)
        .filter(|caller| caller.bridge.is_none())
        .map(|caller| caller.id.clone())
        .collect()
}

/// Who is on the Tilo side, as a line for Discord.
fn status(live: &[LiveCall], channel: &str) -> String {
    let mut names: Vec<String> = live
        .iter()
        .filter(|call| call.channel == channel)
        .flat_map(|call| &call.people)
        .filter(|caller| caller.bridge.is_none())
        .map(|caller| caller.label.trim().to_owned())
        .filter(|name| !name.is_empty())
        .collect();
    names.dedup();
    if names.is_empty() {
        "Nobody on Tilo yet".to_owned()
    } else {
        format!("On Tilo: {}", names.join(", "))
    }
}

/// Hand `live` the keys of the call media it needs now when this host holds them: the key group
/// of its call's rule's generation in `workspace`, which only the devices the call admits are
/// given. Asking for them takes this host into the group; the bridge asks again on its next tick.
async fn hand_call_keys(workspaces: &Workspaces, workspace: &str, live: &LiveSession) {
    let Some((channel, generation)) = live.call_keys_wanted() else {
        return;
    };
    let Some(device) = workspaces.device_keys() else {
        return;
    };
    let route = tilo_peer_state::document_frame::DocumentRoute {
        app_id: collab_types::scope::MEDIA_APP.to_owned(),
        cluster_id: workspace.to_owned(),
        document: collab_types::calls::media_document(&channel, generation),
    };
    let Ok(scope) = route.key_scope_for(None) else {
        return;
    };
    match device
        .group_keys(&scope.to_string(), collab_types::EVERY_MEMBER, true)
        .await
    {
        Some(Ok(keys)) => live.set_call_keys(&channel, generation, keys),
        Some(Err(error)) => {
            tracing::debug!(%workspace, %error, "the call's keys are not here yet");
        }
        None => {}
    }
}
