//! The Discord bridge's calls: a Discord voice channel and the Tilo voice
//! channel it is mapped to, carried as one call by the workspace's host.
//!
//! The host takes part in the workspace's live session as its own device
//! ([`LiveSession::local`] on the relay it serves): it hears who is in which
//! call from everyone's presence, and in the carried call it says it is the
//! bridge to Discord ([`CallPresence::bridge`]), so every call view shows
//! the call as bridged there and the bridge as no one's face. What the Tilo
//! callers say is handed to the connector's worker speaker by speaker, which
//! mixes it into the Discord channel.
//!
//! Every Discord person in the carried channel is a participant of the Tilo
//! call of their own: the bridge says them on presence on their behalf, under
//! their name and picture there ([`Bridged`]), and the voice it publishes is
//! theirs (a [`VoiceObject`] names its speaker), so each is heard, shown and
//! turned down on their own. Nothing Discord said goes back to Discord, and a
//! person blocked in the bridge, muted in Discord or denied in the call is not
//! carried.
//!
//! Which call is carried follows who is in it: a pair is carried while its
//! Discord channel or its Tilo call has anyone besides the bridge, and let
//! go when both are empty. Discord lets a bot into one voice channel of a
//! server at a time, so one pair is carried at a time, the first occupied
//! one by Discord id, kept until it empties.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use ::discord::voice::VoicePair;
use collab_types::presence::{Bridged, CallPresence};
use tilo_agent_session::AppCatalogue;
use tilo_connector::{CallLink, CallsReport, ConnectorProcess, OutsideAudio};
use tilo_live::live::Track;
use tilo_live::live_session::{Join, LiveSession};
use tilo_live::presence::{self, Board, BridgedMember, Call, LiveCall, Own};
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
/// The longest an avatar fetch may hold a bridged person's arrival.
const AVATAR_WAIT: Duration = Duration::from_secs(10);

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

/// One Discord person carried into the Tilo call: who they are there, the
/// member they are in the call as, and whether the call lets them speak.
struct Guest {
    /// The member they are in the call as: the member their account is linked
    /// to, else their stand-in publisher, as hex.
    person: String,
    /// What the bridge says of them in the call.
    member: BridgedMember,
    /// Their microphone is off, in Discord or in the call: nothing of theirs
    /// is carried.
    muted: bool,
    /// Their next voice object: each speaker's stream is its own, as any
    /// caller's is.
    voice: GuestVoice,
}

/// One carried speaker's audio: their stream's place.
struct GuestVoice {
    /// The next timestamp of their voice, in 48 kHz samples since they were
    /// carried.
    timestamp: u64,
    started: Instant,
    sent: bool,
}

impl GuestVoice {
    fn new() -> Self {
        Self {
            timestamp: 0,
            started: Instant::now(),
            sent: false,
        }
    }

