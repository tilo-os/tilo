//! Voice calls on the collaboration plane: the channel this device's call is
//! in, and who is in the channels every room this session takes part in
//! shows.
//!
//! Being in a call is the presence every member of the room reads: joining
//! publishes the channel, leaving stops saying it, and a channel's roster is
//! what the members in it say of themselves. The session publishes this from
//! its pass ([`Calls::follow`]), and the calls themselves through the service
//! an app reaches ([`tilo_native_media`]), whose carrier is this plane
//! ([`Factory`]).
//!
//! A channel id is the whole of a call's identity on the wire: the presence
//! names the channel and what it is called, and a roster says who is in a
//! channel, with no room beside it.
//!
//! What a call carries is the lane of [`tilo_call_lane`] on this session's
//! own endpoint: the channel's keys seal its audio, the room's admitted
//! devices are the ones it admits, and the people the room says are in the
//! channel are the ones this device opens a lane to.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use runtime_collab::native_media::{Input, Issue, Roster, State};
use tilo_desktop_model::RoomId;
use tilo_lock::MutexRecover as _;
use tilo_native_media::{CallTarget, MediaFactory, MediaSession, Prepare, Rosters};

use crate::call_devices::{Audio, Source, Voice};
use crate::sync::{self, LiveCall};

/// How often a live call moves its audio: one frame's worth, so a captured
/// frame is sealed and sent as soon as the microphone has it.
const PUMP: Duration = Duration::from_millis(10);
/// How long a device waits before trying a lane to a peer of the call again.
const REDIAL: Duration = Duration::from_millis(500);
/// Frames each audio datagram repeats, so one lost packet costs no silence.
const REPEATS: usize = 2;
/// What the person's answer covers: the microphone, by the name every app's
/// device capabilities use.
const MICROPHONE: &str = "microphone";

/// The calls of every room this session takes part in.
pub(crate) struct Calls {
    /// The calls here, as the people in them say.
    live: Mutex<Vec<LiveCall>>,
    /// Who is in each channel, as the members say: what a voice channel
    /// shows.
    rosters: Mutex<Vec<Roster>>,
    /// What the rosters last said, to wake the apps watching them only when
    /// they moved.
    said: Mutex<String>,
    /// Woken whenever a room's calls may have changed.
    changed: Arc<tokio::sync::Notify>,
}

impl Calls {
    #[must_use]
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self {
            live: Mutex::new(Vec::new()),
            rosters: Mutex::new(Vec::new()),
            said: Mutex::new(String::new()),
            changed: Arc::new(tokio::sync::Notify::new()),
        })
    }

    /// Take in the calls everyone here says they are in, and the channel
    /// this device joined (if any), so a voice channel shows a join or a
    /// leave on this pass.
    pub(crate) fn follow(&self, sync: &sync::Client, own: Option<&str>) {
        let live = sync.calls();
        let mut people: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        for call in &live {
            let in_call = people.entry(call.channel.clone()).or_default();
            for caller in &call.people {
                if let Some(person) = &caller.user {
                    in_call.insert(person.clone());
                }
            }
        }
        // This device's own call: nobody here hears it from this session, so
        // it is said from what the session itself published.
        if let (Some(channel), Some(person)) = (own, sync.own().user) {
            people.entry(channel.to_owned()).or_default().insert(person);
        }
        let rosters: Vec<Roster> = people
            .into_iter()
            .map(|(channel_id, people)| Roster {
                channel_id,
                people: people.into_iter().collect(),
                ..Roster::default()
            })
            .collect();
        let said = format!("{rosters:?}");
        let moved = {
            let mut last = self.said.lock_recover();
            let moved = *last != said;
            last.clone_from(&said);
            moved
        };
        *self.rosters.lock_recover() = rosters;
        *self.live.lock_recover() = live;
        if moved {
            self.changed.notify_waiters();
        }
    }

    /// What `channel` is called, as the people in it say; empty when nobody
    /// named it.
    pub(crate) fn name(&self, channel: &str) -> String {
        self.live
            .lock_recover()
            .iter()
            .find(|call| call.channel == channel)
            .map(|call| call.name.clone())
            .unwrap_or_default()
    }

    /// Who is in each channel, sorted by channel.
    pub(crate) fn rosters(&self) -> Vec<Roster> {
        self.rosters.lock_recover().clone()
    }

    /// The devices the room says are in `channel`'s call, this one among
    /// them: who a call opens a lane to.
    fn callers(&self, channel: &str) -> Vec<[u8; 32]> {
        let live = self.live.lock_recover();
        live.iter()
            .filter(|call| call.channel == channel)
            .flat_map(|call| &call.people)
            .filter_map(|caller| caller.id.parse::<iroh::EndpointId>().ok())
            .map(|device| *device.as_bytes())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }
}

/// What voice channel `channel` is called, as the app that holds it
/// published the row its call is joined from: its label, empty when the app
/// publishes no such row.
pub(crate) fn channel_name(
    surfaces: &[tilo_action_broker::collab_app_broker::PublishedSurface],
    (workspace, app, record): (&str, &str, &str),
    channel: &str,
) -> Option<String> {
    let row = format!("{record}/channels/{channel}/call");
    surfaces
        .iter()
        .filter(|surface| surface.app_id == app && surface.route.cluster_id == workspace)
        .flat_map(|surface| &surface.actions)
        .find(|action| action.resource == row)
        .map(|action| action.resource_label.trim().to_owned())
        .filter(|name| !name.is_empty())
}

/// What answers the session endpoint's call lanes: the one live call, or a
/// refusal, since a device in no call takes no lane.
#[derive(Clone, Default)]
pub(crate) struct LaneSlot(Arc<Mutex<Option<tilo_call_lane::Calls>>>);

impl LaneSlot {
    /// Answer `calls` on the endpoint from now on.
    pub(crate) fn install(&self, calls: tilo_call_lane::Calls) {
        *self.0.lock_recover() = Some(calls);
    }

