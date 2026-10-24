//! The Discord bridge's calls: a Discord voice channel and the Tilo voice
//! channel it is mapped to carried as one call, run on the room's host
//! beside the bridge bot.
//!
//! The Tilo side goes through the relay's own voice carrier like any
//! participant's. The bot's device holds one participation of its own,
//! which only hears the call and hands each Tilo speaker's frames to the
//! connector's worker, and one per person in the Discord channel, which
//! only speaks: each Discord speaker is their own participant in Tilo, with
//! their own name, picture, speaking light and volume. Nothing a Discord
//! person said goes back to Discord, and a person blocked in the bridge,
//! muted in Discord, denied in the call or linked to someone removed from
//! the room is not carried.
//!
//! Which call is carried follows who is in it: a pair is carried while its
//! Discord channel or its Tilo call has anyone besides the bridge, and let
//! go when both are empty. Discord lets a bot into one voice channel of a
//! server at a time, so one pair is carried at a time, the first occupied
//! one by Discord id, kept until it empties.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use ::discord::voice::VoicePair;
use tilo_agent_participant::AppCatalogue;
use tilo_connector::{CallLink, CallsReport, ConnectorProcess, OutsideAudio, OutsideCall};
use tilo_lock::MutexRecover as _;
use tilo_moq::client::{
    CapturedOpus, Client, ClientAuthority, Connection, ConnectionScope, Status,
};
use tilo_moq::relay::Direction;
use tilo_relay_protocol::Envelope;
use tilo_relay_protocol::voice::{
    Bridged, CallOperation, CallSummary, HEARTBEAT, Showing, VoiceGrant, VoiceParticipant,
};
use tilo_user_identity::DeviceCert;
use tokio::sync::{broadcast, mpsc, watch};

use super::{discord, room_record};
use crate::rooms::Rooms;

/// The service a bridged participant is reached through.
const SERVICE: &str = "discord";
/// The name the bridge's own participation shows.
const BRIDGE_NAME: &str = "Discord";
const CHAT: &str = "so.tilo.chat";
/// How often carriers are looked at: failed ones opened again, the
/// participations renewed when due.
const TICK: Duration = Duration::from_secs(1);
/// How often the call's audio is handed to Discord: half a frame, so a
/// frame waits at most that long. A client wakes its owner for pictures
/// only, so voice is collected on this beat.
const HEARING: Duration = Duration::from_millis(10);
/// Grants, refusals and call lists the relay may queue for the bridge
/// before it drops one; a dropped grant is answered again at the next
/// heartbeat.
const ENVELOPES: usize = 256;
/// The longest a carrier that failed waits before it is opened again.
const MAX_RETRY: Duration = Duration::from_secs(10);
/// The longest an avatar fetch may hold a person's join.
const AVATAR_WAIT: Duration = Duration::from_secs(10);
/// The largest avatar fetched, as an appearance's.
const MAX_AVATAR_BYTES: u64 = tilo_actions::appearance::MAX_AVATAR_BYTES as u64;

/// What the bridge needs of the connector's worker: the call lane of
/// `tilo_connector`, and fetching an avatar into the room's content store.
/// [`ConnectorProcess`] is the real one; a test stands in for Discord.
pub trait CallLane: Send + Sync + 'static {
    /// Have the worker be in Discord voice channel `call`, showing `status`
    /// there; `None` leaves.
    fn set_call(
        &self,
        call: Option<String>,
        status: Option<String>,
    ) -> impl Future<Output = std::io::Result<()>> + Send;
    /// The server's calls and the worker's place in them.
    fn calls(&self) -> watch::Receiver<CallsReport>;
    /// What the Discord call says, frame by frame.
    fn audio(&self) -> broadcast::Receiver<OutsideAudio>;
    /// One frame the room's call heard from `speaker`, for Discord.
    fn send_audio(
        &self,
        speaker: String,
        opus: Vec<u8>,
    ) -> impl Future<Output = std::io::Result<()>> + Send;
    /// Copy the avatar Discord serves at `url` into the room's content
    /// store: its BLAKE3 hex.
    fn fetch_avatar(&self, url: String) -> impl Future<Output = Result<String, String>> + Send;
}

impl CallLane for ConnectorProcess {
    fn set_call(
        &self,
        call: Option<String>,
        status: Option<String>,
    ) -> impl Future<Output = std::io::Result<()>> + Send {
        Self::set_call(self, call, status)
    }

    fn calls(&self) -> watch::Receiver<CallsReport> {
        Self::calls(self)
    }