    /// The next object of `speaker`'s stream for `opus`, the `sequence`th of
    /// the bridge's track: one tilo-voice/1 object naming whose voice it is,
    /// its timestamp following the clock so a receiver hears a pause as one.
    fn take(
        &mut self,
        speaker: [u8; 32],
        opus: Vec<u8>,
        sequence: u64,
        now: Instant,
    ) -> VoiceObject {
        let elapsed = now.saturating_duration_since(self.started);
        let by_clock = u64::try_from(
            elapsed
                .as_micros()
                .saturating_mul(u128::from(SAMPLE_RATE))
                .checked_div(1_000_000)
                .unwrap_or_default(),
        )
        .unwrap_or(u64::MAX);
        let timestamp = if self.sent {
            by_clock.max(self.timestamp.saturating_add(u64::from(FRAME_SAMPLES)))
        } else {
            by_clock
        };
        self.timestamp = timestamp;
        self.sent = true;
        VoiceObject {
            sequence,
            timestamp,
            duration: FRAME_SAMPLES,
            speaker: Some(speaker),
            payload: opus,
        }
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
    /// The Discord people carried into the Tilo call, by their account there:
    /// the blocked and those the call leaves out are absent, and a muted one is
    /// shown but not heard.
    guests: BTreeMap<String, Guest>,
    /// The next object of the bridge's audio track: the track numbers its
    /// datagrams, whatever speaker each carries.
    published: u32,
    /// Avatars fetched, by URL: none when the fetch failed.
    avatars: std::collections::HashMap<String, Option<String>>,
    /// What the workspace's calls allow, and the person each member device is,
    /// as they were last read.
    gate: tilo_peer_state::CallGate,
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
            guests: BTreeMap::new(),
            published: 0,
            avatars: std::collections::HashMap::new(),
            gate: tilo_peer_state::CallGate::default(),
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
        // What the workspace's calls allow: the bridge carries to Discord no voice a call refuses.
        // It is read before the keys, so a call that moved on is held to before the session says
        // anything of it; a read that failed keeps the last word rather than letting everyone in.
        if let Some(gate) = self.call_gate().await {
            self.gate = gate;
        }
        let (rules, persons) = self.gate.clone();
        let Some(keys) = self.keys().await else {
            return;
        };
        let roster = Roster::new(devices);
        let epoch = keys.epoch();
        if self
            .taking
            .as_ref()
            .is_some_and(|taking| taking.live.ended())
        {
            self.taking = None;
        }
        if let Some(taking) = &mut self.taking {
            taking.live.set_calls(rules.clone(), persons.clone());
            // A call's media keys come only to the bridge while the call admits
            // its person: an ended call, or one that denied them, reaches it
            // through no keys of the rule it moved on to.
            let admitted = taking.live.call_keys_wanted().is_none_or(|(channel, _)| {
                let rule = rules.get(&channel).cloned().unwrap_or_default();
                rule.may(
                    persons
                        .get(&self.host.device_public_key)
                        .map(String::as_str),
                )
                .any()
            });
            if admitted {
                hand_call_keys(&self.workspaces, &self.workspace, &taking.live).await;
            }
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
                live.set_calls(rules.clone(), persons.clone());
                let admitted = live.call_keys_wanted().is_none_or(|(channel, _)| {
                    let rule = rules.get(&channel).cloned().unwrap_or_default();
                    rule.may(
                        persons
                            .get(&self.host.device_public_key)
                            .map(String::as_str),
                    )
                    .any()
                });
                if admitted {
                    hand_call_keys(&self.workspaces, &self.workspace, &live).await;
                }
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
            // The Discord people the bridge carries are participants of the call
            // of their own, never the bridge's person.
            bridged: if self.in_tilo {
                self.guests
                    .values()
                    .map(|guest| guest.member.clone())
                    .collect()
            } else {
                Vec::new()
            },
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
    /// Tilo call as theirs, unless they are muted or the call leaves them out.
    fn carry_in(&mut self, frame: OutsideAudio) {
        if !self.in_tilo {
            return;
        }
        let Some(guest) = self
            .guests
            .get_mut(&frame.speaker)
            .filter(|guest| !guest.muted)
        else {
            return;
        };
        let Some(person) = person_key(&guest.person) else {
            return;
        };
        let Some(taking) = &self.taking else {
            return;
        };
        let sequence = self.published;
        let object = guest
            .voice
            .take(person, frame.opus, u64::from(sequence), Instant::now());
        let Ok(body) = object.encode() else {
            return;
        };
        self.published = self.published.wrapping_add(1);
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
        let status = status(&live, &pair.tilo);
        self.ask(Some(pair.discord.clone()), Some(status)).await;
        // Nothing crosses until the worker is in the Discord channel.
        let joined = matches!(&report.link, CallLink::Joined { call } if *call == pair.discord);
        self.set_in_tilo(joined);
        if joined {
            self.sync_guests(report, &pair, &guild).await;
        } else {
            self.guests.clear();
        }
    }

    /// Carry each Discord person in `pair`'s channel as a participant of the
    /// Tilo call of their own, and stop carrying anyone who left, went quiet on
    /// the bridge or is denied in the call.
    async fn sync_guests(
        &mut self,
        report: &CallsReport,
        pair: &VoicePair,
        guild: &::discord::Guild,
    ) {
        let (rules, persons) = self.gate.clone();
        let rule = rules.get(&pair.tilo).cloned().unwrap_or_default();
        let bridge_may = rule.may(
            persons
                .get(&self.host.device_public_key)
                .map(String::as_str),
        );
        // A call that leaves the bridge out carries nobody through it.
        let people = if bridge_may.any() {
            discord_people(report, &pair.discord, guild)
        } else {
            Vec::new()
        };
        let wanted: BTreeSet<String> = people.iter().map(|person| person.account.clone()).collect();
        self.guests.retain(|account, _| wanted.contains(account));
        for person in people {
            let linked = guild.linked_person(&person.account).map(str::to_owned);
            let session = bridged_session(&person.account);
            let stand_in = collab_types::presence::stand_in(self.host.device_public_key, &session);
            let person_hex = linked.clone().unwrap_or_else(|| hex::encode(stand_in));
            let may = rule.may(Some(&person_hex));
            if !may.any() {
                // Denied in the call: carried in neither direction.
                self.guests.remove(&person.account);
                continue;
            }
            let name = shown_name(&person.name, &person.account);
            let bridged = Bridged {
                service: SERVICE.to_owned(),
                account: person.account.clone(),
                name: name.clone(),
                avatar: self.avatar(person.avatar_url.as_deref()).await,
                linked,
            };
            let muted = person.muted || !may.publish;
            let member = BridgedMember {
                session,
                name,
                bridged,
                call: Call {
                    call: CallPresence {
                        channel: pair.tilo.clone(),
                        name: String::new(),
                        bridge: None,
                    },
                    camera: false,
                    screen: false,
                    muted,
                    deafened: !may.subscribe,
                },
            };
            match self.guests.entry(person.account.clone()) {
                std::collections::btree_map::Entry::Occupied(mut held) => {
                    if held.get().member == member && held.get().muted == muted {
                        continue;
                    }
                    // A name, picture or rule moved: keep their stream's place
                    // so the frame after the change is heard as theirs.
                    let voice = std::mem::replace(&mut held.get_mut().voice, GuestVoice::new());
                    held.insert(Guest {
                        person: person_hex,
                        member,
                        muted,
                        voice,
                    });
                }
                std::collections::btree_map::Entry::Vacant(slot) => {
                    slot.insert(Guest {
                        person: person_hex,
                        member,
                        muted,
                        voice: GuestVoice::new(),
                    });
                }
            }
        }
        // Each bridged person says themselves on the bridge's presence: who is
        // here, by what name, and in the call.
        let own = self.own();
        if let Some(taking) = &mut self.taking {
            taking.board.say(&own);
        }
    }

    /// The avatar `url` as the workspace stores it: what is already kept, else
    /// fetched once through the worker.
    async fn avatar(&mut self, url: Option<&str>) -> Option<String> {
        let url = url?;
        if let Some(known) = self.avatars.get(url) {
            return known.clone();
        }
        let op = serde_json::json!({
            "op": {
                "op": "fetch-asset",
                "url": url,
                "max_bytes": tilo_actions::appearance::MAX_AVATAR_BYTES,
            },
        });
        let fetched = match tokio::time::timeout(AVATAR_WAIT, self.lane.perform(op)).await {
            Ok(Ok(result)) => result
                .get("blob")
                .and_then(serde_json::Value::as_str)
                .filter(|blob| content_hash(blob))
                .map(str::to_owned),
            Ok(Err(error)) => {
                tracing::debug!(%error, "a Discord avatar was not fetched");
                None
            }
            Err(_) => None,
        };
        self.avatars.insert(url.to_owned(), fetched.clone());
        fetched
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
        self.guests.clear();
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

/// Who is in Discord voice channel `channel`, the people the bridge keeps out
/// left out: the muted stay, shown as such. A call the workspace denies one of
/// them leaves that one out later.
fn discord_people<'a>(
    report: &'a CallsReport,
    channel: &str,
    guild: &::discord::Guild,
) -> Vec<&'a tilo_connector::OutsidePerson> {
    report
        .calls
        .iter()
        .filter(|call| call.call == channel)
        .flat_map(|call| &call.people)
        .filter(|person| !guild.blocks(&person.account))
        .collect()
}

/// The devices in the Tilo call of voice channel `channel`, bridges and the
/// people a bridge carries left out, by their publisher: whose voice is handed
/// to Discord.
fn tilo_callers(live: &[LiveCall], channel: &str) -> Vec<String> {
    live.iter()
        .filter(|call| call.channel == channel)
        .flat_map(|call| &call.people)
        .filter(|caller| caller.bridge.is_none() && caller.bridged.is_none())
        .map(|caller| caller.id.clone())
        .collect()
}

/// Who is on the Tilo side, as a line for Discord.
fn status(live: &[LiveCall], channel: &str) -> String {
    let mut names: Vec<String> = live
        .iter()
        .filter(|call| call.channel == channel)
        .flat_map(|call| &call.people)
        .filter(|caller| caller.bridge.is_none() && caller.bridged.is_none())
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

/// The session a Discord account's bridged participant is said under: one per
/// account, so every device derives the same publisher for them.
fn bridged_session(account: &str) -> String {
    format!("{SERVICE}:{account}")
}

/// The name a Discord person is shown by: theirs as an appearance may carry it,
/// else their account's.
fn shown_name(name: &str, account: &str) -> String {
    tilo_actions::appearance::Appearance::new(name, None, None).map_or_else(
        |_| {
            format!("Discord user {account}")
                .chars()
                .take(collab_types::presence::MAX_BRIDGED_NAME_CHARS)
                .collect()
        },
        |appearance| appearance.name,
    )
}

/// A person id as the key the live session's voice names: 32 bytes in hex.
fn person_key(person: &str) -> Option<[u8; 32]> {
    let mut key = [0; 32];
    hex::decode_to_slice(person, &mut key).ok()?;
    Some(key)
}

/// Whether `blob` names content by its BLAKE3 hex, as an avatar must.
fn content_hash(blob: &str) -> bool {
    blob.len() == 64
        && blob
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
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