    /// Stop answering `calls`, unless another call took the slot since.
    fn clear_if(&self, calls: &tilo_call_lane::Calls) {
        let mut slot = self.0.lock_recover();
        if slot
            .as_ref()
            .is_some_and(|held| held.hello() == calls.hello())
        {
            slot.take();
        }
    }

    /// What the endpoint's router hands a connection on [`tilo_call_lane`]'s
    /// protocol.
    pub(crate) fn handler(&self) -> LaneHandler {
        LaneHandler(self.clone())
    }
}

/// The session endpoint's handler for call lanes, resolved per connection:
/// the call the slot holds now, or a refusal.
#[derive(Clone)]
pub(crate) struct LaneHandler(LaneSlot);

impl std::fmt::Debug for LaneHandler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LaneHandler")
            .field("answering", &self.0.0.lock_recover().is_some())
            .finish()
    }
}

impl iroh::protocol::ProtocolHandler for LaneHandler {
    async fn accept(
        &self,
        connection: iroh::endpoint::Connection,
    ) -> Result<(), iroh::protocol::AcceptError> {
        let Some(calls) = self.0.0.lock_recover().clone() else {
            return Err(iroh::protocol::AcceptError::from_err(
                std::io::Error::other("this device is in no call"),
            ));
        };
        calls.accept(connection).await
    }
}

/// The devices a room's governance admits, read fresh: what a call's roster
/// is made of, and who this device accepts frames from.
pub(crate) type Admitted = Arc<dyn Fn(&str) -> Vec<[u8; 32]> + Send + Sync>;

/// Where a call's carrier reads what only the session knows: its endpoint,
/// the channel service, the room's governance, the person's answers, and
/// what the audio runs on.
pub(crate) struct Sources {
    /// The calls of the rooms this session takes part in, as the loop heard
    /// them: who else is in the channel this device joins.
    pub(crate) calls: Arc<Calls>,
    /// The session's own endpoint, where the call's lanes run. `None` for a
    /// session taking part in no transport, which carries no call.
    pub(crate) endpoint: Option<Arc<std::sync::OnceLock<iroh::Endpoint>>>,
    /// What answers that endpoint's call lanes.
    pub(crate) lane: Option<LaneSlot>,
    /// This device's channel keys: the ones a call in the channel seals its
    /// audio with.
    pub(crate) keys: Option<tilo_action_broker::channel_encryption::ChannelKeyProvider>,
    /// The devices the room `room` admits, which a call's roster names.
    pub(crate) roster: Option<Admitted>,
    /// The person's prompts: the microphone is asked for through them.
    pub(crate) questions: Option<tilo_service::Questions>,
    /// What the call hears and is heard by.
    pub(crate) audio: Audio,
    /// Where a test reads the samples the call played, in place of a
    /// speaker.
    #[cfg(test)]
    pub(crate) played: Option<Arc<Mutex<Vec<f32>>>>,
}

/// This plane as the carrier of a call: being in the channel is joining it,
/// which the session's presence then says to everyone in the room, and a
/// lane on the session's own endpoint carries its audio.
pub(crate) struct Factory {
    sources: Arc<Sources>,
    /// The person's answers about the microphone: the prompt the session's
    /// apps get, so one allowance covers every call of the session.
    consents: Option<tilo_portal::consent::Consents>,
}

impl Sources {
    /// What a session's own calls read: its endpoint, the channel service,
    /// the room's governance, and the person's prompts.
    #[must_use]
    pub(crate) fn carrier(
        calls: Arc<Calls>,
        endpoint: Arc<std::sync::OnceLock<iroh::Endpoint>>,
        lane: LaneSlot,
        keys: Option<tilo_action_broker::channel_encryption::ChannelKeyProvider>,
        roster: Admitted,
        questions: Option<tilo_service::Questions>,
    ) -> Self {
        Self {
            calls,
            endpoint: Some(endpoint),
            lane: Some(lane),
            keys,
            roster: Some(roster),
            questions,
            audio: Audio::Devices,
            #[cfg(test)]
            played: None,
        }
    }
}

impl Factory {
    #[must_use]
    pub(crate) fn new(sources: Sources) -> Self {
        let consents = sources
            .questions
            .clone()
            .map(|questions| tilo_portal::consent::Consents::new(questions, None));
        Self {
            sources: Arc::new(sources),
            consents,
        }
    }
}

impl MediaFactory for Factory {
    fn prepare(&self, target: CallTarget) -> Prepare {
        let sources = Arc::clone(&self.sources);
        let consents = self.consents.clone();
        Box::pin(async move { open(&sources, consents, target).await })
    }

    fn rosters(&self, _target: CallTarget) -> Rosters {
        let rosters = self.sources.calls.rosters();
        Box::pin(async move { rosters })
    }

    fn rosters_changed(&self) -> Option<Arc<tokio::sync::Notify>> {
        Some(Arc::clone(&self.sources.calls.changed))
    }
}