    fn audio(&self) -> broadcast::Receiver<OutsideAudio> {
        Self::audio(self)
    }

    fn send_audio(
        &self,
        speaker: String,
        opus: Vec<u8>,
    ) -> impl Future<Output = std::io::Result<()>> + Send {
        Self::send_audio(self, speaker, opus)
    }

    async fn fetch_avatar(&self, url: String) -> Result<String, String> {
        let queued = serde_json::json!({
            "op": { "op": "fetch-asset", "url": url, "max_bytes": MAX_AVATAR_BYTES },
        });
        let done = self
            .perform(queued)
            .await
            .map_err(|error| format!("{error:?}"))?;
        done.get("blob")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| "the worker named no stored avatar".to_owned())
    }
}

/// The admission of one participation's carrier: what its latest grant
/// allows, re-read whenever the grant changes. The bridge's own carrier
/// never reads what the bridge itself publishes.
struct Granted {
    grant: std::sync::Mutex<VoiceGrant>,
    /// The bot's device, whose publications its own carrier leaves alone.
    bridge: [u8; 32],
    revision: AtomicU64,
}

impl Granted {
    fn new(grant: VoiceGrant, bridge: [u8; 32]) -> Self {
        Self {
            grant: std::sync::Mutex::new(grant),
            bridge,
            revision: AtomicU64::new(0),
        }
    }

    fn update(&self, grant: VoiceGrant) {
        *self.grant.lock_recover() = grant;
        self.revision.fetch_add(1, Ordering::Relaxed);
    }
}

impl ClientAuthority for Granted {
    fn current(&self, scope: &ConnectionScope, direction: Direction, namespace: &str) -> bool {
        let grant = self.grant.lock_recover();
        if grant.participation != scope.owner_epoch {
            return false;
        }
        match direction {
            Direction::Publish => {
                grant.directions.publish && grant.publication.as_deref() == Some(namespace)
            }
            Direction::Subscribe => {
                grant.directions.subscribe
                    && (scope.subscription_prefix.as_deref() == Some(namespace)
                        || grant.participants.iter().any(|participant| {
                            participant.namespace == namespace
                                && participant.device_public_key != self.bridge
                                && grant.subscriptions.iter().any(|heard| heard == namespace)
                        }))
            }
        }
    }

    fn revision(&self) -> u64 {
        self.revision.load(Ordering::Relaxed)
    }
}

/// One participation the bridge holds, and its carrier.
struct Seat {
    bridged: Bridged,
    grant: VoiceGrant,
    authority: Arc<Granted>,
    client: Option<Client>,
    /// What the open carrier was opened for: its participation and
    /// directions. Another grant keeps it unless these change.
    opened: Option<([u8; 32], bool, bool)>,
    generation: u64,
    publishing: bool,
    retry_at: Instant,
    failures: u32,
    /// The guest's next frame's timestamp, in 48 kHz samples since it
    /// joined.
    started: Instant,
    next_timestamp: u64,
    /// Whether the person is muted on the other side, as everyone in the
    /// call is shown.
    muted: bool,
}

impl Seat {
    fn new(bridged: Bridged, grant: VoiceGrant, bridge: [u8; 32]) -> Self {
        Self {
            bridged,
            authority: Arc::new(Granted::new(grant.clone(), bridge)),
            grant,
            client: None,
            opened: None,
            generation: 0,
            publishing: false,
            retry_at: Instant::now(),
            failures: 0,
            started: Instant::now(),
            next_timestamp: 0,
            muted: false,
        }
    }

    fn granted(&mut self, grant: VoiceGrant) {
        self.authority.update(grant.clone());
        self.grant = grant;
    }