/// Start this device's side of `target`'s call, or say why it cannot carry
/// one here.
async fn open(
    sources: &Sources,
    consents: Option<tilo_portal::consent::Consents>,
    target: CallTarget,
) -> Result<Box<dyn MediaSession>, Issue> {
    let (Some(endpoint), Some(lane)) = (
        sources
            .endpoint
            .as_ref()
            .and_then(|slot| slot.get().cloned()),
        sources.lane.clone(),
    ) else {
        // No transport, no lane: nothing could carry the call.
        return Err(Issue::Unavailable);
    };
    let room = RoomId::parse(&target.workspace).ok_or(Issue::Unavailable)?;
    let channel = collab_types::document_channel_id(&target.app, &target.workspace, &target.record);
    let access = sources
        .keys
        .as_ref()
        .and_then(tilo_action_broker::channel_encryption::ChannelKeyProvider::get)
        .ok_or(Issue::Unavailable)?;
    let (granted, _) = access.access(room, channel);
    // Keys that are on their way still carry the call: it opens joining the
    // channel, and the worker takes them when they arrive. Only a device the
    // channel has no keys for at all leaves it uncarried.
    let keys = match granted {
        tilo_action_broker::channel_encryption::ChannelAccess::Ready(keys) => Some(keys),
        tilo_action_broker::channel_encryption::ChannelAccess::Waiting => None,
        _ => return Err(Issue::Unavailable),
    };
    let roster = admitted(sources, &target.workspace);
    let own = *endpoint.id().as_bytes();
    let (calls, accepted) = tilo_call_lane::Calls::new(
        tilo_call_lane::Hello {
            workspace: target.workspace.clone(),
            channel,
            call: target.channel.clone(),
            lane: tilo_call_lane::Lane::Audio,
        },
        roster.clone(),
    );
    let audio = sources.audio;
    let voice = tokio::task::spawn_blocking(move || Voice::open(audio))
        .await
        .map_err(|_| Issue::Unavailable)??;
    let microphones = voice.sources();
    let offered: Vec<Input> = microphones
        .iter()
        .map(|source| Input {
            id: source.id.clone(),
            label: source.label.clone(),
        })
        .collect();
    let microphone = keys
        .as_ref()
        .and_then(|keys| tilo_call_lane::Microphone::new(keys, own, REPEATS).ok());
    let live = keys.is_some();
    let current = keys.map(|keys| Current {
        opener: tilo_call_lane::Opener::new(keys.clone(), roster.clone()),
        keys,
        roster,
    });
    let shared = Arc::new(Shared {
        lane,
        endpoint,
        calls: calls.clone(),
        own,
        access,
        room,
        channel,
        workspace: target.workspace.clone(),
        admitted: sources.roster.clone(),
        current: Mutex::new(current),
        side: Mutex::new(Side { voice, microphone }),
        peers: Mutex::new(Vec::new()),
        sources: microphones,
        caller: tilo_portal::PortalCaller {
            app_id: target.app.clone(),
            workspace: Some(target.workspace.clone()),
            sandboxed: false,
        },
        call: target.channel.clone(),
        consents,
        live: AtomicBool::new(live),
        closed: AtomicBool::new(false),
        deafened: AtomicBool::new(false),
        issue: Mutex::new(None),
        #[cfg(test)]
        played: sources.played.clone(),
    });
    let dialled = sources.calls.callers(&target.channel);
    tokio::spawn(run(
        Arc::clone(&shared),
        dialled
            .into_iter()
            .filter(|device| *device != own)
            .collect(),
        accepted,
    ));
    Ok(Box::new(Call {
        shared,
        inputs: offered,
    }))
}

/// The devices the room `workspace` admits: who may send to this device in
/// its channels' calls.
fn admitted(sources: &Sources, workspace: &str) -> tilo_call_lane::Roster {
    let devices = sources
        .roster
        .as_ref()
        .map(|admitted| admitted(workspace))
        .unwrap_or_default();
    tilo_call_lane::Roster::new(devices)
}

/// One call this device is in, as the call service holds it: where the call
/// stands, and what it sends.
pub(crate) struct Call {
    shared: Arc<Shared>,
    inputs: Vec<Input>,
}

impl MediaSession for Call {
    fn tick(&mut self, _now: Instant) -> Result<State, Issue> {
        let issue = *self.shared.issue.lock_recover();
        if let Some(issue) = issue {
            return Err(issue);
        }
        if self.shared.closed.load(Ordering::Acquire) {
            return Err(Issue::SessionEnded);
        }
        Ok(if self.shared.live.load(Ordering::Acquire) {
            State::Live
        } else {
            State::Joining
        })
    }

    fn inputs(&self) -> Vec<Input> {
        self.inputs.clone()
    }

    fn start_capture(&mut self, input: &str) -> Result<(), Issue> {
        self.shared.start_capture(input)
    }

    fn stop_capture(&mut self) {
        self.shared.stop_capture();
    }

    fn set_deafened(&mut self, deafened: bool) {
        self.shared.set_deafened(deafened);
    }

    fn close(&mut self) {
        self.shared.close();
    }
}

/// The channel's keys and who may read them, and what the room's governance
/// admitted at the same time: what seals this call's frames.
struct Current {
    keys: channel_crypto::ChannelContentKeys,
    roster: tilo_call_lane::Roster,
    opener: tilo_call_lane::Opener,
}

/// This call's own audio side: what it plays, and the microphone it seals
/// frames with while the person is heard.
struct Side {
    voice: Voice,
    microphone: Option<tilo_call_lane::Microphone>,
}

struct Shared {
    lane: LaneSlot,
    endpoint: iroh::Endpoint,
    calls: tilo_call_lane::Calls,
    /// This device's key: who its frames say they are from.
    own: [u8; 32],
    /// Where the channel's keys come from when its membership moves.
    access: tilo_action_broker::channel_encryption::ChannelKeyAccess,
    room: RoomId,
    channel: collab_types::ChannelId,
    workspace: String,
    /// The room's admitted devices, re-read while the call lasts.
    admitted: Option<Admitted>,
    /// The channel's keys and roster, once this device holds them: the call
    /// is up from then, and joining until.
    current: Mutex<Option<Current>>,
    side: Mutex<Side>,
    /// The lanes this device opened or took, one per device in the call.
    peers: Mutex<Vec<tilo_call_lane::Peer>>,
    /// The microphones this call may send.
    sources: Vec<Source>,
    caller: tilo_portal::PortalCaller,
    call: String,
    consents: Option<tilo_portal::consent::Consents>,
    /// This call's lane holds keys and answers the endpoint: the call is up.
    live: AtomicBool,
    closed: AtomicBool,
    deafened: AtomicBool,
    /// Why the call ended, once it did.
    issue: Mutex<Option<Issue>>,
    /// Where a test reads the samples the call played.
    #[cfg(test)]
    played: Option<Arc<Mutex<Vec<f32>>>>,
}