    /// Open the carrier its grant allows when it has none, or one for an
    /// earlier participation or directions, or one that failed: the
    /// bridge's own hears the call, a guest's publishes its person.
    fn keep_carrier(&mut self) {
        let own = self.bridged.account.is_none();
        let directions = self.grant.directions;
        let wanted = if own {
            directions.subscribe
        } else {
            directions.publish && self.grant.publication.is_some()
        };
        let key = (
            self.grant.participation,
            directions.publish,
            directions.subscribe,
        );
        let live = self.client.as_ref().is_some_and(|client| {
            !matches!(
                client.status(),
                Status::Denied | Status::Failed | Status::Closed
            )
        });
        if !wanted {
            self.close();
            return;
        }
        if live && self.opened == Some(key) {
            return;
        }
        if self.client.is_some() {
            // It failed or was opened for another grant.
            self.close();
            self.failures = self.failures.saturating_add(1);
            self.retry_at = later(
                Duration::from_millis(250)
                    .saturating_mul(2u32.saturating_pow(self.failures.min(6)))
                    .min(MAX_RETRY),
            );
        }
        if Instant::now() < self.retry_at {
            return;
        }
        self.generation = self.generation.saturating_add(1);
        let scope = ConnectionScope {
            endpoint: self.grant.endpoint.clone(),
            fingerprints: self.grant.fingerprints.clone(),
            owner_epoch: self.grant.participation,
            carrier_generation: self.generation,
            publication: if own {
                None
            } else {
                self.grant.publication.clone()
            },
            subscription_prefix: own.then(|| hex::encode(self.grant.call)),
        };
        let authority: Arc<dyn ClientAuthority> = self.authority.clone();
        match Client::start(
            Connection {
                scope,
                authorization: self.grant.token.clone(),
            },
            authority,
        ) {
            Ok(client) => {
                self.client = Some(client);
                self.opened = Some(key);
                self.publishing = false;
            }
            Err(error) => {
                tracing::warn!(%error, "the Discord bridge could not open a voice carrier");
                self.failures = self.failures.saturating_add(1);
                self.retry_at = later(MAX_RETRY);
            }
        }
    }

    /// Say `opus`, which its person said in Discord just now.
    fn say(&mut self, opus: Vec<u8>) {
        let Some(client) = &self.client else { return };
        if client.status() != Status::Live {
            return;
        }
        if !self.publishing {
            if client.enable_publication().is_err() {
                return;
            }
            self.publishing = true;
            self.failures = 0;
        }
        let now = Instant::now();
        let arrived = u64::try_from(
            now.duration_since(self.started)
                .as_micros()
                .saturating_mul(48)
                .checked_div(1000)
                .unwrap_or_default(),
        )
        .unwrap_or(u64::MAX);
        let timestamp = arrived.max(self.next_timestamp);
        self.next_timestamp = timestamp.saturating_add(u64::from(tilo_moq::voice::FRAME_SAMPLES));
        // A frame the carrier cannot take now is late already: live audio
        // is dropped, never queued.
        let _ = client.try_send(CapturedOpus {
            timestamp_48k: timestamp,
            duration_samples: tilo_moq::voice::FRAME_SAMPLES,
            payload: opus,
            captured_at: now,
        });
    }

    fn close(&mut self) {
        if let Some(client) = self.client.take() {
            client.close();
        }
        self.opened = None;
        self.publishing = false;
    }
}

/// The pair carried now, and what the bridge holds of its call.
struct Carried {
    pair: VoicePair,
    /// The Tilo call, once opened.
    call: Option<[u8; 32]>,
    own: Option<Seat>,
    /// The Discord people carried, by account.
    guests: BTreeMap<String, Seat>,
    /// Why a person is not carried, by account, as last said in the log.
    refused: BTreeMap<String, String>,
    /// Every namespace a guest of this pair published under: what the
    /// bridge's own carrier must never hand back to Discord.
    echoes: BTreeSet<String>,
}

impl Carried {
    const fn new(pair: VoicePair) -> Self {
        Self {
            pair,
            call: None,
            own: None,
            guests: BTreeMap::new(),
            refused: BTreeMap::new(),
            echoes: BTreeSet::new(),
        }
    }

    fn seats_mut(&mut self) -> impl Iterator<Item = &mut Seat> {
        self.own.iter_mut().chain(self.guests.values_mut())
    }
}

/// The Discord bridge's call carrier in one room.
pub struct VoiceBridge<C> {
    rooms: Rooms,
    room: String,
    /// The room's owner device, which reports to the guild record what
    /// Discord refused.
    host: DeviceCert,
    /// The bridge bot's device, which holds the participations.
    bot: DeviceCert,
    lane: Arc<C>,
    catalogue: AppCatalogue,
    outbound: mpsc::Sender<Envelope>,
    inbound: mpsc::Receiver<Envelope>,
    /// The room's calls as the relay last listed them.
    list: Vec<CallSummary>,
    carried: Option<Carried>,
    /// What the worker was last asked: the outside call and its status.
    asked: Option<(Option<String>, Option<String>)>,
    /// Avatars fetched, by URL: none when the fetch failed.
    avatars: HashMap<String, Option<String>>,
    last_heartbeat: Instant,
    /// The Discord people muted there, by account, as the worker last
    /// said: nothing of theirs is carried.
    muted: BTreeSet<String>,
}