impl Shared {
    fn start_capture(self: &Arc<Self>, input: &str) -> Result<(), Issue> {
        if self.closed.load(Ordering::Acquire) {
            return Err(Issue::SourceLost);
        }
        let Some(source) = self
            .sources
            .iter()
            .find(|source| source.id == input)
            .cloned()
        else {
            return Err(Issue::SourceLost);
        };
        if let Some(consents) = &self.consents
            && !consents.allowed(&self.caller, MICROPHONE)
        {
            // The person has not allowed their microphone yet: ask them
            // through the session's own sheet, and open nothing until they
            // answer. Asking again while the question waits is refused by
            // the service itself.
            let consents = consents.clone();
            let caller = self.caller.clone();
            let handle = format!("call-microphone:{}", self.call);
            let question = tilo_portal::consent::AccessQuestion {
                capability: MICROPHONE.to_owned(),
                reason: Some("to be heard in the call".to_owned()),
                may_remember: false,
                text: None,
            };
            tokio::spawn(async move {
                let _ = consents.decide(&caller, &handle, question).await;
            });
            return Err(Issue::PermissionDenied);
        }
        let keys = {
            let current = self.current.lock_recover();
            current.as_ref().map(|current| current.keys.clone())
        };
        let Some(keys) = keys else {
            // Nothing to seal a microphone with: the channel's keys are not
            // here yet.
            return Err(Issue::Unavailable);
        };
        let still_current = {
            let call = Arc::downgrade(self);
            Arc::new(move || call.upgrade().is_some_and(|call| !call.closed()))
        };
        let mut side = self.side.lock_recover();
        side.voice.start(&source, still_current)?;
        side.microphone = tilo_call_lane::Microphone::new(&keys, self.own, REPEATS).ok();
        drop(side);
        Ok(())
    }

    fn stop_capture(&self) {
        let mut side = self.side.lock_recover();
        side.voice.stop();
        side.microphone = None;
    }

    fn set_deafened(&self, deafened: bool) {
        self.deafened.store(deafened, Ordering::Release);
        self.side.lock_recover().voice.set_deafened(deafened);
    }

    fn close(&self) {
        if self.closed.swap(true, Ordering::AcqRel) {
            return;
        }
        self.lane.clear_if(&self.calls);
        for peer in self.peers.lock_recover().drain(..) {
            peer.close();
        }
        let mut side = self.side.lock_recover();
        side.microphone = None;
        side.voice.close();
    }

    fn closed(&self) -> bool {
        self.closed.load(Ordering::Acquire)
    }

    /// Take `peer`'s lane: from now on the call hears what it sends, until
    /// it ends.
    fn join(self: &Arc<Self>, peer: tilo_call_lane::Peer) {
        if self.closed() {
            peer.close();
            return;
        }
        let remote = peer.remote();
        let hearing = Arc::downgrade(self);
        let receiver = peer.clone();
        tokio::spawn(async move {
            while let Ok(datagram) = receiver.received().await {
                let Some(shared) = hearing.upgrade() else {
                    return;
                };
                if shared.closed() {
                    return;
                }
                shared.hear(remote, &datagram);
            }
        });
        self.peers.lock_recover().push(peer);
    }

    /// Play what `sender` sent in `datagram`, unless the person is deafened:
    /// a deafened call drops the frames rather than holding them.
    fn hear(&self, sender: [u8; 32], datagram: &[u8]) {
        if self.deafened.load(Ordering::Acquire) {
            return;
        }
        let heard = {
            let held = self.current.lock_recover();
            let heard = held
                .as_ref()
                .map(|current| tilo_call_lane::heard(&current.opener, sender, datagram));
            drop(held);
            heard
        };
        let Some(Ok(frames)) = heard else {
            return;
        };
        let now = Instant::now();
        let mut side = self.side.lock_recover();
        for frame in frames {
            if side
                .voice
                .receive(sender, frame.sequence, frame.timestamp_48k, frame.opus, now)
                .is_err()
            {
                break;
            }
        }
    }

    /// Move the call's audio once: send what the microphone captured, and
    /// play what arrived.
    fn pump(&self) {
        let now = Instant::now();
        let mut side = self.side.lock_recover();
        let captured = match side.voice.pump(now) {
            Ok(captured) => captured,
            Err(issue) => {
                side.microphone = None;
                *self.issue.lock_recover() = Some(issue);
                return;
            }
        };
        let mut sent = Vec::with_capacity(captured.len());
        for frame in captured {
            let Some(microphone) = &mut side.microphone else {
                break;
            };
            match microphone.frame(frame.timestamp_48k, &frame.payload) {
                Ok(Some(datagram)) => sent.push(datagram),
                Ok(None) => {}
                Err(_) => break,
            }
        }
        if side.voice.playout(now).is_err() {
            side.microphone = None;
            *self.issue.lock_recover() = Some(Issue::TransportFailed);
            return;
        }
        #[cfg(test)]
        if let Some(played) = &self.played {
            let samples = side.voice.played();
            if !samples.is_empty() {
                played.lock_recover().extend(samples);
            }
        }
        drop(side);
        if sent.is_empty() {
            return;
        }
        let mut peers = self.peers.lock_recover();
        for datagram in &sent {
            for peer in std::mem::take(&mut *peers) {
                if peer.send(datagram.clone()).is_ok() {
                    peers.push(peer);
                } else {
                    peer.close();
                }
            }
        }
        drop(peers);
    }