impl<C: CallLane> VoiceBridge<C> {
    /// The bridge of `room` for the bot whose device `bot` certifies,
    /// reaching Discord through `lane`; `host` is the room's owner device
    /// that governs it.
    #[must_use]
    pub fn new(
        rooms: Rooms,
        room: String,
        host: DeviceCert,
        bot: DeviceCert,
        lane: Arc<C>,
    ) -> Self {
        let (outbound, inbound) = mpsc::channel(ENVELOPES);
        Self {
            rooms,
            room,
            host,
            bot,
            lane,
            catalogue: AppCatalogue::native(),
            outbound,
            inbound,
            list: Vec::new(),
            carried: None,
            asked: None,
            avatars: HashMap::new(),
            last_heartbeat: Instant::now(),
            muted: BTreeSet::new(),
        }
    }

    /// Carry the room's calls for as long as the room is governed here and
    /// the worker's lane stands; leaves every call on the way out.
    pub async fn run(mut self) {
        let mut writes = self.rooms.record_writes();
        let mut governance = self.rooms.governance_changes();
        let mut calls = self.lane.calls();
        let mut audio = self.lane.audio();
        let mut tick = tokio::time::interval(TICK);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut hearing = tokio::time::interval(HEARING);
        hearing.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut watching = false;
        loop {
            if !watching {
                watching = self.watch_calls().await;
            }
            let mut reconcile = false;
            tokio::select! {
                write = writes.recv() => match write {
                    Ok(write) => reconcile = write.room == self.room && write.app == discord::APP,
                    Err(broadcast::error::RecvError::Lagged(_)) => reconcile = true,
                    Err(broadcast::error::RecvError::Closed) => break,
                },
                changed = governance.recv() => match changed {
                    Ok(room) => reconcile = room == self.room,
                    Err(broadcast::error::RecvError::Lagged(_)) => reconcile = true,
                    Err(broadcast::error::RecvError::Closed) => break,
                },
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
                envelope = self.inbound.recv() => {
                    // The bridge holds a sender itself: never closed.
                    if let Some(envelope) = envelope {
                        reconcile = self.envelope(envelope);
                    }
                }
                _ = hearing.tick(), if self.hearing() => self.carry_out().await,
                _ = tick.tick() => {
                    if self.last_heartbeat.elapsed() >= HEARTBEAT {
                        self.heartbeat().await;
                        reconcile = true;
                    }
                    self.keep_carriers();
                }
            }
            if reconcile {
                let report = calls.borrow_and_update().clone();
                self.reconcile(&report).await;
            }
        }
        self.let_go().await;
    }

    /// Follow the room's call list as the bot's device. Answers whether it
    /// does now.
    async fn watch_calls(&mut self) -> bool {
        match self
            .rooms
            .voice_calls_as(&self.room, &self.bot, self.outbound.clone())
            .await
        {
            Ok(list) => {
                self.list = list;
                true
            }
            Err(error) => {
                tracing::debug!(room = %self.room, %error, "the Discord bridge cannot follow the room's calls yet");
                false
            }
        }
    }

    /// Take in what the relay sent; answers whether who is where changed.
    fn envelope(&mut self, envelope: Envelope) -> bool {
        match envelope {
            Envelope::VoiceCallList {
                result: Ok(list), ..
            } => {
                self.list = list;
                true
            }
            Envelope::VoiceGranted { grant, .. } => {
                let seat = self.carried.as_mut().and_then(|carried| {
                    carried
                        .seats_mut()
                        .find(|seat| seat.grant.participation == grant.participation)
                });
                if let Some(seat) = seat {
                    seat.granted(*grant);
                    self.keep_carriers();
                }
                false
            }
            // Which participation was refused, the next heartbeat finds.
            Envelope::VoiceRefused { .. } => {
                self.last_heartbeat = Instant::now()
                    .checked_sub(HEARTBEAT)
                    .unwrap_or_else(Instant::now);
                false
            }
            _ => false,
        }
    }

    /// The guild record as the room holds it now.
    async fn guild(&self) -> Option<::discord::Guild> {
        let record = room_record(&self.catalogue, discord::APP);
        let saved = self
            .rooms
            .record_document(&self.room, discord::APP, &record)
            .await
            .ok()
            .flatten()?;
        let mut doc = automerge::AutoCommit::load(&saved).ok()?;
        super::opened::<::discord::Guild>(&mut doc).ok()
    }

    /// Carry the pair who is in calls asks for, and let go of the rest.
    async fn reconcile(&mut self, report: &CallsReport) {
        self.muted = report
            .calls
            .iter()
            .flat_map(|call| &call.people)
            .filter(|person| person.muted)
            .map(|person| person.account.clone())
            .collect();
        let Some(guild) = self.guild().await else {
            return self.let_go().await;
        };
        let pairs = guild.voice_pairs();
        let current = self.carried.as_ref().map(|carried| carried.pair.clone());
        let wanted = choose(&pairs, current.as_ref(), |pair| {
            !discord_people(report, &pair.discord, &guild).is_empty()
                || !tilo_people(&self.list, &pair.tilo).is_empty()
        });
        let Some(pair) = wanted else {
            return self.let_go().await;
        };
        if current.as_ref() != Some(&pair) {
            self.let_go().await;
            self.carried = Some(Carried::new(pair.clone()));
        }
        let status = self.status(&pair).await;
        self.ask(Some(pair.discord.clone()), Some(status)).await;
        self.report_problem(report, &pair, &guild).await;
        let joined = matches!(&report.link, CallLink::Joined { call } if *call == pair.discord);
        if !joined {
            // Nothing crosses until the worker is in the Discord channel.
            self.leave_tilo();
            return;
        }
        if !self.open_call(&pair).await {
            return;
        }
        self.keep_own().await;
        let people = discord_people(report, &pair.discord, &guild);
        self.keep_guests(&people, &guild).await;
        self.keep_carriers();
    }

    /// Who is on the Tilo side, as a line for Discord.
    async fn status(&self, pair: &VoicePair) -> String {
        let people = tilo_people(&self.list, &pair.tilo);
        if people.is_empty() {
            return "Nobody on Tilo yet".to_owned();
        }
        // The names people show in the room's Chat, as its messages carry
        // them to Discord; a member Chat does not know yet by the name their
        // device joined with.
        let chat = self.chat().await;
        let members = self
            .rooms
            .summary(&self.room)
            .await
            .map(|summary| summary.members)
            .unwrap_or_default();
        let names: Vec<String> = people
            .iter()
            .map(|person| {
                let id = hex::encode(person);
                chat.as_ref()
                    .and_then(|chat| chat.people.iter().find(|shown| shown.id.0 == id))
                    .map(|shown| shown.name.trim().to_owned())
                    .filter(|name| !name.is_empty())
                    .or_else(|| {
                        members
                            .iter()
                            .find(|member| member.user_public_key == *person)
                            .and_then(|member| member.display_name.clone())
                    })
                    .unwrap_or_else(|| "someone".to_owned())
            })
            .collect();
        format!("On Tilo: {}", names.join(", "))
    }