    /// Re-read what the room admits and what the channel's keys are: a
    /// membership change reaches the call as it happens, and a channel whose
    /// keys were still on their way when the call joined brings it up.
    fn refresh(&self) {
        let roster = self
            .admitted
            .as_ref()
            .map(|admitted| tilo_call_lane::Roster::new(admitted(&self.workspace)));
        let (granted, _) = self.access.access(self.room, self.channel);
        let keys = match granted {
            tilo_action_broker::channel_encryption::ChannelAccess::Ready(keys) => Some(keys),
            // Keys still on their way keep a joining call joining.
            tilo_action_broker::channel_encryption::ChannelAccess::Waiting => None,
            // A channel this device may no longer read, or has no store for,
            // carries no call: one that held its keys ends, and one that
            // never did says it cannot be carried here.
            _ => {
                *self.issue.lock_recover() = Some(if self.live.load(Ordering::Acquire) {
                    Issue::TransportFailed
                } else {
                    Issue::Unavailable
                });
                return;
            }
        };
        let mut rekeyed = false;
        {
            let mut held = self.current.lock_recover();
            match (held.as_mut(), keys) {
                (Some(current), Some(keys)) if keys.epoch() != current.keys.epoch() => {
                    current.keys = keys;
                    if let Some(roster) = roster {
                        self.calls.set_roster(roster.clone());
                        current.roster = roster;
                    }
                    current.opener =
                        tilo_call_lane::Opener::new(current.keys.clone(), current.roster.clone());
                    rekeyed = true;
                }
                (Some(current), _) => {
                    if let Some(roster) =
                        roster.filter(|roster| roster.devices().ne(current.roster.devices()))
                    {
                        self.calls.set_roster(roster.clone());
                        current.roster = roster;
                        current.opener = tilo_call_lane::Opener::new(
                            current.keys.clone(),
                            current.roster.clone(),
                        );
                    }
                }
                (None, Some(keys)) => {
                    let roster = roster.unwrap_or_default();
                    self.calls.set_roster(roster.clone());
                    *held = Some(Current {
                        opener: tilo_call_lane::Opener::new(keys.clone(), roster.clone()),
                        keys,
                        roster,
                    });
                    self.live.store(true, Ordering::Release);
                }
                (None, None) => {}
            }
            drop(held);
        }
        if rekeyed {
            let keys = self
                .current
                .lock_recover()
                .as_ref()
                .map(|current| current.keys.clone());
            let mut side = self.side.lock_recover();
            if side.microphone.is_some() {
                side.microphone = keys.and_then(|keys| {
                    tilo_call_lane::Microphone::new(&keys, self.own, REPEATS).ok()
                });
            }
        }
    }
}

/// Carry `shared`'s call: answer the endpoint's lanes, open one to every
/// device the room says is in the channel, and move its audio until it ends.
async fn run(
    shared: Arc<Shared>,
    dialled: Vec<[u8; 32]>,
    mut accepted: tokio::sync::mpsc::UnboundedReceiver<tilo_call_lane::Peer>,
) {
    shared.lane.install(shared.calls.clone());
    shared.live.store(true, Ordering::Release);
    let (joined, mut arrived) = tokio::sync::mpsc::unbounded_channel();
    for device in dialled {
        let calls = shared.calls.clone();
        let endpoint = shared.endpoint.clone();
        let joined = joined.clone();
        let call = Arc::downgrade(&shared);
        tokio::spawn(async move {
            let Ok(remote) = iroh::EndpointId::from_bytes(&device) else {
                return;
            };
            loop {
                let Some(live) = call.upgrade() else {
                    return;
                };
                if live.closed() {
                    return;
                }
                drop(live);
                match calls.dial(&endpoint, iroh::EndpointAddr::new(remote)).await {
                    Ok(peer) => {
                        let _ = joined.send(peer);
                        return;
                    }
                    Err(_) => tokio::time::sleep(REDIAL).await,
                }
            }
        });
    }
    let mut beat = tokio::time::interval(PUMP);
    beat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut ticks = 0_u32;
    loop {
        if shared.closed() {
            break;
        }
        tokio::select! {
            Some(peer) = accepted.recv() => shared.join(peer),
            Some(peer) = arrived.recv() => shared.join(peer),
            _ = beat.tick() => {}
        }
        shared.pump();
        ticks = ticks.wrapping_add(1);
        if ticks.is_multiple_of(100) {
            shared.refresh();
        }
    }
    shared.close();
}

#[cfg(test)]
mod tests {
    //! A call on the native desk, in two shapes: an app joins a voice channel
    //! on a session that can carry one, and the person and the people in the
    //! room hear each other; and a session with no transport ends the call it
    //! cannot carry, saying why.
    //!
    //! The audio here is the call's own: the same encoder, lane, sealing and
    //! playout a device's call runs, ending in memory instead of a speaker,
    //! and a tone instead of a microphone.

    use std::collections::HashMap;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use crate::fabric::MediaContinuity;
    use channel_crypto::VaultKey;
    use collab_types::presence::{Availability, PresenceKey};
    use collab_types::{ChannelRoute, Ed25519PublicKey, PeerPresence};
    use iroh::EndpointId;
    use runtime_collab::native_media::{Command, Request, State};
    use tilo_action_broker::channel_encryption::{
        ChannelDirectory, ChannelEncryption, ChannelEncryptionConfig, ChannelKeyAccess,
        ChannelKeyProvider,
    };
    use tilo_action_broker::collab_core::BrokerChannels;
    use tilo_compositor::OutputConfig;
    use tilo_desktop_model::{DesktopTree, RoomId, Workspace, WorkspaceId};
    use tilo_network_actor::{HostIdentity, NetEvent, room_topic_id};

    use super::*;
    use crate::sync::{Client, OwnPresence};
    use crate::{ActionPlane, Options, Session};

    const ROOM: &str = "0b6a8f2c-3d4e-4f5a-8b9c-0d1e2f3a4b5c";
    const CHANNEL: &str = "c_standup";
    const APP: &str = "so.tilo.chat";
    const RECORD: &str = "chat";
    /// The app id the desktop of a room says its own presence under.
    const DESKTOP_PRESENCE_APP: &str = "tilo-shell";
    /// What the tone's playout reads as heard, against the silence a call
    /// plays while nobody speaks.
    const HEARD: f32 = 0.02;
    /// How long a call's audio has to begin, or stop, playing here.
    const DEADLINE: Duration = Duration::from_secs(30);