    /// The room's Chat as the room holds it now.
    async fn chat(&self) -> Option<<::chat::Chat as tilo_resource::Resource>::State> {
        let record = room_record(&self.catalogue, CHAT);
        let saved = self
            .rooms
            .record_document(&self.room, CHAT, &record)
            .await
            .ok()
            .flatten()?;
        let mut doc = automerge::AutoCommit::load(&saved).ok()?;
        super::opened::<::chat::Chat>(&mut doc).ok()
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

    /// Write what Discord refused about `pair`'s channel to the guild
    /// record, where the app shows the bridge's state, and clear it once
    /// the bridge is in.
    async fn report_problem(
        &self,
        report: &CallsReport,
        pair: &VoicePair,
        guild: &::discord::Guild,
    ) {
        let problem = match &report.link {
            CallLink::Refused { call, reason } if *call == pair.discord => Some(
                reason
                    .chars()
                    .take(::discord::record::MAX_PROBLEM_CHARS)
                    .collect::<String>(),
            )
            .filter(|reason| !reason.is_empty()),
            CallLink::Joined { call } if *call == pair.discord => None,
            _ => return,
        };
        if guild.voice_problem == problem {
            return;
        }
        if let Some(reason) = &problem {
            tracing::warn!(room = %self.room, channel = %pair.discord, %reason, "Discord refused the bridge its voice channel");
        }
        let owner = hex::encode(self.host.user_public_key);
        let invoke =
            ::discord::Guild::REPORT_VOICE.with(::discord::record::guild::ReportVoice { problem });
        let record = room_record(&self.catalogue, discord::APP);
        if let Err(error) = self
            .rooms
            .author_application(
                &self.room,
                &self.host,
                &self.catalogue,
                discord::APP,
                &record,
                move |doc| {
                    discord::apply_as_owner::<::discord::Guild>(
                        doc,
                        &owner,
                        (discord::APP, "Discord"),
                        invoke,
                    )
                },
            )
            .await
        {
            tracing::warn!(room = %self.room, %error, "the Discord bridge's voice problem was not recorded");
        }
    }

    /// Open `pair`'s Tilo call as the bot, which joining the channel
    /// does. Answers whether the call stands.
    async fn open_call(&mut self, pair: &VoicePair) -> bool {
        if self
            .carried
            .as_ref()
            .is_some_and(|carried| carried.call.is_some())
        {
            return true;
        }
        let open = CallOperation::Open {
            app_id: CHAT.to_owned(),
            record_id: room_record(&self.catalogue, CHAT),
            channel_id: pair.tilo.clone(),
        };
        match self.rooms.voice_call_as(&self.room, &self.bot, &open).await {
            Ok(state) => {
                if let Some(carried) = &mut self.carried {
                    carried.call = Some(state.call);
                }
                true
            }
            Err(error) => {
                tracing::warn!(room = %self.room, channel = %pair.tilo, %error, "the Discord bridge could not open the Tilo call");
                false
            }
        }
    }

    /// Hold the bridge's own participation, which hears the call.
    async fn keep_own(&mut self) {
        let Some(carried) = &self.carried else { return };
        let Some(call) = carried.call else { return };
        if carried.own.is_some() {
            return;
        }
        let bridged = Bridged {
            service: SERVICE.to_owned(),
            account: None,
            name: BRIDGE_NAME.to_owned(),
            avatar: None,
            linked: None,
        };
        match self.join(call, None, bridged.clone()).await {
            Ok(grant) => {
                let mut seat = Seat::new(bridged, grant, self.bot.device_public_key);
                seat.keep_carrier();
                if let Some(carried) = &mut self.carried {
                    carried.own = Some(seat);
                }
            }
            Err(error) => {
                tracing::warn!(room = %self.room, %error, "the Discord bridge could not join the Tilo call");
                // The call may have ended under it: opened again next time.
                if let Some(carried) = &mut self.carried {
                    carried.call = None;
                }
            }
        }
    }

    /// Hold one participation per Discord person in `people`, as they are
    /// shown now, and none for anyone else.
    async fn keep_guests(
        &mut self,
        people: &[&tilo_connector::OutsidePerson],
        guild: &::discord::Guild,
    ) {
        let Some(call) = self.carried.as_ref().and_then(|carried| carried.call) else {
            return;
        };
        let wanted: BTreeSet<&str> = people
            .iter()
            .map(|person| person.account.as_str())
            .collect();
        let gone: Vec<String> = self
            .carried
            .as_ref()
            .map(|carried| {
                carried
                    .guests
                    .keys()
                    .filter(|account| !wanted.contains(account.as_str()))
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        for account in gone {
            self.leave_guest(&account);
        }
        if let Some(carried) = &mut self.carried {
            carried
                .refused
                .retain(|account, _| wanted.contains(account.as_str()));
        }
        for person in people {
            let bridged = Bridged {
                service: SERVICE.to_owned(),
                account: Some(person.account.clone()),
                name: shown_name(&person.name, &person.account),
                avatar: self.avatar(person.avatar_url.as_deref(), guild).await,
                linked: guild
                    .linked_person(&person.account)
                    .and_then(|person| hex::decode(person).ok())
                    .and_then(|bytes| <[u8; 32]>::try_from(bytes).ok()),
            };
            let held = self
                .carried
                .as_ref()
                .and_then(|carried| carried.guests.get(&person.account))
                .map(|seat| seat.bridged == bridged);
            match held {
                Some(true) => {}
                // A new name, picture or link: the heartbeat carries it.
                Some(false) => {
                    let participation = self
                        .carried
                        .as_ref()
                        .and_then(|carried| carried.guests.get(&person.account))
                        .map(|seat| seat.grant.participation);
                    match self.join(call, participation, bridged.clone()).await {
                        Ok(grant) => {
                            if let Some(seat) = self
                                .carried
                                .as_mut()
                                .and_then(|carried| carried.guests.get_mut(&person.account))
                            {
                                seat.bridged = bridged;
                                seat.granted(grant);
                            }
                        }
                        Err(_) => self.leave_guest(&person.account),
                    }
                }
                None => match self.join(call, None, bridged.clone()).await {
                    Ok(grant) => {
                        let mut seat = Seat::new(bridged, grant, self.bot.device_public_key);
                        seat.keep_carrier();
                        if let Some(carried) = &mut self.carried {
                            if let Some(namespace) = &seat.grant.publication {
                                carried.echoes.insert(namespace.clone());
                            }
                            carried.refused.remove(&person.account);
                            carried.guests.insert(person.account.clone(), seat);
                        }
                    }
                    Err(reason) => {
                        if let Some(carried) = &mut self.carried
                            && carried.refused.get(&person.account) != Some(&reason)
                        {
                            tracing::info!(room = %self.room, account = %person.account, %reason, "a Discord person is not carried into the call");
                            carried.refused.insert(person.account.clone(), reason);
                        }
                    }
                },
            }
            if let Some(seat) = self
                .carried
                .as_mut()
                .and_then(|carried| carried.guests.get_mut(&person.account))
            {
                seat.muted = person.muted;
            }
            self.show(call, &person.account);
        }
    }

    /// Show everyone in the call whether the guest `account` is muted in
    /// Discord (self-, server-muted or suppressed), as any participant
    /// shows their microphone; the relay ignores a repeat.
    fn show(&self, call: [u8; 32], account: &str) {
        let Some(seat) = self
            .carried
            .as_ref()
            .and_then(|carried| carried.guests.get(account))
        else {
            return;
        };
        self.rooms.voice_show_bridged(
            &self.bot,
            call,
            seat.grant.participation,
            Showing {
                muted: seat.muted,
                ..Showing::default()
            },
        );
    }

    /// The avatar at `url` as stored in the room: what the bridge already
    /// keeps of it, else fetched once.
    async fn avatar(&mut self, url: Option<&str>, guild: &::discord::Guild) -> Option<String> {
        let url = url?;
        if let Some(stored) = guild.stored_avatar(url) {
            return Some(stored.to_owned());
        }
        if let Some(known) = self.avatars.get(url) {
            return known.clone();
        }
        let fetched =
            match tokio::time::timeout(AVATAR_WAIT, self.lane.fetch_avatar(url.to_owned())).await {
                Ok(Ok(blob)) if content_hash(&blob) => Some(blob),
                Ok(Ok(_)) | Err(_) => None,
                Ok(Err(error)) => {
                    tracing::debug!(%error, "a Discord avatar was not fetched");
                    None
                }
            };
        self.avatars.insert(url.to_owned(), fetched.clone());
        fetched
    }

    async fn join(
        &self,
        call: [u8; 32],
        participation: Option<[u8; 32]>,
        bridged: Bridged,
    ) -> Result<VoiceGrant, String> {
        self.rooms
            .voice_join_bridged(
                &self.room,
                &self.bot,
                call,
                participation,
                bridged,
                self.outbound.clone(),
            )
            .await
    }

    /// Repeat every participation's join, as a participant does each
    /// [`HEARTBEAT`]; one the relay forgot joins afresh.
    async fn heartbeat(&mut self) {
        self.last_heartbeat = Instant::now();
        let Some(call) = self.carried.as_ref().and_then(|carried| carried.call) else {
            return;
        };
        let held: Vec<(Option<String>, [u8; 32], Bridged)> = self
            .carried
            .as_ref()
            .map(|carried| {
                carried
                    .own
                    .iter()
                    .chain(carried.guests.values())
                    .map(|seat| {
                        (
                            seat.bridged.account.clone(),
                            seat.grant.participation,
                            seat.bridged.clone(),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default();
        for (account, participation, bridged) in held {
            let renewed = match self.join(call, Some(participation), bridged.clone()).await {
                Ok(grant) => Ok(grant),
                Err(_) => self.join(call, None, bridged).await,
            };
            let Some(carried) = &mut self.carried else {
                return;
            };
            match renewed {
                Ok(grant) => {
                    if account.is_some()
                        && let Some(namespace) = &grant.publication
                    {
                        carried.echoes.insert(namespace.clone());
                    }
                    let seat = match &account {
                        None => carried.own.as_mut(),
                        Some(account) => carried.guests.get_mut(account),
                    };
                    if let Some(seat) = seat {
                        seat.granted(grant);
                    }
                    // A participation joined afresh shows nothing yet.
                    if let Some(account) = &account {
                        self.show(call, account);
                    }
                }
                Err(reason) => {
                    tracing::info!(room = %self.room, ?account, %reason, "a bridged participation ended");
                    match &account {
                        None => {
                            if let Some(mut seat) = carried.own.take() {
                                seat.close();
                            }
                            carried.call = None;
                        }
                        Some(account) => {
                            if let Some(mut seat) = carried.guests.remove(account) {
                                seat.close();
                            }
                            carried.refused.insert(account.clone(), reason);
                        }
                    }
                }
            }
        }
    }

    fn keep_carriers(&mut self) {
        if let Some(carried) = &mut self.carried {
            if let Some(own) = &mut carried.own {
                own.keep_carrier();
            }
            for guest in carried.guests.values_mut() {
                guest.keep_carrier();
            }
        }
    }

    /// One frame Discord person `speaker` said: their guest says it in the
    /// Tilo call, unless they are muted there.
    fn carry_in(&mut self, frame: OutsideAudio) {
        if self.muted.contains(&frame.speaker) {
            return;
        }
        if let Some(seat) = self
            .carried
            .as_mut()
            .and_then(|carried| carried.guests.get_mut(&frame.speaker))
        {
            seat.say(frame.opus);
        }
    }

    /// Whether the bridge hears a Tilo call: only then is there audio to
    /// hand Discord.
    fn hearing(&self) -> bool {
        self.carried
            .as_ref()
            .and_then(|carried| carried.own.as_ref())
            .is_some_and(|own| own.client.is_some())
    }

    /// Hand Discord what the Tilo call said since last time, but nothing
    /// Discord itself said.
    async fn carry_out(&self) {
        let mut frames = Vec::new();
        if let Some(carried) = &self.carried
            && let Some(client) = carried.own.as_ref().and_then(|own| own.client.as_ref())
        {
            while let Some(frame) = client.try_recv() {
                let namespace = &frame.publisher.namespace;
                if carried.echoes.contains(namespace) {
                    continue;
                }
                let speaker = namespace
                    .rsplit_once('/')
                    .map_or(namespace.as_str(), |(_, participation)| participation)
                    .to_owned();
                frames.push((speaker, frame.object.payload));
            }
        }
        for (speaker, opus) in frames {
            if self.lane.send_audio(speaker, opus).await.is_err() {
                // The worker is not running: what it missed is gone.
                break;
            }
        }
    }

    fn leave_guest(&mut self, account: &str) {
        let Some(carried) = &mut self.carried else {
            return;
        };
        let Some(mut seat) = carried.guests.remove(account) else {
            return;
        };
        seat.close();
        self.rooms
            .voice_leave_bridged(&self.bot, seat.grant.call, seat.grant.participation);
    }

    /// Leave the Tilo call, keeping the pair.
    fn leave_tilo(&mut self) {
        let Some(carried) = &mut self.carried else {
            return;
        };
        let seats: Vec<Seat> = carried
            .own
            .take()
            .into_iter()
            .chain(std::mem::take(&mut carried.guests).into_values())
            .collect();
        carried.call = None;
        for mut seat in seats {
            seat.close();
            self.rooms
                .voice_leave_bridged(&self.bot, seat.grant.call, seat.grant.participation);
        }
    }

    /// Carry no call: leave both sides.
    async fn let_go(&mut self) {
        self.leave_tilo();
        self.carried = None;
        self.ask(None, None).await;
    }
}

/// `wait` from now.
fn later(wait: Duration) -> Instant {
    let now = Instant::now();
    now.checked_add(wait).unwrap_or(now)
}

/// Whether `blob` names content by its BLAKE3 hex, as an avatar must.
fn content_hash(blob: &str) -> bool {
    blob.len() == 64
        && blob
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
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

/// Who is in Discord voice channel `channel`, the people the bridge keeps
/// out left out.
fn discord_people<'a>(
    report: &'a CallsReport,
    channel: &str,
    guild: &::discord::Guild,
) -> Vec<&'a tilo_connector::OutsidePerson> {
    report
        .calls
        .iter()
        .filter(|call: &&OutsideCall| call.call == channel)
        .flat_map(|call| &call.people)
        .filter(|person| !guild.blocks(&person.account))
        .collect()
}

/// The people in the Tilo call of voice channel `channel`, bridges left
/// out, each once.
fn tilo_people(list: &[CallSummary], channel: &str) -> Vec<[u8; 32]> {
    let mut people: Vec<[u8; 32]> = list
        .iter()
        .filter(|call| call.state.active && call.app_id == CHAT && call.channel_id == channel)
        .flat_map(|call| &call.participants)
        .filter(|participant: &&VoiceParticipant| participant.bridged.is_none())
        .map(|participant| participant.user_public_key)
        .collect();
    people.sort_unstable();
    people.dedup();
    people
}

/// The name a Discord person is shown by: theirs as an appearance may
/// carry it, else their account's.
fn shown_name(name: &str, account: &str) -> String {
    tilo_actions::appearance::Appearance::new(name, None, None).map_or_else(
        |_| {
            let fallback: String = format!("Discord user {account}").chars().take(80).collect();
            fallback
        },
        |appearance| appearance.name,
    )
}

#[cfg(test)]
mod tests;