    fn public(seed: u8) -> EndpointId {
        iroh::SecretKey::from_bytes(&[seed; 32]).public()
    }

    /// The person a device is a device of, in the hex everyone here knows
    /// them by.
    fn person(seed: u8) -> String {
        hex::encode(
            ed25519_dalek::SigningKey::from_bytes(&[seed; 32])
                .verifying_key()
                .to_bytes(),
        )
    }

    /// One device's key, as the room's governance names it.
    fn key(seed: u8) -> Ed25519PublicKey {
        Ed25519PublicKey::from_bytes(
            ed25519_dalek::SigningKey::from_bytes(&[seed; 32])
                .verifying_key()
                .to_bytes(),
        )
    }

    /// The person all of a call's devices are devices of.
    fn person_key() -> ed25519_dalek::SigningKey {
        ed25519_dalek::SigningKey::from_bytes(&[21; 32])
    }

    fn room() -> RoomId {
        RoomId::parse(ROOM).expect("a room id")
    }

    fn target() -> CallTarget {
        CallTarget {
            workspace: ROOM.to_owned(),
            app: APP.to_owned(),
            record: RECORD.to_owned(),
            channel: CHANNEL.to_owned(),
        }
    }

    /// A desk showing one workspace, in the room.
    fn desk_tree() -> DesktopTree {
        let room = room();
        let workspace = Workspace::in_room(WorkspaceId::of_room(room), room, "Desk");
        DesktopTree {
            workspaces: vec![workspace.clone()],
            active: workspace.id,
            ..DesktopTree::default()
        }
    }

    /// A directory for the plane's socket that only this user reaches.
    fn private_home() -> tempfile::TempDir {
        use std::os::unix::fs::PermissionsExt as _;
        let root = tempfile::tempdir().expect("a directory to serve the plane in");
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700))
            .expect("the plane's directory is this user's");
        root
    }

    /// One of the person's devices, on its own loopback endpoint, taking part
    /// in the collaboration plane as its certified self.
    fn device(person: &ed25519_dalek::SigningKey, seed: u8, label: &str) -> Client {
        let key = ed25519_dalek::SigningKey::from_bytes(&[seed; 32]);
        let mut cert = tilo_user_identity::DeviceCert {
            user_public_key: person.verifying_key().to_bytes(),
            device_public_key: key.verifying_key().to_bytes(),
            device_label: label.to_owned(),
            capabilities: tilo_user_identity::DeviceCapabilities::member(),
            issued_at_ns: tilo_clock::unix_now_ns(),
            expires_at_ns: None,
            signature: [0; 64],
        };
        tilo_user_identity::sign_device_cert(&mut cert, person)
            .expect("the person signs their device");
        Client::start(
            Some(HostIdentity {
                device_cert: cert,
                device_signing_key: key,
                workspace_device_certs: HashMap::new(),
                workspace_revocations: HashMap::new(),
                identity_paths: None,
            }),
            None,
            OwnPresence::default(),
            Vec::new(),
            MediaContinuity::default(),
            Arc::new(|| {}),
        )
        .expect("a loopback endpoint binds")
    }

    /// The room as this device knows its governance: one device governs it
    /// and both of a call's devices may read its channels.
    struct Directory {
        governor: Ed25519PublicKey,
        admitted: std::collections::BTreeSet<Ed25519PublicKey>,
    }

    impl ChannelDirectory for Directory {
        fn governor_of(&self, _room: RoomId) -> Option<Ed25519PublicKey> {
            Some(self.governor)
        }
        fn allowed_devices(
            &self,
            _room: RoomId,
            _channel: collab_types::ChannelId,
            _route: Option<&ChannelRoute>,
            _now: u64,
        ) -> Option<std::collections::BTreeSet<Ed25519PublicKey>> {
            Some(self.admitted.clone())
        }
    }

    /// One channel's keys, as the device `device` of a room `governor` runs
    /// holds them: ready at once under the room's own device, and still on
    /// their way under any other.
    fn keys(device: u8, governor: u8, admitted: &[u8]) -> (ChannelKeyProvider, tempfile::TempDir) {
        let directory = tempfile::tempdir().expect("a directory to keep channel state in");
        let broker = Arc::new(Mutex::new(BrokerChannels::default()));
        let encryption = ChannelEncryption::open(
            ChannelEncryptionConfig {
                root: directory.path().join("channels"),
                vault: VaultKey::new([governor; 32]),
                device_key: ed25519_dalek::SigningKey::from_bytes(&[device; 32]),
            },
            Box::new(Directory {
                governor: key(governor),
                admitted: admitted.iter().copied().map(key).collect(),
            }),
            broker,
        )
        .expect("a channel service");
        let provider = ChannelKeyProvider::default();
        provider.set(Some(ChannelKeyAccess::new(encryption)));
        (provider, directory)
    }

    /// A session taking part in the room through `sync`, showing the desk.
    fn session(sync: &std::rc::Rc<Client>, socket: &std::path::Path) -> Session {
        let session = Session::headless(
            Options {
                presentation: ui::headless::fixture_runner_inputs,
                sync: Some(std::rc::Rc::clone(sync)),
                action_plane: ActionPlane::At(socket.to_path_buf()),
                ..Options::default()
            },
            &[OutputConfig::new("HEADLESS-1", 1280, 800)],
        )
        .expect("session");
        session.handle().replace_tree(desk_tree());
        session
    }

    fn step(session: &mut Session) {
        for _ in 0..4 {
            session.step(Duration::from_millis(1)).expect("step");
        }
    }

    /// One thing `from` said in the room.
    fn said(from: u8, msg: neighbour_wire::NeighbourMsg) -> NetEvent {
        NetEvent::Msg {
            ws: room_topic_id(room()),
            peer: public(from),
            membership_epoch: None,
            replayed: false,
            msg,
        }
    }

    /// A device's identity: the person it is a device of, and its name here.
    fn hello(from: u8, label: &str) -> NetEvent {
        let device_key = public(from);
        let root = ed25519_dalek::SigningKey::from_bytes(&[from; 32]);
        let mut cert = tilo_user_identity::DeviceCert {
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key: *device_key.as_bytes(),
            device_label: label.to_owned(),
            capabilities: tilo_user_identity::DeviceCapabilities {
                can_own_workspaces: false,
                can_admit_devices: false,
                can_act_as_host: false,
                can_read_all_workspaces: false,
                foreign: false,
                can_administer_devices: false,
            },
            issued_at_ns: 0,
            expires_at_ns: None,
            signature: [0; 64],
        };
        tilo_user_identity::sign_device_cert(&mut cert, &root).expect("a certified device");
        said(
            from,
            neighbour_wire::NeighbourMsg::IdentityHello {
                protocol: neighbour_wire::PEER_PROTOCOL_VERSION,
                cert: Box::new(cert),
                revocations: Vec::new(),
            },
        )
    }

    /// One presence `from` said of themselves in the room.
    fn presence(from: u8, payload: collab_types::PeerPresencePayload) -> NetEvent {
        let device_key = public(from);
        said(
            from,
            neighbour_wire::NeighbourMsg::AppPresenceV2 {
                peer: neighbour_wire::peer_handle(device_key.as_bytes()),
                app_id: DESKTOP_PRESENCE_APP.to_owned(),
                cluster_id: ROOM.to_owned(),
                presence: PeerPresence {
                    key: postcard::to_stdvec(&PresenceKey::Workspace {
                        id: ROOM.to_owned(),
                    })
                    .expect("a key"),
                    payload,
                },
                colour: None,
            },
        )
    }

    /// How available `from` says they are, and the call they say they are in.
    fn availability(from: u8, call: Option<String>) -> NetEvent {
        presence(
            from,
            collab_types::PeerPresencePayload::Availability {
                state: Availability::default().id().to_owned(),
                call,
            },
        )
    }

    /// The call `CHANNEL` is, as a person in it publishes it.
    fn call_presence() -> String {
        ui::CallPresence {
            channel: CHANNEL.to_owned(),
            name: "Standup".to_owned(),
        }
        .encode()
    }

    /// What a call's playout reads over `over` here: the tone's energy, or
    /// the silence a call plays while nobody speaks.
    async fn level(probe: &Arc<Mutex<Vec<f32>>>, over: Duration) -> f32 {
        if let Ok(mut played) = probe.lock() {
            played.clear();
        }
        tokio::time::sleep(over).await;
        let played = probe
            .lock()
            .map(|played| played.clone())
            .unwrap_or_default();
        let energy: f32 = played.iter().map(|sample| sample * sample).sum();
        let samples: f32 = played.iter().map(|_| 1.0).sum::<f32>().max(1.0);
        (energy / samples).sqrt()
    }

    /// Wait until this side of the call does, or does not, hear the tone.
    #[allow(
        clippy::future_not_send,
        reason = "a session's client is not Send, and the test runtime does not need it to be"
    )]
    async fn wait_heard(what: &str, probe: &Arc<Mutex<Vec<f32>>>, heard: bool) {
        let deadline = Instant::now().checked_add(DEADLINE).expect("a deadline");
        loop {
            let playing = level(probe, Duration::from_millis(200)).await > HEARD;
            if playing == heard {
                return;
            }
            assert!(Instant::now() < deadline, "{what}");
        }
    }

    /// A session that carries its calls: the endpoint it binds, the channel
    /// keys, and the room's admitted devices.
    struct Carrier {
        calls: Arc<Calls>,
        factory: Factory,
        probe: Arc<Mutex<Vec<f32>>>,
    }

    /// Wait until `client`'s endpoint has bound: a call runs on it.
    #[allow(
        clippy::future_not_send,
        reason = "a session's client is not Send, and the test runtime does not need it to be"
    )]
    async fn bound(client: &Client) {
        let deadline = Instant::now().checked_add(DEADLINE).expect("a deadline");
        while client.endpoint().is_none() {
            assert!(Instant::now() < deadline, "the session's endpoint binds");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    fn carrier(client: &Client, keys: &ChannelKeyProvider) -> Carrier {
        let calls = Calls::new();
        calls.follow(client, None);
        let probe = Arc::new(Mutex::new(Vec::new()));
        let devices = vec![*public(0x11).as_bytes(), *public(0x22).as_bytes()];
        let factory = Factory::new(Sources {
            calls: Arc::clone(&calls),
            endpoint: Some(client.endpoint_handle()),
            lane: Some(client.lane().clone()),
            keys: Some(keys.clone()),
            roster: Some(Arc::new(move |_room: &str| devices.clone())),
            questions: None,
            audio: Audio::Software,
            played: Some(Arc::clone(&probe)),
        });
        Carrier {
            calls,
            factory,
            probe,
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn two_devices_in_one_call_hear_each_other_until_a_mute_a_deafen_or_a_leave() {
        let person = person_key();
        let laptop = device(&person, 0x11, "Laptop");
        let desk = device(&person, 0x22, "Desk");
        laptop.connect(desk.endpoint_addr());
        desk.connect(laptop.endpoint_addr());
        laptop.follow_desktop(&desk_tree());
        desk.follow_desktop(&desk_tree());
        // Each session hears the other say it is in the channel's call.
        for (here, there) in [(&laptop, 0x22_u8), (&desk, 0x11_u8)] {
            here.hear(hello(there, "the other device"));
            here.hear(presence(there, collab_types::PeerPresencePayload::Arrived));
            here.hear(availability(there, Some(call_presence())));
        }
        bound(&laptop).await;
        bound(&desk).await;
        let (keys, _kept) = keys(0x11, 0x11, &[0x11, 0x22]);
        let left = carrier(&laptop, &keys);
        let right = carrier(&desk, &keys);
        assert_eq!(
            left.calls.callers(CHANNEL),
            vec![*public(0x22).as_bytes()],
            "the call opens a lane to the other device in the channel"
        );
        let mut here = left
            .factory
            .prepare(target())
            .await
            .expect("a call the room's keys and transport carry");
        let mut there = right
            .factory
            .prepare(target())
            .await
            .expect("the other device's side of it");
        assert_eq!(
            here.tick(Instant::now()),
            Ok(State::Live),
            "a call with the channel's keys is live"
        );
        here.start_capture("tone").expect("the test's tone");
        there.start_capture("tone").expect("the test's tone");
        wait_heard("each device hears the other's tone", &left.probe, true).await;
        wait_heard("the other device hears this one's tone", &right.probe, true).await;

        // Muted, this device is heard by no one, while it still hears.
        here.stop_capture();
        wait_heard("a muted device is not heard", &right.probe, false).await;
        wait_heard("a muted device still hears", &left.probe, true).await;

        // Deafened, this device hears nothing, while the call plays on.
        here.start_capture("tone").expect("the test's tone");
        wait_heard("unmuting is heard again", &right.probe, true).await;
        here.set_deafened(true);
        wait_heard("a deafened device hears nothing", &left.probe, false).await;
        wait_heard("the call still plays for everyone else", &right.probe, true).await;
        here.set_deafened(false);
        wait_heard("undeafening hears it again", &left.probe, true).await;

        // Leaving ends the lane: the other device hears nothing of it.
        here.close();
        assert_eq!(
            here.tick(Instant::now()),
            Err(Issue::SessionEnded),
            "a left call is over"
        );
        wait_heard("a device that left is heard by no one", &right.probe, false).await;
        there.close();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_call_joins_while_the_channels_keys_are_still_on_their_way() {
        let person = person_key();
        // This desk's device is not the one its room is governed by, and its
        // channel's keys have not reached it yet.
        let laptop = device(&person, 0x22, "Laptop");
        let desk = device(&person, 0x11, "Desk");
        laptop.connect(desk.endpoint_addr());
        desk.connect(laptop.endpoint_addr());
        laptop.follow_desktop(&desk_tree());
        desk.follow_desktop(&desk_tree());
        laptop.hear(hello(0x11, "the other device"));
        laptop.hear(presence(0x11, collab_types::PeerPresencePayload::Arrived));
        laptop.hear(availability(0x11, Some(call_presence())));
        bound(&laptop).await;
        let (keys, _kept) = keys(0x22, 0x11, &[0x11, 0x22]);
        let left = carrier(&laptop, &keys);
        let mut here = left
            .factory
            .prepare(target())
            .await
            .expect("a call whose channel's keys are on their way");
        assert_eq!(
            here.tick(Instant::now()),
            Ok(State::Joining),
            "a call waits for the channel's keys instead of ending"
        );
        here.close();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[allow(
        clippy::significant_drop_tightening,
        reason = "the session lives until the call is over, and ends in a blocking context"
    )]
    async fn a_session_that_cannot_carry_a_call_ends_it_and_says_why() {
        let _settled = ui::motion::testing::settled();
        let root = private_home();
        let socket = root.path().join("collab.sock");
        let sync = std::rc::Rc::new(Client::untethered(
            public(0xf0),
            OwnPresence {
                label: "Quinten".to_owned(),
                user: Some(person(0xf0)),
                ..OwnPresence::default()
            },
        ));
        let mut session = session(&sync, &socket);
        step(&mut session);
        let media = session
            .media_service()
            .expect("the desk hands its apps a media service")
            .clone();

        // Chat joins a voice channel through that service, as its people's
        // call action does. This session carries no transport, so the call
        // cannot be carried: it says so rather than pretending to be live.
        let chat = tilo_native_media::Caller::connected(
            7,
            APP,
            "the-workspace",
            CHANNEL,
            Arc::new(|| Ok(())),
        );
        let joined = media
            .command_for(
                chat.clone(),
                Request {
                    version: 1,
                    intent: 1,
                    command: Command::Join {
                        channel_id: CHANNEL.to_owned(),
                    },
                },
            )
            .expect("an app joins a voice channel");
        assert_eq!(
            joined.state,
            State::Joining,
            "a joined call waits for its carrier"
        );
        step(&mut session);
        assert!(
            sync.own().call.is_none(),
            "a session with no transport says no call in this person's presence"
        );
        let ended = media
            .command_for(
                chat,
                Request {
                    version: 1,
                    intent: 0,
                    command: Command::Status,
                },
            )
            .expect("the app reads its call");
        assert_eq!(ended.state, State::Ended);
        assert_eq!(ended.issue, Some(Issue::Unavailable));

        // Another person's device still reads this person in a call when
        // this person's presence says so: the presence is the whole of it.
        let peer = Client::untethered(public(0xb0), OwnPresence::default());
        peer.follow_desktop(&desk_tree());
        peer.hear(hello(0xf0, "Quinten"));
        peer.hear(presence(0xf0, collab_types::PeerPresencePayload::Arrived));
        peer.hear(availability(0xf0, Some(call_presence())));
        let calls = peer.calls();
        let heard = calls
            .iter()
            .find(|call| call.channel == CHANNEL)
            .expect("the peer reads the call this person is in");
        assert_eq!(heard.room, ROOM);
        assert_eq!(
            heard
                .people
                .iter()
                .map(|caller| (caller.user.clone(), caller.label.clone()))
                .collect::<Vec<_>>(),
            [(Some(person(0xf0)), "Quinten".to_owned())],
            "the person in the call is this session's person"
        );
        // The session keeps a runtime of its own, which blocking drop ends.
        tokio::task::block_in_place(|| drop(session));
    }
}
