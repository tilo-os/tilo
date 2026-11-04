//! Voice calls on the collaboration plane: the channel this device's call is
//! in, and who is in the channels every workspace this session takes part in
//! shows.
//!
//! Being in a call is the presence every member of the workspace reads:
//! joining
//! publishes the channel, leaving stops saying it, and a channel's roster is
//! what the members in it say of themselves. The session publishes this from
//! its pass ([`Calls::follow`]), and the calls themselves through the service
//! an app reaches ([`tilo_native_media`]), whose carrier is this plane
//! ([`Factory`]).
//!
//! A channel id is the whole of a call's identity on the wire: the presence
//! names the channel and what it is called, and a roster says who is in a
//! channel, with no workspace beside it.
//!
//! What a call carries is the workspace's live plane ([`tilo_live::plane`]):
//! its voice is one more sealed track of the session's plane in that
//! workspace, which every device in the workspace already carries, and the
//! people the workspace says are in the channel are the ones this device
//! plays. Pictures keep their own lane of [`tilo_call_lane`] on this
//! session's own endpoint.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use media::realtime::InputFormat;
use media::realtime::capture::{CaptureConfig, VideoCapture};
use runtime_collab::native_media::{
    Input, Issue, Picture, PictureLayer, PictureSource, Roster, Shown, State,
};
use tilo_call_lane::video as pictures;
use tilo_desktop_model::WorkspaceId;
use tilo_live::live::Track;
use tilo_live::plane::{Heard, Plane};
use tilo_live::voice::{self, VoiceObject};
use tilo_lock::MutexRecover as _;
use tilo_native_media::{CallTarget, MediaFactory, MediaSession, Prepare, Rosters, ScreenShare};

use crate::call_devices::{Audio, Source, Voice};
use crate::sync::{self, LiveCall};

/// How often a live call moves its audio: one frame's worth, so a captured
/// frame is sealed and sent as soon as the microphone has it.
const PUMP: Duration = Duration::from_millis(10);
/// How long a device waits before trying a lane to a peer of the call again.
const REDIAL: Duration = Duration::from_millis(500);
/// Voice objects one workspace's plane may have heard before a call plays
/// them: a call that starts late plays the newest of them, never a backlog.
const HEARD: usize = 256;
/// What the person's answer covers when an app turns a camera on, by the
/// name every app's device capabilities use.
const CAMERA: &str = "camera";
/// What the person's answer covers when an app shares a screen.
const SCREEN: &str = "screen";
/// The largest desktop a shared screen is captured at: the screen's largest
/// layer, so nothing is captured that no tile could show.
const SCREEN_MAX: (u32, u32) = {
    let (width, height) = pictures::Layer::High.size(pictures::Source::Screen);
    (width as u32, height as u32)
};
/// Bytes of received pictures one answer carries at most: several tiles of
/// every layer, so a burst of them is drawn in one pass.
const PICTURES_BUDGET: usize = 1024 * 1024;
/// What the person's answer covers: the microphone, by the name every app's
/// device capabilities use.
const MICROPHONE: &str = "microphone";

/// The calls of every workspace this session takes part in.
pub(crate) struct Calls {
    /// The calls here, as the people in them say.
    live: Mutex<Vec<LiveCall>>,
    /// Who is in each channel, as the members say: what a voice channel
    /// shows.
    rosters: Mutex<Vec<Roster>>,
    /// What the rosters last said, to wake the apps watching them only when
    /// they moved.
    said: Mutex<String>,
    /// Woken whenever a workspace's calls may have changed.
    changed: Arc<tokio::sync::Notify>,
    /// This session's live plane in each workspace, as it stands: what a
    /// call's voice is published into and heard from.
    planes: Mutex<BTreeMap<String, Arc<Plane>>>,
    /// What each workspace's plane heard since the last call, newest last:
    /// the calls of that workspace play it.
    heard: Mutex<BTreeMap<String, VecDeque<Heard>>>,
    /// The next voice object each workspace publishes. One counter for the
    /// session, so a stream keeps its place across a call that ends and comes
    /// back, and across the microphone being muted.
    audio: Mutex<BTreeMap<String, u32>>,
}

impl Calls {
    #[must_use]
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self {
            live: Mutex::new(Vec::new()),
            rosters: Mutex::new(Vec::new()),
            said: Mutex::new(String::new()),
            changed: Arc::new(tokio::sync::Notify::new()),
            planes: Mutex::new(BTreeMap::new()),
            heard: Mutex::new(BTreeMap::new()),
            audio: Mutex::new(BTreeMap::new()),
        })
    }

    /// Take in the calls everyone here says they are in, and the channel
    /// this device joined (if any), so a voice channel shows a join or a
    /// leave on this pass.
    pub(crate) fn follow(&self, sync: &sync::Client, own: Option<&sync::InCall>) {
        let live = sync.calls();
        let mut people: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        let mut cameras: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        let mut screens: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        for call in &live {
            let in_call = people.entry(call.channel.clone()).or_default();
            for caller in &call.people {
                let Some(person) = &caller.user else {
                    continue;
                };
                in_call.insert(person.clone());
                if caller.camera {
                    cameras
                        .entry(call.channel.clone())
                        .or_default()
                        .insert(person.clone());
                }
                if caller.screen {
                    screens
                        .entry(call.channel.clone())
                        .or_default()
                        .insert(person.clone());
                }
            }
        }
        // This device's own call: nobody here hears it from this session, so
        // it is said from what the session itself published.
        if let (Some(call), Some(person)) = (own, sync.own().user) {
            let channel = &call.call.channel;
            people
                .entry(channel.clone())
                .or_default()
                .insert(person.clone());
            if call.camera {
                cameras
                    .entry(channel.clone())
                    .or_default()
                    .insert(person.clone());
            }
            if call.screen {
                screens.entry(channel.clone()).or_default().insert(person);
            }
        }
        let rosters: Vec<Roster> = people
            .into_iter()
            .map(|(channel_id, people)| Roster {
                cameras: cameras
                    .remove(&channel_id)
                    .map(|set| set.into_iter().collect())
                    .unwrap_or_default(),
                screens: screens
                    .remove(&channel_id)
                    .map(|set| set.into_iter().collect())
                    .unwrap_or_default(),
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
        // The planes this session is in now: a call's voice runs on the one
        // its workspace is in, and is no longer carried once this device
        // leaves the workspace.
        let planes: BTreeMap<String, Arc<Plane>> = sync
            .live()
            .map(|live| {
                live.planes()
                    .into_iter()
                    .map(|(workspace, plane)| (workspace.to_string(), plane))
                    .collect()
            })
            .unwrap_or_default();
        {
            let mut held = self.planes.lock_recover();
            if held.len() != planes.len() || held.keys().ne(planes.keys()) {
                *held = planes;
            }
        }
        // What the planes heard since the last pass, each workspace's own:
        // the calls playing it take it.
        for (workspace, heard) in sync.take_call_audio() {
            self.hear(&workspace, heard);
        }
        if moved {
            self.changed.notify_waiters();
        }
    }

    /// The plane `workspace`'s call's voice runs on, while this session is in
    /// the workspace's plane.
    pub(crate) fn plane(&self, workspace: &str) -> Option<Arc<Plane>> {
        self.planes.lock_recover().get(workspace).cloned()
    }

    /// Put one object `workspace`'s plane heard where the calls playing that
    /// workspace take it.
    fn hear(&self, workspace: &str, heard: Heard) {
        let mut buckets = self.heard.lock_recover();
        let bucket = buckets.entry(workspace.to_owned()).or_default();
        bucket.push_back(heard);
        while bucket.len() > HEARD {
            bucket.pop_front();
        }
        drop(buckets);
    }

    /// What `workspace`'s plane heard since the last call, oldest first.
    fn took(&self, workspace: &str) -> Vec<Heard> {
        self.heard
            .lock_recover()
            .get_mut(workspace)
            .map(|bucket| bucket.drain(..).collect())
            .unwrap_or_default()
    }

    /// The sequence of the next voice object this device publishes in
    /// `workspace`.
    fn next_audio(&self, workspace: &str) -> u32 {
        let mut audio = self.audio.lock_recover();
        let next = audio.entry(workspace.to_owned()).or_insert(0);
        let sequence = *next;
        *next = next.wrapping_add(1);
        drop(audio);
        sequence
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

    /// The devices the workspace says are in `channel`'s call, this one among
    /// them: who a call opens a lane to.
    fn callers(&self, channel: &str) -> Vec<[u8; 32]> {
        let live = self.live.lock_recover();
        live.iter()
            .filter(|call| call.channel == channel)
            .flat_map(|call| &call.people)
            // ignored-failure: a caller id that is not an endpoint names no device to open a lane to
            .filter_map(|caller| caller.id.parse::<iroh::EndpointId>().ok())
            .map(|device| *device.as_bytes())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }
}

impl Calls {
    /// The person each device in `channel`'s call is, as the callers said:
    /// what the pictures a device sends are named by. A device whose caller
    /// names no person is left out, its pictures unnamed.
    fn persons(&self, channel: &str) -> BTreeMap<[u8; 32], String> {
        let live = self.live.lock_recover();
        live.iter()
            .filter(|call| call.channel == channel)
            .flat_map(|call| &call.people)
            .filter_map(|caller| {
                let device = caller.id.parse::<iroh::EndpointId>().ok()?;
                Some((*device.as_bytes(), caller.user.clone()?))
            })
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

/// The devices a workspace's governance admits, read fresh: what a call's
/// roster is made of, and who this device accepts frames from.
pub(crate) type Admitted = Arc<dyn Fn(&str) -> Vec<[u8; 32]> + Send + Sync>;

/// Where a call's carrier reads what only the session knows: its endpoint,
/// the channel service, the workspace's governance, the person's answers,
/// and what the audio runs on.
pub(crate) struct Sources {
    /// The calls of the workspaces this session takes part in, as the loop
    /// heard them: who else is in the channel this device joins.
    pub(crate) calls: Arc<Calls>,
    /// The session's own endpoint, where the call's lanes run. `None` for a
    /// session taking part in no transport, which carries no call.
    pub(crate) endpoint: Option<Arc<std::sync::OnceLock<iroh::Endpoint>>>,
    /// What answers that endpoint's call lanes.
    pub(crate) lane: Option<LaneSlot>,
    /// This device's channel keys: the ones a call in the channel seals its
    /// audio with.
    pub(crate) keys: Option<tilo_action_broker::channel_encryption::ChannelKeyProvider>,
    /// The devices `workspace` admits, which a call's roster names.
    pub(crate) roster: Option<Admitted>,
    /// The person's prompts: the microphone is asked for through them.
    pub(crate) questions: Option<tilo_service::Questions>,
    /// What the call hears and is heard by.
    pub(crate) audio: Audio,
    /// This person, as the workspace names them: whose tile their own camera
    /// feeds.
    pub(crate) person: Option<String>,
    /// What this device's camera captures from.
    pub(crate) camera: tilo_call_lane::Camera,
    /// Where a test reads the samples the call played, in place of a
    /// speaker.
    #[cfg(test)]
    pub(crate) played: Option<Arc<Mutex<Vec<f32>>>>,
}

/// This plane as the carrier of a call: being in the channel is joining it,
/// which the session's presence then says to everyone in the workspace, and
/// a lane on the session's own endpoint carries its audio.
pub(crate) struct Factory {
    sources: Arc<Sources>,
    /// The person's answers about the microphone: the prompt the session's
    /// apps get, so one allowance covers every call of the session.
    consents: Option<tilo_portal::consent::Consents>,
}

impl Sources {
    /// What a session's own calls read: its endpoint, the channel service,
    /// the workspace's governance, and the person's prompts.
    #[must_use]
    pub(crate) fn carrier(
        calls: Arc<Calls>,
        endpoint: Arc<std::sync::OnceLock<iroh::Endpoint>>,
        lane: LaneSlot,
        keys: Option<tilo_action_broker::channel_encryption::ChannelKeyProvider>,
        roster: Admitted,
        questions: Option<tilo_service::Questions>,
        person: Option<String>,
    ) -> Self {
        Self {
            calls,
            endpoint: Some(endpoint),
            lane: Some(lane),
            keys,
            roster: Some(roster),
            questions,
            audio: Audio::Devices,
            person,
            camera: tilo_call_lane::Camera::from_env(),
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
    let workspace_id = WorkspaceId::parse(&target.workspace).ok_or(Issue::Unavailable)?;
    let channel = collab_types::document_channel_id(&target.app, &target.workspace, &target.record);
    // The call is one channel of one workspace: that scope's keys seal its
    // tracks, and its id names them.
    let scope = collab_types::scope::Scope::channel(target.workspace.clone(), channel);
    let (access, keys) = call_keys(sources, workspace_id, channel)?;
    let roster = admitted(sources, &target.workspace);
    let own = *endpoint.id().as_bytes();
    // The call's pictures run on their own lane, sealed with the channel's
    // keys: a camera or a screen never waits behind a microphone's datagrams.
    let (video_calls, video_accepted) =
        lane_calls(&target, channel, tilo_call_lane::Lane::Video, &roster);
    let video = tilo_call_lane::Video::new(own);
    if let Some(keys) = &keys {
        video.set_keys(keys, roster.clone(), &scope);
    }
    let (voice, microphones, offered) = opened_microphones(sources.audio).await?;
    let live = keys.is_some();
    let (lost, lost_lanes) = tokio::sync::mpsc::unbounded_channel();
    let current = keys.map(|keys| Current { keys, roster });
    let shared = Arc::new(Shared {
        lane,
        endpoint,
        video,
        video_calls,
        screen: Arc::clone(screen_share()),
        showing: Mutex::new(Vec::new()),
        streams: Mutex::new(BTreeMap::new()),
        person: sources.person.clone(),
        presence: Arc::clone(&sources.calls),
        camera: sources.camera,
        own,
        access,
        workspace_id,
        channel,
        scope,
        workspace: target.workspace.clone(),
        admitted: sources.roster.clone(),
        current: Mutex::new(current),
        side: Mutex::new(Side {
            voice,
            camera: None,
            screen: None,
        }),
        lanes: Mutex::new(BTreeMap::new()),
        next_lane: AtomicU64::new(0),
        lost,
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
    // Both devices know the other is in the call: of the two, one dials and
    // the other answers, so a pair holds one lane rather than two.
    let dialled = sources
        .calls
        .callers(&target.channel)
        .into_iter()
        .filter(|device| shared.dials(*device))
        .collect();
    tokio::spawn(run(
        Arc::clone(&shared),
        dialled,
        lost_lanes,
        video_accepted,
    ));
    Ok(Box::new(Call {
        shared,
        inputs: offered,
    }))
}

/// This call, as the lane `lane` of it announces itself.
fn hello(
    target: &CallTarget,
    channel: collab_types::ChannelId,
    lane: tilo_call_lane::Lane,
) -> tilo_call_lane::Hello {
    tilo_call_lane::Hello {
        workspace: target.workspace.clone(),
        channel,
        call: target.channel.clone(),
        lane,
    }
}

/// One lane of this call: the [`tilo_call_lane::Calls`] that carries it, and
/// the lanes other devices open on it.
fn lane_calls(
    target: &CallTarget,
    channel: collab_types::ChannelId,
    lane: tilo_call_lane::Lane,
    roster: &tilo_call_lane::Roster,
) -> (
    tilo_call_lane::Calls,
    tokio::sync::mpsc::UnboundedReceiver<tilo_call_lane::Peer>,
) {
    tilo_call_lane::Calls::new(hello(target, channel, lane), roster.clone())
}

/// The keys the call's channel holds, or why the call cannot be carried: keys
/// that are on their way still carry it, since it opens joining the channel
/// and the worker takes them when they arrive. Only a device the channel has
/// no keys for at all leaves it uncarried.
fn call_keys(
    sources: &Sources,
    workspace_id: WorkspaceId,
    channel: collab_types::ChannelId,
) -> Result<
    (
        tilo_action_broker::channel_encryption::ChannelKeyAccess,
        Option<channel_crypto::ChannelContentKeys>,
    ),
    Issue,
> {
    let access = sources
        .keys
        .as_ref()
        .and_then(tilo_action_broker::channel_encryption::ChannelKeyProvider::get)
        .ok_or(Issue::Unavailable)?;
    let keys = match access.access(workspace_id, channel).0 {
        tilo_action_broker::channel_encryption::ChannelAccess::Ready(keys) => Some(keys),
        tilo_action_broker::channel_encryption::ChannelAccess::Waiting => None,
        _ => return Err(Issue::Unavailable),
    };
    Ok((access, keys))
}

/// This device's microphones, opened for the call, and what the app is
/// offered of them.
async fn opened_microphones(
    audio: crate::call_devices::Audio,
) -> Result<(Voice, Vec<Source>, Vec<Input>), Issue> {
    let voice = tokio::task::spawn_blocking(move || Voice::open(audio))
        .await
        .map_err(|_| Issue::Unavailable)??;
    let microphones = voice.sources();
    let offered = microphones
        .iter()
        .map(|source| Input {
            id: source.id.clone(),
            label: source.label.clone(),
        })
        .collect();
    Ok((voice, microphones, offered))
}

/// The devices `workspace` admits: who may send to this device in
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
        // A call the person left is over: whatever its audio said in the same
        // moment — a stream that ended with the call, say — is not why it did.
        if self.shared.closed.load(Ordering::Acquire) {
            return Err(Issue::SessionEnded);
        }
        let issue = *self.shared.issue.lock_recover();
        if let Some(issue) = issue {
            return Err(issue);
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

    fn show(&mut self, shown: Vec<Shown>) {
        self.shared.show(shown);
    }

    fn set_camera(&mut self, on: bool) -> Result<(), Issue> {
        self.shared.set_camera(on)
    }

    fn set_screen(&mut self, on: bool) -> Result<(), Issue> {
        self.shared.set_screen(on)
    }

    fn pictures(&mut self, after: u64) -> (Vec<Picture>, u64) {
        self.shared.pictures(after)
    }

    fn pictures_changed(&self) -> Option<Arc<tokio::sync::Notify>> {
        Some(self.shared.video.signal())
    }

    fn close(&mut self) {
        self.shared.close();
    }
}

/// The channel's keys and who may read them, and what the workspace's
/// governance admitted at the same time: what seals this call's frames.
struct Current {
    keys: channel_crypto::ChannelContentKeys,
    roster: tilo_call_lane::Roster,
}

/// This call's own side: what it plays, the microphone it seals frames with
/// while the person is heard, and the pictures it sends while they are
/// shown.
struct Side {
    voice: Voice,
    /// This device's camera, while the person has it on in the call.
    camera: Option<CameraSend>,
    /// The screen share's encoder, while the person shares their screen.
    screen: Option<Arc<tilo_call_lane::Sending>>,
}

/// This device's camera: the capture feeding the call's encoder thread, and
/// that thread, which ends when the last handle to it goes.
struct CameraSend {
    /// Dropping the last handle ends the encoder thread and what it sends.
    _sending: Arc<tilo_call_lane::Sending>,
    /// Dropping the capture stops the camera: no frame is published after.
    _capture: VideoCapture,
}

struct Shared {
    lane: LaneSlot,
    endpoint: iroh::Endpoint,
    /// This device's key: whose pictures the app's are.
    own: [u8; 32],
    /// Where the channel's keys come from when its membership moves.
    access: tilo_action_broker::channel_encryption::ChannelKeyAccess,
    /// The workspace id the channel's keys and governance are read by.
    workspace_id: WorkspaceId,
    channel: collab_types::ChannelId,
    /// The channel's scope: what its tracks are sealed under and named as.
    scope: collab_types::scope::Scope,
    workspace: String,
    /// The workspace's admitted devices, re-read while the call lasts.
    admitted: Option<Admitted>,
    /// The channel's keys and roster, once this device holds them: the call
    /// is up from then, and joining until.
    current: Mutex<Option<Current>>,
    side: Mutex<Side>,
    /// Which of this device's lanes is the call's lane to each device, by
    /// lane: a lane that ends is only a lane lost while it is still the one
    /// the call holds.
    lanes: Mutex<BTreeMap<(tilo_call_lane::Lane, [u8; 32]), u64>>,
    /// The generation of the next lane this device opens or takes.
    next_lane: AtomicU64,
    /// The lanes that ended, for the call to dial the device again.
    lost: tokio::sync::mpsc::UnboundedSender<(tilo_call_lane::Lane, [u8; 32])>,
    /// The microphones this call may send.
    sources: Vec<Source>,
    /// This call's pictures: what this device sends, and what arrived here
    /// for the app to draw.
    video: Arc<tilo_call_lane::Video>,
    /// The lane this device opens to each device of the call for pictures.
    video_calls: tilo_call_lane::Calls,
    /// The desktop this session shares, which this call encodes while the
    /// person shares it.
    screen: Arc<ScreenShare>,
    /// What the app shows of the people in this call, as it last said.
    showing: Mutex<Vec<Shown>>,
    /// This person, as the workspace names them: whose tile their own camera
    /// feeds, and what their own pictures are labelled.
    person: Option<String>,
    /// The calls this session takes part in, as presence said
    /// them: who is in this call, and which person each device is.
    presence: Arc<Calls>,
    /// What this device's camera captures from.
    camera: tilo_call_lane::Camera,
    /// The timestamp each device's voice stream was last heard at, in this
    /// call: a stream that starts over is recognised by its own bytes.
    streams: Mutex<BTreeMap<[u8; 32], u64>>,
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
                if let Err(error) = consents.decide(&caller, &handle, question).await {
                    tracing::warn!(
                        error = ?error,
                        "a microphone permission question was not put to the person"
                    );
                }
            });
            return Err(Issue::PermissionDenied);
        }
        if !self.live.load(Ordering::Acquire) {
            // The channel's keys are not here yet: the call is still joining.
            return Err(Issue::Unavailable);
        }
        let still_current = {
            let call = Arc::downgrade(self);
            Arc::new(move || call.upgrade().is_some_and(|call| !call.closed()))
        };
        self.side
            .lock_recover()
            .voice
            .start(&source, still_current)?;
        Ok(())
    }

    fn stop_capture(&self) {
        let mut side = self.side.lock_recover();
        side.voice.stop();
    }

    fn set_deafened(&self, deafened: bool) {
        self.deafened.store(deafened, Ordering::Release);
        self.side.lock_recover().voice.set_deafened(deafened);
    }

    /// Show these people's pictures here, each at the layer named: every
    /// device of theirs in the call is told, and encodes only that.
    fn show(&self, shown: Vec<Shown>) {
        // This person's own camera tile is fed here, at the largest layer
        // the app shows of them.
        let own = self.person.as_ref().and_then(|own| {
            shown
                .iter()
                .filter(|picture| picture.source == PictureSource::Camera && &picture.person == own)
                .map(|picture| layer_of(picture.layer))
                .max()
        });
        self.video.own_tile(own);
        *self.showing.lock_recover() = shown;
        self.send_showing(&self.presence.persons(&self.call));
    }

    /// Tell every device of the call what this device shows of it: nothing
    /// of a device whose person the app no longer shows.
    fn send_showing(&self, persons: &BTreeMap<[u8; 32], String>) {
        let showing = self.showing.lock_recover().clone();
        let mut devices: BTreeSet<[u8; 32]> = persons.keys().copied().collect();
        devices.extend(self.video.devices());
        for device in devices {
            let (camera, screen) = layers_shown(&showing, persons.get(&device));
            self.video.show(device, &camera, &screen);
        }
    }

    /// Turn this person's camera on in the call, or off: the capture feeds
    /// the lane's encoder, which sends only the layers someone shows.
    fn set_camera(self: &Arc<Self>, on: bool) -> Result<(), Issue> {
        if self.closed() {
            return Err(Issue::CameraLost);
        }
        if !on {
            self.side.lock_recover().camera = None;
            self.video.stop(pictures::Source::Camera);
            return Ok(());
        }
        if !self.ask(CAMERA, "call-camera", "to be seen in the call") {
            return Err(Issue::PermissionDenied);
        }
        let device = self.camera.device().ok_or(Issue::CameraLost)?;
        let sending = self
            .video
            .start(pictures::Source::Camera, tilo_call_lane::CAMERA_FPS)
            .map_err(|_| Issue::CameraLost)?;
        let feed = Arc::downgrade(&sending);
        let (width, height) = tilo_call_lane::CAMERA_SIZE;
        let capture = VideoCapture::start(
            &CaptureConfig {
                device,
                width,
                height,
                fps: tilo_call_lane::CAMERA_FPS,
            },
            Box::new(move |frame| {
                let Some(sending) = feed.upgrade() else {
                    return;
                };
                sending.publish(tilo_call_lane::Frame {
                    pixels: Arc::from(frame.nv12),
                    width: frame.width,
                    height: frame.height,
                    format: InputFormat::Nv12,
                    captured_at: frame.captured_at,
                });
            }),
        )
        .map_err(|_| {
            self.video.stop(pictures::Source::Camera);
            Issue::CameraLost
        })?;
        self.side.lock_recover().camera = Some(CameraSend {
            _sending: sending,
            _capture: capture,
        });
        Ok(())
    }

    /// Share this person's screen in the call, or stop: the session's own
    /// pass hands the share its frames, and the lane encodes them.
    fn set_screen(self: &Arc<Self>, on: bool) -> Result<(), Issue> {
        if self.closed() {
            return Err(Issue::ScreenLost);
        }
        if !on {
            self.side.lock_recover().screen = None;
            self.screen.set_sharing(false);
            self.video.stop(pictures::Source::Screen);
            return Ok(());
        }
        if !self.ask(SCREEN, "call-screen", "to share this screen in the call") {
            return Err(Issue::PermissionDenied);
        }
        let sending = self
            .video
            .start(pictures::Source::Screen, tilo_call_lane::SCREEN_FPS)
            .map_err(|_| Issue::ScreenLost)?;
        self.screen.set_sharing(true);
        self.side.lock_recover().screen = Some(sending);
        Ok(())
    }

    /// Ask the person through the session's own prompt for `capability`:
    /// whether they already allowed it for this call. A question that was
    /// not answered yet is not asked again; the service refuses the second.
    fn ask(&self, capability: &str, handle: &str, reason: &str) -> bool {
        let Some(consents) = &self.consents else {
            return true;
        };
        if consents.allowed(&self.caller, capability) {
            return true;
        }
        let consents = consents.clone();
        let caller = self.caller.clone();
        let handle = format!("{handle}:{}", self.call);
        let question = tilo_portal::consent::AccessQuestion {
            capability: capability.to_owned(),
            reason: Some(reason.to_owned()),
            may_remember: false,
            text: None,
        };
        tokio::spawn(async move {
            let _ = consents.decide(&caller, &handle, question).await;
        });
        false
    }

    /// The pictures that arrived here for the app, oldest first, named by
    /// the person whose device sent them.
    fn pictures(&self, after: u64) -> (Vec<Picture>, u64) {
        let (taken, cursor) = self.video.take(after, PICTURES_BUDGET);
        let persons = self.presence.persons(&self.call);
        let mut pictures = Vec::with_capacity(taken.len());
        for picture in taken {
            let person = if picture.from == self.own {
                self.person.clone()
            } else {
                persons.get(&picture.from).cloned()
            };
            let Some(person) = person else {
                // A device the callers do not name a person for: the app has
                // nobody to draw its picture under.
                continue;
            };
            pictures.push(Picture {
                person,
                stream: hex(&picture.from),
                source: source_of(picture.source),
                object: picture.object,
            });
        }
        (pictures, cursor)
    }

    /// Move the pictures once: the desktop the pass just drew goes to the
    /// encoder, and a locked screen's last picture is forgotten rather than
    /// sent again.
    fn pump_video(&self) {
        let Some(sending) = self.side.lock_recover().screen.clone() else {
            return;
        };
        if self.screen.withheld() {
            sending.clear();
            return;
        }
        let Some((width, height, rgba)) = self.screen.take() else {
            return;
        };
        if let Some(frame) = tilo_call_lane::even_rgba(width, height, Arc::from(rgba)) {
            sending.publish(frame);
        }
    }

    fn close(&self) {
        if self.closed.swap(true, Ordering::AcqRel) {
            return;
        }
        self.lane.clear_if(&self.video_calls);
        let mut side = self.side.lock_recover();
        side.camera = None;
        side.screen = None;
        side.voice.close();
        drop(side);
        self.screen.set_sharing(false);
        self.video.stop(pictures::Source::Camera);
        self.video.stop(pictures::Source::Screen);
        self.video.close();
    }

    fn closed(&self) -> bool {
        self.closed.load(Ordering::Acquire)
    }

    /// Take `peer`'s picture lane: from now on this device sends it what the
    /// app shows of it, and reads the pictures it sends.
    fn join_video(self: &Arc<Self>, peer: tilo_call_lane::Peer) {
        if self.closed() {
            peer.close();
            return;
        }
        let device = peer.remote();
        let generation = self.lane_up(tilo_call_lane::Lane::Video, device);
        self.video.add(peer.clone());
        let losing = Arc::downgrade(self);
        tokio::spawn(async move {
            peer.ended().await;
            if let Some(shared) = losing.upgrade() {
                shared.lane_ended(tilo_call_lane::Lane::Video, device, generation);
            }
        });
        // A device that joined after the app said what it shows hears it
        // now, as a lane that joins later does.
        let persons = self.presence.persons(&self.call);
        let showing = self.showing.lock_recover().clone();
        let (camera, screen) = layers_shown(&showing, persons.get(&device));
        self.video.show(device, &camera, &screen);
    }

    /// Make `lane` to `device` the call's lane from now on, and say which
    /// generation it is: a lane whose end is reported is only lost while it
    /// is still the one the call holds.
    fn lane_up(&self, lane: tilo_call_lane::Lane, device: [u8; 32]) -> u64 {
        let generation = self.next_lane.fetch_add(1, Ordering::Relaxed);
        self.lanes.lock_recover().insert((lane, device), generation);
        generation
    }

    /// A lane to `device` ended: what it carried is forgotten, and the call
    /// dials the device again, unless another lane replaced it since.
    fn lane_ended(&self, lane: tilo_call_lane::Lane, device: [u8; 32], generation: u64) {
        {
            let mut lanes = self.lanes.lock_recover();
            if lanes.get(&(lane, device)) != Some(&generation) {
                return;
            }
            lanes.remove(&(lane, device));
        }
        let _ = self.lost.send((lane, device));
    }

    /// Whether this device dials the lane to `device`: the device of the two
    /// whose key is smaller does, so a pair holds one lane.
    fn dials(&self, device: [u8; 32]) -> bool {
        self.own < device
    }

    /// Move the call's audio once: publish what the microphone captured on
    /// the workspace's plane, and play what the call's devices published
    /// there.
    ///
    /// The plane carries one voice track per device per workspace, so what it
    /// heard is played only from the devices this call's channel says are in
    /// it. A deafened call drops the frames rather than holding them.
    fn pump(&self) {
        let now = Instant::now();
        let deafened = self.deafened.load(Ordering::Acquire);
        let heard = if deafened {
            Vec::new()
        } else {
            self.presence.took(&self.workspace)
        };
        if !heard.is_empty() {
            let callers: BTreeSet<[u8; 32]> =
                self.presence.callers(&self.call).into_iter().collect();
            let mut side = self.side.lock_recover();
            for heard in heard {
                let sender = heard.sender;
                if !callers.contains(&sender) {
                    continue;
                }
                let Ok(object) = VoiceObject::decode(&heard.body) else {
                    continue;
                };
                // A stream that starts over — the device left the call and
                // came back, or its page reloaded — is a new stream: what was
                // heard from it before says nothing about this frame's place,
                // so the playout starts over with it rather than holding it
                // behind where the last one got.
                let restarted = {
                    let mut streams = self.streams.lock_recover();
                    let restarted = streams
                        .get(&sender)
                        .is_some_and(|last| object.timestamp < *last);
                    streams.insert(sender, object.timestamp);
                    restarted
                };
                if restarted {
                    side.voice.forget(sender);
                }
                if side
                    .voice
                    .receive(
                        sender,
                        object.sequence,
                        object.timestamp,
                        object.payload,
                        now,
                    )
                    .is_err()
                {
                    break;
                }
            }
        }
        let mut side = self.side.lock_recover();
        let captured = match side.voice.pump(now) {
            Ok(captured) => captured,
            Err(issue) => {
                // The person's microphone stopped: capture is off from now
                // on, and the call says why. Its place in the stream stays
                // where it is, so starting capture again is heard at once.
                *self.issue.lock_recover() = Some(issue);
                return;
            }
        };
        let plane = self.presence.plane(&self.workspace);
        if let Some(plane) = plane {
            for frame in captured {
                let sequence = self.presence.next_audio(&self.workspace);
                let Ok(body) = (VoiceObject {
                    sequence: u64::from(sequence),
                    timestamp: frame.timestamp_48k,
                    duration: voice::FRAME_SAMPLES,
                    payload: frame.payload,
                })
                .encode() else {
                    continue;
                };
                if plane.publish(Track::Audio, sequence, &body).is_err() {
                    // The plane ended: what follows is published when this
                    // session's pass joins it again.
                    break;
                }
            }
        }
        if let Err(issue) = side.voice.playout(now) {
            *self.issue.lock_recover() = Some(issue);
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
    }

    /// Re-read what the workspace admits and what the channel's keys are: a
    /// membership change reaches the call as it happens, and a channel whose
    /// keys were still on their way when the call joined brings it up.
    fn refresh(&self) {
        let roster = self
            .admitted
            .as_ref()
            .map(|admitted| tilo_call_lane::Roster::new(admitted(&self.workspace)));
        let (granted, _) = self.access.access(self.workspace_id, self.channel);
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
        {
            let mut held = self.current.lock_recover();
            match (held.as_mut(), keys) {
                (Some(current), Some(keys)) if keys.epoch() != current.keys.epoch() => {
                    current.keys = keys;
                    if let Some(roster) = roster {
                        self.video_calls.set_roster(roster.clone());
                        current.roster = roster;
                    }
                    // The pictures seal with the epoch's keys from this
                    // epoch on.
                    self.video
                        .set_keys(&current.keys, current.roster.clone(), &self.scope);
                }
                (Some(current), _) => {
                    if let Some(roster) =
                        roster.filter(|roster| roster.devices().ne(current.roster.devices()))
                    {
                        self.video_calls.set_roster(roster.clone());
                        current.roster = roster;
                        self.video
                            .set_keys(&current.keys, current.roster.clone(), &self.scope);
                    }
                }
                (None, Some(keys)) => {
                    let roster = roster.unwrap_or_default();
                    self.video_calls.set_roster(roster.clone());
                    self.video.set_keys(&keys, roster.clone(), &self.scope);
                    *held = Some(Current { keys, roster });
                    self.live.store(true, Ordering::Release);
                }
                (None, None) => {}
            }
            drop(held);
        }
    }
}

/// Carry `shared`'s call: answer the endpoint's lanes, open one to every
/// device the workspace says is in the channel, and move its audio until it
/// ends.
async fn run(
    shared: Arc<Shared>,
    dialled: Vec<[u8; 32]>,
    mut lost: tokio::sync::mpsc::UnboundedReceiver<(tilo_call_lane::Lane, [u8; 32])>,
    mut accepted: tokio::sync::mpsc::UnboundedReceiver<tilo_call_lane::Peer>,
) {
    // The endpoint answers the call's picture lane with this call's calls:
    // a lane that was dialled here and one that dialled in are both taken.
    shared.lane.install(shared.video_calls.clone());
    // Whether the call is live is the channel's keys to say, not this task's
    // starting: a call whose keys are still on their way joins the channel and
    // says so until `refresh` brings it up, whether or not the lane is already
    // dialling.
    let (joined, mut arrived) = tokio::sync::mpsc::unbounded_channel();
    // The lanes this call is opening: one dial at a time per lane, so a lane
    // that ended goes back to the one path that dials it without a second
    // lane to the same device.
    let mut opening: BTreeSet<(tilo_call_lane::Lane, [u8; 32])> = BTreeSet::new();
    for device in dialled {
        dial_lane(&shared, &mut opening, device, &joined);
    }
    let mut beat = tokio::time::interval(PUMP);
    beat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut ticks = 0_u32;
    loop {
        if shared.closed() {
            break;
        }
        tokio::select! {
            Some(peer) = accepted.recv() => lane_joined(&shared, &mut opening, peer),
            Some(peer) = arrived.recv() => lane_joined(&shared, &mut opening, peer),
            Some((_, device)) = lost.recv() => dial_lane(&shared, &mut opening, device, &joined),
            _ = beat.tick() => {}
        }
        shared.pump();
        shared.pump_video();
        ticks = ticks.wrapping_add(1);
        if ticks.is_multiple_of(100) {
            shared.refresh();
        }
    }
    shared.close();
}

/// Dial the call's picture lane to `device`, unless this device does not dial
/// the pair or is already dialling it: the one path a lane is opened by,
/// whether the call is starting or a lane that ended is coming back.
fn dial_lane(
    shared: &Arc<Shared>,
    opening: &mut BTreeSet<(tilo_call_lane::Lane, [u8; 32])>,
    device: [u8; 32],
    joined: &tokio::sync::mpsc::UnboundedSender<tilo_call_lane::Peer>,
) {
    let lane = tilo_call_lane::Lane::Video;
    if !shared.dials(device) || !opening.insert((lane, device)) {
        return;
    }
    let calls = shared.video_calls.clone();
    let joined = joined.clone();
    let endpoint = shared.endpoint.clone();
    let call = Arc::downgrade(shared);
    tokio::spawn(async move {
        let Ok(remote) = iroh::EndpointId::from_bytes(&device) else {
            tracing::warn!("a call lane names a device that is not an endpoint; it is not dialled");
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
                // A dial that failed is tried again after REDIAL, while the
                // call is open.
                // ignored-failure: the lane is redialled until the call closes
                Err(_) => tokio::time::sleep(REDIAL).await,
            }
        }
    });
}

/// Take the lane `peer` carries, which this call is no longer dialling.
fn lane_joined(
    shared: &Arc<Shared>,
    opening: &mut BTreeSet<(tilo_call_lane::Lane, [u8; 32])>,
    peer: tilo_call_lane::Peer,
) {
    opening.remove(&(peer.lane(), peer.remote()));
    shared.join_video(peer);
}

/// The desktop this session shares: the pass that draws it hands it the
/// frames, and every call sharing it encodes what it took.
pub(crate) fn screen_share() -> &'static Arc<ScreenShare> {
    static SHARE: std::sync::OnceLock<Arc<ScreenShare>> = std::sync::OnceLock::new();
    SHARE.get_or_init(|| Arc::new(ScreenShare::new()))
}

/// When the screen a call shares next needs a frame of the desk; `None` while
/// nothing is shared, or a locked desktop withholds it.
pub(crate) fn next_screen_frame() -> Option<Instant> {
    screen_share().next_frame(Instant::now())
}

/// Hand the desktop this session shares the frame the pass just drew: the
/// largest screen layer at most, and never a locked screen.
pub(crate) fn share_screen(compositor: &mut tilo_compositor::Compositor) {
    let locked = compositor.locked();
    hand_screen(screen_share(), locked, || {
        crate::capture::video(compositor, SCREEN_MAX)
    });
}

/// Hand `share` what `draw` draws of the desktop, unless a locked desktop
/// stops it first: locking forgets the frame the share held, so nothing of
/// the screen before the lock is ever sent, now or after unlocking.
fn hand_screen(
    share: &ScreenShare,
    locked: bool,
    draw: impl FnOnce() -> Result<image::RgbaImage, String>,
) {
    if !share.sharing() {
        return;
    }
    if locked {
        share.withhold();
        return;
    }
    if !share.wants_frame(Instant::now()) {
        return;
    }
    match draw() {
        Ok(picture) => {
            let (width, height) = picture.dimensions();
            share.publish_rgba(width, height, picture.into_raw());
        }
        Err(error) => tracing::warn!(%error, "no frame of the desktop to share"),
    }
}

/// The layers of each source this device shows of `person`: nothing at all
/// for a device whose person the app shows nowhere.
fn layers_shown(
    shown: &[Shown],
    person: Option<&String>,
) -> (Vec<pictures::Layer>, Vec<pictures::Layer>) {
    let Some(person) = person else {
        return (Vec::new(), Vec::new());
    };
    let mut camera = BTreeSet::new();
    let mut screen = BTreeSet::new();
    for picture in shown.iter().filter(|picture| &picture.person == person) {
        match picture.source {
            PictureSource::Camera => {
                camera.insert(layer_of(picture.layer));
            }
            PictureSource::Screen => {
                screen.insert(layer_of(picture.layer));
            }
        }
    }
    (camera.into_iter().collect(), screen.into_iter().collect())
}

/// The layer a picture the app shows is received at.
const fn layer_of(layer: PictureLayer) -> pictures::Layer {
    match layer {
        PictureLayer::Low => pictures::Layer::Low,
        PictureLayer::Mid => pictures::Layer::Mid,
        PictureLayer::High => pictures::Layer::High,
    }
}

/// The source a picture the app shows is of.
const fn source_of(source: pictures::Source) -> PictureSource {
    match source {
        pictures::Source::Camera => PictureSource::Camera,
        pictures::Source::Screen => PictureSource::Screen,
    }
}

/// A device's key, as the app names the stream of its pictures.
fn hex(device: &[u8; 32]) -> String {
    use std::fmt::Write as _;
    device
        .iter()
        .fold(String::with_capacity(64), |mut text, byte| {
            let _ = write!(text, "{byte:02x}");
            text
        })
}

#[cfg(test)]
mod tests {
    //! A call on the native desk, in two shapes: an app joins a voice channel
    //! on a session that can carry one, and the person and the people in the
    //! workspace hear each other; and a session with no transport ends the
    //! call it cannot carry, saying why.
    //!
    //! The audio here is the call's own: the same encoder, plane, sealing and
    //! playout a device's call runs, ending in memory instead of a speaker,
    //! and a tone instead of a microphone.

    use std::collections::HashMap;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use crate::fabric::Continuity;
    use channel_crypto::{ChannelGroup, MemberKey, VaultKey};
    use collab_types::presence::{Availability, Payload, Presence};
    use collab_types::{ChannelRoute, Ed25519PublicKey};
    use iroh::EndpointId;
    use runtime_collab::native_media::{Command, Request, State};
    use tilo_action_broker::channel_encryption::{
        ChannelDirectory, ChannelEncryption, ChannelEncryptionConfig, ChannelKeyAccess,
        ChannelKeyProvider,
    };
    use tilo_action_broker::collab_core::BrokerChannels;
    use tilo_compositor::OutputConfig;
    use tilo_desktop_model::{DesktopTree, Workspace, WorkspaceId, WorkspaceSlot};
    use tilo_network_actor::{HostIdentity, NetEvent, workspace_topic_id};

    use super::*;
    use crate::sync::{Client, OwnPresence};
    use crate::{ActionPlane, Options, Session};
    use iroh::endpoint::presets;
    use iroh::protocol::Router;
    use iroh::{Endpoint, RelayMode, SecretKey};
    use tilo_action_broker::channel_encryption::ChannelAccess;
    use tilo_browser_peer::{HttpClient, HttpFuture, LaneCall};
    use tilo_call_lane::page::{Picture, Sealed, Up};
    use tilo_live::plane::Join;
    use tilo_live::relay::{Denied, Peer, Relay, RelayAuthority, SessionAuthorization};
    use tilo_live::roster::Roster as LiveRoster;
    use tilo_live::scope::Scope;

    const WORKSPACE: &str = "0b6a8f2c-3d4e-4f5a-8b9c-0d1e2f3a4b5c";
    const CHANNEL: &str = "c_standup";
    const APP: &str = "so.tilo.chat";
    const RECORD: &str = "chat";
    /// What the tone's playout reads as heard, against the silence a call
    /// plays while nobody speaks.
    const HEARD: f32 = 0.02;

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

    /// One device's key, as the workspace's governance names it.
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

    fn workspace_id() -> WorkspaceId {
        WorkspaceId::parse(WORKSPACE).expect("a workspace id")
    }

    fn target() -> CallTarget {
        CallTarget {
            workspace: WORKSPACE.to_owned(),
            app: APP.to_owned(),
            record: RECORD.to_owned(),
            channel: CHANNEL.to_owned(),
        }
    }

    /// A desk showing one workspace, as this device holds it.
    fn desk_tree() -> DesktopTree {
        let workspace_id = workspace_id();
        let workspace = Workspace::in_workspace(
            WorkspaceSlot::of_workspace(workspace_id),
            workspace_id,
            "Desk",
        );
        DesktopTree {
            workspaces: vec![workspace.clone()],
            active: workspace.slot,
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
            Continuity::default(),
            Arc::new(|| {}),
            None,
            Vec::new(),
        )
        .expect("a loopback endpoint binds")
    }

    /// The workspace as this device knows its governance: one device governs
    /// it and both of a call's devices may read its channels.
    struct Directory {
        governor: Ed25519PublicKey,
        admitted: std::collections::BTreeSet<Ed25519PublicKey>,
    }

    impl ChannelDirectory for Directory {
        fn governor_of(&self, _workspace: WorkspaceId) -> Option<Ed25519PublicKey> {
            Some(self.governor)
        }
        fn allowed_devices(
            &self,
            _workspace: WorkspaceId,
            _channel: collab_types::ChannelId,
            _route: Option<&ChannelRoute>,
            _now: u64,
        ) -> Option<std::collections::BTreeSet<Ed25519PublicKey>> {
            Some(self.admitted.clone())
        }
    }

    /// One channel's keys, as the device `device` of the workspace `governor`
    /// runs holds them: ready at once under the workspace's own device, and
    /// still on their way under any other.
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

    /// A session taking part in the workspace through `sync`, showing the
    /// desk.
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

    /// One thing `from` said in the workspace.
    fn said(from: u8, msg: neighbour_wire::NeighbourMsg) -> NetEvent {
        NetEvent::Msg {
            ws: workspace_topic_id(workspace_id()),
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

    /// One presence `from` said of themselves on the workspace's live plane.
    fn presence(client: &Client, from: u8, payload: collab_types::PeerPresencePayload) {
        let payload = match payload {
            collab_types::PeerPresencePayload::Arrived => Payload::Arrived,
            collab_types::PeerPresencePayload::Availability {
                state,
                call,
                camera,
                screen,
            } => Payload::Availability {
                state,
                call,
                camera,
                screen,
            },
            other => panic!("the tests say no {other:?} as a desktop"),
        };
        let frame = Presence::desktop(WORKSPACE, payload);
        client.heard_presence(WORKSPACE, public(from), &frame);
    }

    /// How available `from` says they are, and the call they say they are in.
    fn availability(client: &Client, from: u8, call: Option<String>) {
        presence(
            client,
            from,
            collab_types::PeerPresencePayload::Availability {
                state: Availability::default().id().to_owned(),
                call,
                camera: false,
                screen: false,
            },
        );
    }

    /// `from` is in the call `call` names, with their camera and screen as
    /// `camera` and `screen` say.
    fn availability_showing(client: &Client, from: u8, call: String, camera: bool, screen: bool) {
        presence(
            client,
            from,
            collab_types::PeerPresencePayload::Availability {
                state: Availability::default().id().to_owned(),
                call: Some(call),
                camera,
                screen,
            },
        );
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

    /// The instant `bound` from now: the moment a sound a person made has to
    /// be heard by.
    fn within(bound: Duration) -> Instant {
        Instant::now()
            .checked_add(bound)
            .expect("a deadline this machine reaches")
    }

    /// Wait until this side of the call does, or does not, hear the tone.
    ///
    /// What it waits for is the call's own audio: moved by the call's worker
    /// on its own thread, between two sessions' devices, so the bound is the
    /// suite's, not a second count.
    #[allow(
        clippy::future_not_send,
        reason = "a session's client is not Send, and the test runtime does not need it to be"
    )]
    async fn wait_heard(what: &str, probe: &Arc<Mutex<Vec<f32>>>, heard: bool) {
        wait_heard_by(what, probe, heard, tilo_test_env::hang_deadline()).await;
    }

    /// As [`wait_heard`], by the instant `deadline`: the audio a person
    /// unmuting is heard again is theirs at once, not after a bound long
    /// enough for a stream that started over to catch up with where it was.
    #[allow(
        clippy::future_not_send,
        reason = "a session's client is not Send, and the test runtime does not need it to be"
    )]
    async fn wait_heard_by(
        what: &str,
        probe: &Arc<Mutex<Vec<f32>>>,
        heard: bool,
        deadline: Instant,
    ) {
        loop {
            let playing = level(probe, Duration::from_millis(200)).await > HEARD;
            if playing == heard {
                return;
            }
            assert!(Instant::now() < deadline, "{what}");
        }
    }

    /// A session that carries its calls: the endpoint it binds, the channel
    /// keys, and the workspace's admitted devices.
    struct Carrier {
        calls: Arc<Calls>,
        factory: Factory,
        probe: Arc<Mutex<Vec<f32>>>,
        /// What carries the workspace's plane's objects to the calls, while a
        /// plane is attached.
        _heard: Option<tokio::task::JoinHandle<()>>,
    }

    /// Wait until `client`'s endpoint has bound: a call runs on it.
    #[allow(
        clippy::future_not_send,
        reason = "a session's client is not Send, and the test runtime does not need it to be"
    )]
    async fn bound(client: &Client) {
        // The endpoint binds on its own task and over the network, so the
        // bound is the suite's, not a second count.
        let deadline = tilo_test_env::hang_deadline();
        while client.endpoint().is_none() {
            assert!(Instant::now() < deadline, "the session's endpoint binds");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    /// The relay the test's planes meet on: in-process, with no one to admit,
    /// since a local plane is its own relay's participant.
    fn relay() -> Arc<Relay> {
        struct Nobody;
        impl RelayAuthority for Nobody {
            fn admit(
                &self,
                _peer: Peer,
                _proof: &[u8],
            ) -> Result<Arc<dyn SessionAuthorization>, Denied> {
                Err(Denied)
            }
        }
        Arc::new(Relay::new(Arc::new(Nobody)))
    }

    /// The workspace scope's content keys, as its own channel hands them to
    /// its members.
    fn workspace_keys() -> channel_crypto::ChannelContentKeys {
        let channel = Scope::workspace(WORKSPACE.to_owned()).channel_id();
        ChannelGroup::create(channel, MemberKey::generate("owner").expect("a member key"))
            .expect("the workspace's channel")
            .content_keys()
            .expect("the workspace's keys")
    }

    /// `device`'s plane in the test workspace, on `relay`, hearing and heard
    /// by `devices`.
    fn plane(
        relay: &Relay,
        device: [u8; 32],
        devices: &[[u8; 32]],
        keys: &channel_crypto::ChannelContentKeys,
    ) -> Arc<Plane> {
        Arc::new(
            Plane::local(
                relay,
                &Join {
                    scope: Scope::workspace(WORKSPACE.to_owned()),
                    device,
                    keys: keys.clone(),
                    roster: LiveRoster::new(devices.iter().copied()),
                    publish: true,
                },
            )
            .expect("a local plane"),
        )
    }

    fn carrier(
        client: &Client,
        keys: &ChannelKeyProvider,
        heard_on: Option<Arc<Plane>>,
    ) -> Carrier {
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
            person: None,
            camera: tilo_call_lane::Camera::TestPattern,
            played: Some(Arc::clone(&probe)),
        });
        // What the session's own pass does with a plane's objects: hand them
        // to the calls playing that workspace.
        let draining = heard_on.map(|plane| {
            calls
                .planes
                .lock_recover()
                .insert(WORKSPACE.to_owned(), Arc::clone(&plane));
            let calls = Arc::clone(&calls);
            tokio::spawn(async move {
                loop {
                    while let Some(heard) = plane.try_recv() {
                        if heard.track == Track::Audio {
                            calls.hear(WORKSPACE, heard);
                        }
                    }
                    tokio::time::sleep(Duration::from_millis(2)).await;
                }
            })
        });
        Carrier {
            calls,
            factory,
            probe,
            _heard: draining,
        }
    }

    /// Two of one person's devices in one call, each hearing the other's
    /// tone: the devices' sides of it, and which of them dials the pair.
    struct Pair {
        left: Carrier,
        right: Carrier,
        here: Box<dyn MediaSession>,
        there: Box<dyn MediaSession>,
        /// Whether the left device dials the pair: of two devices, the one
        /// whose key is smaller does.
        left_dials: bool,
        /// The channel's keys, kept for as long as the call reads them.
        _keys: tempfile::TempDir,
    }

    /// One call between two of the person's devices, both heard in it.
    #[allow(
        clippy::future_not_send,
        reason = "a session's client is not Send, and the test runtime does not need it to be"
    )]
    async fn one_call(laptop: &Client, desk: &Client) -> Pair {
        laptop.connect(desk.endpoint_addr());
        desk.connect(laptop.endpoint_addr());
        laptop.follow_desktop(&desk_tree());
        desk.follow_desktop(&desk_tree());
        // Each session hears the other say it is in the channel's call.
        for (here, there) in [(laptop, 0x22_u8), (desk, 0x11_u8)] {
            here.hear(hello(there, "the other device"));
            presence(here, there, collab_types::PeerPresencePayload::Arrived);
            availability(here, there, Some(call_presence()));
        }
        bound(laptop).await;
        bound(desk).await;
        let (keys, kept) = keys(0x11, 0x11, &[0x11, 0x22]);
        // Voice runs on the workspace's plane: both devices publish and hear
        // on their own plane in one scope, as they do in a workspace's relay.
        let relay = relay();
        let workspace = workspace_keys();
        let devices = [*public(0x11).as_bytes(), *public(0x22).as_bytes()];
        let left = carrier(
            laptop,
            &keys,
            Some(plane(&relay, devices[0], &devices, &workspace)),
        );
        let right = carrier(
            desk,
            &keys,
            Some(plane(&relay, devices[1], &devices, &workspace)),
        );
        let mut here = left
            .factory
            .prepare(target())
            .await
            .expect("a call the workspace's keys and transport carry");
        let mut there = right
            .factory
            .prepare(target())
            .await
            .expect("the other device's side of it");
        here.start_capture("tone").expect("the test's tone");
        there.start_capture("tone").expect("the test's tone");
        wait_heard("each device hears the other's tone", &left.probe, true).await;
        wait_heard("the other device hears this one's tone", &right.probe, true).await;
        Pair {
            left_dials: *public(0x11).as_bytes() < *public(0x22).as_bytes(),
            left,
            right,
            here,
            there,
            _keys: kept,
        }
    }

    /// One device of a call leaves it and comes back: the device that stayed
    /// hears it again within a second of its return, and it hears the one
    /// that stayed.
    ///
    /// `dialer_leaves` picks which of the pair goes — the one that dials the
    /// other's lane, or the one that only answers it — because both ends of a
    /// lane that ended have to bring it back, and each does it its own way.
    #[allow(
        clippy::future_not_send,
        reason = "a session's client is not Send, and the test runtime does not need it to be"
    )]
    async fn leaves_and_rejoins(dialer_leaves: bool) {
        let person = person_key();
        let laptop = device(&person, 0x11, "Laptop");
        let desk = device(&person, 0x22, "Desk");
        let Pair {
            left,
            right,
            mut here,
            mut there,
            left_dials,
            ..
        } = one_call(&laptop, &desk).await;
        let (leaving, staying, rejoining) = if left_dials == dialer_leaves {
            (&mut here, &right, &left)
        } else {
            (&mut there, &left, &right)
        };
        // Heard for a while, the device owns a place in the playout of the
        // one that stays, well past the start of a stream: a stream that
        // starts over would take that long to be heard again.
        tokio::time::sleep(Duration::from_millis(2000)).await;
        wait_heard("the device is heard before it leaves", &staying.probe, true).await;
        // Left, this device is heard by no one, and its lane is gone.
        leaving.close();
        assert_eq!(
            leaving.tick(Instant::now()),
            Err(Issue::SessionEnded),
            "a left call is over"
        );
        wait_heard(
            "a device that left is heard by no one",
            &staying.probe,
            false,
        )
        .await;
        // Back in the call, it is heard again at once: its stream starts at
        // the place its own playhead gives it, not behind where the device
        // that stayed was already playing the stream it lost.
        let mut back = rejoining
            .factory
            .prepare(target())
            .await
            .expect("the device rejoins the call");
        back.start_capture("tone").expect("the test's tone");
        wait_heard_by(
            "a device that rejoined is heard again",
            &staying.probe,
            true,
            within(Duration::from_secs(1)),
        )
        .await;
        wait_heard_by(
            "a device that rejoined hears the call",
            &rejoining.probe,
            true,
            within(Duration::from_secs(1)),
        )
        .await;
        back.close();
    }

    /// A device that leaves the call and rejoins is heard again at once.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_device_that_leaves_a_call_and_rejoins_is_heard_again_at_once() {
        leaves_and_rejoins(true).await;
    }

    /// A lane that dropped is dialled again by the device that dials it, so
    /// the device on the other end is heard as soon as it is back.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_lane_that_dropped_is_dialled_again_and_its_device_is_heard() {
        leaves_and_rejoins(false).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn two_devices_in_one_call_hear_each_other_until_a_mute_a_deafen_or_a_leave() {
        let person = person_key();
        let laptop = device(&person, 0x11, "Laptop");
        let desk = device(&person, 0x22, "Desk");
        let Pair {
            left,
            right,
            mut here,
            mut there,
            ..
        } = one_call(&laptop, &desk).await;
        assert_eq!(
            left.calls.callers(CHANNEL),
            vec![*public(0x22).as_bytes()],
            "the call opens a lane to the other device in the channel"
        );
        assert_eq!(
            here.tick(Instant::now()),
            Ok(State::Live),
            "a call with the channel's keys is live"
        );

        // The tone has been heard this long before this device is muted: a
        // stream that starts over when it unmutes is behind by every frame
        // it spoke here, which is seconds of silence, not the instant a
        // stream that keeps its place is heard at once.
        assert!(
            level(&left.probe, Duration::from_secs(6)).await > HEARD,
            "the tone speaks until this device mutes"
        );

        // Muted, this device is heard by no one, while it still hears.
        here.stop_capture();
        wait_heard("a muted device is not heard", &right.probe, false).await;
        wait_heard("a muted device still hears", &left.probe, true).await;

        // Unmuted, this device is heard again at once, and deafened it hears
        // nothing while the call plays on.
        here.start_capture("tone").expect("the test's tone");
        wait_heard_by(
            "unmuting is heard again",
            &right.probe,
            true,
            Instant::now() + Duration::from_secs(3),
        )
        .await;
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

    /// Ana's camera as `there` receives it, until `wanted` pictures came:
    /// every one the Low layer of her one stream, returning what they cost,
    /// the largest among them, and how long they took.
    #[allow(
        clippy::future_not_send,
        reason = "a session's call is not Send, and the test runtime does not need it to be"
    )]
    async fn receive_camera(
        there: &mut Box<dyn MediaSession>,
        cursor: &mut u64,
        wanted: usize,
    ) -> (usize, usize, Duration) {
        let low = pictures::Layer::Low.size(pictures::Source::Camera);
        let started = Instant::now();
        let deadline = tilo_test_env::hang_deadline();
        let mut seen = 0_usize;
        let mut bytes = 0_usize;
        let mut biggest = 0_usize;
        while seen < wanted {
            let (received, next) = there.pictures(*cursor);
            *cursor = next;
            for picture in received {
                assert_eq!(picture.person, person(0x11), "named by the person Ana is");
                assert_eq!(picture.source, PictureSource::Camera);
                assert_eq!(
                    picture.stream,
                    hex(public(0x11).as_bytes()),
                    "one stream: the one source Ana publishes"
                );
                let unit = picture.unit().expect("a picture that holds together");
                assert_eq!(
                    (unit.width, unit.height),
                    low,
                    "the layer Ben shows, not the camera's 720p source"
                );
                assert_eq!(
                    picture.object.get(3),
                    Some(&0),
                    "the low layer's own pictures, and no other layer's"
                );
                bytes = bytes.saturating_add(picture.object.len());
                biggest = biggest.max(picture.object.len());
                seen = seen.saturating_add(1);
            }
            assert!(
                Instant::now() < deadline,
                "Ben receives Ana's camera at the layer he shows"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        (
            bytes,
            biggest,
            Instant::now().saturating_duration_since(started),
        )
    }

    /// Ana's camera, shown by Ben's app in a filmstrip tile: the call encodes
    /// the layer he shows and nothing else, so what arrives is the size of
    /// that layer and its bitrate, not the camera's 720p source.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_device_shows_anothers_camera_at_the_one_layer_it_shows() {
        let root = person_key();
        let ana = device(&root, 0x11, "Laptop");
        let ben = device(&root, 0x22, "Desk");
        ana.connect(ben.endpoint_addr());
        ben.connect(ana.endpoint_addr());
        ana.follow_desktop(&desk_tree());
        ben.follow_desktop(&desk_tree());
        for (here, there) in [(&ana, 0x22_u8), (&ben, 0x11_u8)] {
            here.hear(hello(there, "the other device"));
            presence(here, there, collab_types::PeerPresencePayload::Arrived);
            availability(here, there, Some(call_presence()));
        }
        bound(&ana).await;
        bound(&ben).await;
        let (keys, _kept) = keys(0x11, 0x11, &[0x11, 0x22]);
        let left = carrier(&ana, &keys, None);
        let right = carrier(&ben, &keys, None);
        assert_eq!(
            left.calls.persons(CHANNEL).get(public(0x22).as_bytes()),
            Some(&person(0x22)),
            "presence names the person Ben's device is"
        );
        let mut here = left
            .factory
            .prepare(target())
            .await
            .expect("Ana's side of the call");
        let mut there = right
            .factory
            .prepare(target())
            .await
            .expect("Ben's side of the call");
        assert_eq!(here.tick(Instant::now()), Ok(State::Live));

        // Ben's app shows Ana's camera in a tile too small for more: Low.
        there.show(vec![Shown {
            person: person(0x11),
            source: PictureSource::Camera,
            layer: PictureLayer::Low,
        }]);
        here.set_camera(true).expect("Ana's camera");

        let mut cursor = 0;
        // One layer's bitrate, not a 720p source's: what arrived over the
        // time it took fits the layer's budget, keyframes and all.
        let (bytes, biggest, elapsed) = receive_camera(&mut there, &mut cursor, 10).await;
        let budget = usize::try_from(
            u64::from(pictures::Layer::Low.bitrate(pictures::Source::Camera))
                .saturating_mul(elapsed.as_millis().try_into().unwrap_or(0))
                / 8_000,
        )
        .unwrap_or(usize::MAX);
        assert!(
            bytes <= budget.saturating_mul(4) + 64 * 1024,
            "the pictures cost the layer's bitrate, not the 720p source's: {bytes} bytes in {elapsed:?}"
        );
        assert!(
            biggest <= 1280 * 720 * 3 / 2 / 16,
            "no picture carries a 720p frame: {biggest} bytes at most"
        );

        // Ana turns her camera off: Ben receives nothing more.
        here.set_camera(false).expect("the camera stops");
        tokio::time::sleep(Duration::from_millis(300)).await;
        // Whatever was in flight is taken and dropped: nothing follows it.
        let (in_flight, next) = there.pictures(cursor);
        cursor = next;
        drop(in_flight);
        tokio::time::sleep(Duration::from_millis(500)).await;
        let (later, _) = there.pictures(cursor);
        assert!(
            later.is_empty(),
            "no picture after the camera stopped: {} arrived",
            later.len()
        );
        here.close();
        there.close();
    }

    /// A locked desktop's share stops at once: the frame the pass drew before
    /// the lock is never handed on, and a locked pass draws nothing new.
    #[test]
    fn a_locked_screen_stops_the_share_at_once() {
        let share = ScreenShare::new();
        share.set_sharing(true);
        let drawn = || {
            Ok(image::RgbaImage::from_pixel(
                640,
                360,
                image::Rgba([200, 40, 40, 255]),
            ))
        };
        hand_screen(&share, false, drawn);
        assert!(
            share.take().is_some(),
            "an unlocked desktop hands the pass's frame on"
        );
        hand_screen(&share, false, drawn);
        assert!(
            !share.wants_frame(Instant::now()),
            "a frame this recent is not drawn again"
        );
        assert!(share.take().is_some(), "the frame the pass just drew");
        hand_screen(&share, false, drawn);
        assert!(share.take().is_some(), "a frame worth drawing arrives");
        // The screen locks: the frame the pass drew last is forgotten, and a
        // locked pass hands on nothing.
        hand_screen(&share, true, || panic!("a locked desktop is not drawn"));
        assert!(share.withheld(), "the share withholds what it held");
        assert!(
            share.take().is_none(),
            "nothing of the locked screen remains"
        );
        hand_screen(&share, true, || panic!("a locked desktop is not drawn"));
        assert!(share.take().is_none(), "a locked desktop shares nothing");
        // Unlocked again, the pass draws a fresh frame and the share goes on.
        share.publish_rgba(320, 180, vec![0; 320 * 180 * 4]);
        assert!(!share.withheld(), "an unlocked desktop shares again");
        assert!(share.take().is_some(), "the frame drawn after unlocking");
        share.set_sharing(false);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_call_joins_while_the_channels_keys_are_still_on_their_way() {
        let person = person_key();
        // This desk's device is not the one its workspace is governed by, and
        // its channel's keys have not reached it yet.
        let laptop = device(&person, 0x22, "Laptop");
        let desk = device(&person, 0x11, "Desk");
        laptop.connect(desk.endpoint_addr());
        desk.connect(laptop.endpoint_addr());
        laptop.follow_desktop(&desk_tree());
        desk.follow_desktop(&desk_tree());
        laptop.hear(hello(0x11, "the other device"));
        presence(&laptop, 0x11, collab_types::PeerPresencePayload::Arrived);
        availability(&laptop, 0x11, Some(call_presence()));
        bound(&laptop).await;
        let (keys, _kept) = keys(0x22, 0x11, &[0x11, 0x22]);
        let left = carrier(&laptop, &keys, None);
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
        presence(&peer, 0xf0, collab_types::PeerPresencePayload::Arrived);
        availability(&peer, 0xf0, Some(call_presence()));
        let calls = peer.calls();
        let heard = calls
            .iter()
            .find(|call| call.channel == CHANNEL)
            .expect("the peer reads the call this person is in");
        assert_eq!(heard.workspace, WORKSPACE);
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

    /// A channel's roster says who in it has a camera or a screen on, as the
    /// presence of their devices says: theirs and this device's own alike.
    #[test]
    fn a_channels_roster_says_who_has_a_camera_or_screen_on() {
        let mine = Client::untethered(
            public(0xf0),
            OwnPresence {
                user: Some(person(0xf0)),
                ..OwnPresence::default()
            },
        );
        mine.follow_desktop(&desk_tree());
        // Two people in the channel, sending nothing: in it, with neither
        // on.
        for (seed, label) in [(0xaa, "Framework 16"), (0xbb, "ThinkPad")] {
            mine.hear(hello(seed, label));
            presence(&mine, seed, collab_types::PeerPresencePayload::Arrived);
            availability(&mine, seed, Some(call_presence()));
        }

        let calls = Calls::new();
        calls.follow(&mine, None);
        let roster = |calls: &Calls| {
            calls
                .rosters()
                .into_iter()
                .find(|roster| roster.channel_id == CHANNEL)
                .expect("the channel the peers are in")
        };
        // A roster lists people by the id everyone here knows them by.
        let heard = roster(&calls);
        assert_eq!(heard.people, [person(0xbb), person(0xaa)]);
        assert!(heard.cameras.is_empty(), "{heard:?}");
        assert!(heard.screens.is_empty(), "{heard:?}");

        // Their camera on, their screen shared: the roster says so.
        availability_showing(&mine, 0xaa, call_presence(), true, true);
        calls.follow(&mine, None);
        let heard = roster(&calls);
        assert_eq!(heard.cameras, [person(0xaa)]);
        assert_eq!(heard.screens, [person(0xaa)]);

        // This device's own camera is in the roster its apps read too,
        // though nobody here hears it from this session.
        let in_call = sync::InCall {
            call: ui::CallPresence {
                channel: CHANNEL.to_owned(),
                name: "Standup".to_owned(),
            },
            camera: true,
            screen: false,
        };
        calls.follow(&mine, Some(&in_call));
        let heard = roster(&calls);
        assert_eq!(heard.people, [person(0xf0), person(0xbb), person(0xaa)]);
        assert_eq!(heard.cameras, [person(0xf0), person(0xaa)]);
        assert_eq!(heard.screens, [person(0xaa)]);
    }

    /// A page's turns with its host, over the HTTP a host's call lane route
    /// answers: one `POST` of a sealed turn and its answer.
    struct HttpTurns {
        addr: std::net::SocketAddr,
    }

    impl HttpClient for HttpTurns {
        fn post(&self, path: &str, _session: Option<&str>, body: Vec<u8>) -> HttpFuture<'_> {
            use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

            let addr = self.addr;
            let path = path.to_owned();
            Box::pin(async move {
                let failed = |error: std::io::Error| error.to_string();
                let mut stream = tokio::net::TcpStream::connect(addr).await.map_err(failed)?;
                let head = format!(
                    "POST {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                stream.write_all(head.as_bytes()).await.map_err(failed)?;
                stream.write_all(&body).await.map_err(failed)?;
                let mut answer = Vec::new();
                stream.read_to_end(&mut answer).await.map_err(failed)?;
                let split = answer
                    .windows(4)
                    .position(|window| window == b"\r\n\r\n")
                    .ok_or_else(|| "the answer has no head".to_owned())?;
                let head = String::from_utf8_lossy(&answer[..split]).into_owned();
                let status = head
                    .split_whitespace()
                    .nth(1)
                    .and_then(|code| code.parse::<u16>().ok())
                    .ok_or_else(|| "the answer has no status".to_owned())?;
                let body = answer
                    .get(split..)
                    .and_then(|rest| rest.get(4..))
                    .unwrap_or_default()
                    .to_vec();
                Ok((status, body))
            })
        }
    }

    /// The address a loopback endpoint answers on.
    fn bound_addr(endpoint: &Endpoint) -> iroh::EndpointAddr {
        use std::net::{Ipv4Addr, SocketAddr};
        let port = endpoint
            .bound_sockets()
            .first()
            .expect("a bound socket")
            .port();
        iroh::EndpointAddr::new(endpoint.id())
            .with_ip_addr(SocketAddr::from((Ipv4Addr::LOCALHOST, port)))
    }

    /// The channel keys a device of `CHANNEL` holds, as a page's sealing needs
    /// them.
    fn channel_keys(keys: &ChannelKeyProvider) -> channel_crypto::ChannelContentKeys {
        let access = keys
            .get()
            .expect("this device's channel keys")
            .access(
                workspace_id(),
                collab_types::document_channel_id(APP, WORKSPACE, RECORD),
            )
            .0;
        let ChannelAccess::Ready(content) = access else {
            panic!("the channel's keys are ready");
        };
        content
    }

    /// A page and a native device in one call: each publishes its microphone
    /// on the workspace's plane and hears the other there, the page sealing
    /// as its own device, while the call's lane carries only the page's
    /// pictures, which the call shows the service.
    #[allow(clippy::too_many_lines, reason = "one scenario, read top to bottom")]
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_page_in_a_call_is_heard_and_hears() {
        let owner = person_key();
        let laptop = device(&owner, 0x11, "Laptop");
        // The page's device: the key its frames name, and the device its host
        // takes part as.
        let page_device = *public(0x22).as_bytes();
        let laptop_device = *public(0x11).as_bytes();
        let roster = tilo_call_lane::Roster::new([laptop_device, page_device]);
        let bridge = tilo_call_lane::Bridge::new(
            WORKSPACE.to_owned(),
            collab_types::document_channel_id(APP, WORKSPACE, RECORD),
            CHANNEL.to_owned(),
            page_device,
            roster.clone(),
        );
        let endpoint = Endpoint::builder(presets::Minimal)
            .relay_mode(RelayMode::Disabled)
            .secret_key(SecretKey::from_bytes(&[0x22; 32]))
            .bind_addr((std::net::Ipv4Addr::LOCALHOST, 0))
            .expect("a loopback endpoint binds")
            .bind()
            .await
            .expect("a loopback endpoint binds");
        let answering = Router::builder(endpoint.clone())
            .accept(tilo_call_lane::ALPN, bridge.handler())
            .spawn();

        // The host's route: what the page posts, the bridge takes in, and its
        // answer posted back.
        let exchange_with = Arc::clone(&bridge);
        let turning_handle = tokio::runtime::Handle::current();
        let responder = test_http::Responder::handle(move |request| {
            let up: Up = postcard::from_bytes(&request.body).ok()?;
            let down = turning_handle.block_on(exchange_with.exchange(up)).ok()?;
            let body = postcard::to_stdvec(&down).ok()?;
            Some(test_http::Reply::status(200).body(body))
        });
        let page_addr: std::net::SocketAddr = responder
            .url()
            .trim_start_matches("http://")
            .parse()
            .expect("the responder's address");

        // The workspace says the page's device is in the call, and the two
        // devices know where each other is.
        laptop.connect(bound_addr(&endpoint));
        laptop.follow_desktop(&desk_tree());
        laptop.hear(hello(0x22, "Page"));
        presence(&laptop, 0x22, collab_types::PeerPresencePayload::Arrived);
        availability(&laptop, 0x22, Some(call_presence()));
        bound(&laptop).await;
        let (keys, _kept) = keys(0x11, 0x11, &[0x11, 0x22]);
        // Voice runs on the workspace's plane: the page publishes its
        // microphone there as its own device and hears the laptop there,
        // while the lane carries only pictures.
        let relay = relay();
        let workspace = workspace_keys();
        let devices = [laptop_device, page_device];
        let left = carrier(
            &laptop,
            &keys,
            Some(plane(&relay, laptop_device, &devices, &workspace)),
        );
        let page_plane = plane(&relay, page_device, &devices, &workspace);

        // The call shows the page's person in it.
        let rosters = left.calls.rosters();
        let standup = rosters
            .iter()
            .find(|roster| roster.channel_id == CHANNEL)
            .expect("the roster of the channel the call is in");
        assert_eq!(
            standup.people,
            vec![person(0x22)],
            "the page's person is in it"
        );

        let mut here = left
            .factory
            .prepare(target())
            .await
            .expect("a call the workspace's keys and transport carry");
        assert_eq!(here.tick(Instant::now()), Ok(State::Live));
        // Of a pair, the smaller device dials; here that is the page's,
        // carried by the bridge.
        if page_device < laptop_device {
            bridge
                .dial(&endpoint, laptop.endpoint_addr())
                .await
                .expect("the bridge opens the call's lanes to the device");
        }
        // The page shows the laptop's camera, so the laptop's camera goes to
        // it, and the laptop shows the page's: every layer of the page's
        // pictures is the one the call draws.
        here.show(vec![Shown {
            person: person(0x22),
            source: PictureSource::Camera,
            layer: PictureLayer::Low,
        }]);

        // The page: the tone its microphone makes, sealed as its device, and
        // what came back played out.
        let page_probe = Arc::new(Mutex::new(Vec::new()));
        let played = Arc::clone(&page_probe);
        let page = tilo_call_lane::Page::new(
            WORKSPACE.to_owned(),
            CHANNEL.to_owned(),
            collab_types::document_channel_id(APP, WORKSPACE, RECORD),
            page_device,
            &channel_keys(&keys),
            roster,
        )
        .expect("a page sealed as its device");
        let lane = LaneCall::new(page, Arc::new(HttpTurns { addr: page_addr }), None);
        let turning = tokio::spawn(async move {
            let mut voice = tokio::task::spawn_blocking(|| Voice::open(Audio::Software))
                .await
                .expect("the page's audio")
                .expect("the test's audio");
            let sources = voice.sources();
            voice
                .start(&sources[0], Arc::new(|| true))
                .expect("the test's tone");
            let mut lane = lane;
            let mut sequence = 0_u64;
            let mut published = 0_u32;
            let mut beat = tokio::time::interval(Duration::from_millis(20));
            loop {
                beat.tick().await;
                let now = Instant::now();
                let mut up = lane.page().up();
                if let Ok(frames) = voice.pump(now) {
                    for frame in frames {
                        let Ok(body) = (VoiceObject {
                            sequence: u64::from(published),
                            timestamp: frame.timestamp_48k,
                            duration: voice::FRAME_SAMPLES,
                            payload: frame.payload,
                        })
                        .encode() else {
                            continue;
                        };
                        if page_plane.publish(Track::Audio, published, &body).is_err() {
                            break;
                        }
                        published = published.wrapping_add(1);
                    }
                }
                while let Some(heard) = page_plane.try_recv() {
                    if heard.track != Track::Audio {
                        continue;
                    }
                    let Ok(object) = VoiceObject::decode(&heard.body) else {
                        continue;
                    };
                    let _ = voice.receive(
                        heard.sender,
                        object.sequence,
                        object.timestamp,
                        object.payload,
                        now,
                    );
                }
                // One picture of the page's camera the first turn, a keyframe,
                // and the ones behind it.
                if sequence < 8 {
                    let picture = Picture {
                        source: tilo_call_lane::Source::Camera,
                        layer: tilo_call_lane::Layer::Low,
                        sequence,
                        keyframe: sequence == 0,
                        timestamp_us: sequence * 33_000,
                        width: 64,
                        height: 64,
                        payload: vec![0_u8; 16],
                    };
                    if let Ok(sealed) = lane.page_mut().picture(picture) {
                        up.pictures.push(Sealed {
                            source: 0,
                            layer: 0,
                            keyframe: sequence == 0,
                            sealed,
                        });
                    }
                    sequence += 1;
                }
                let Ok(down) = lane.turn(up).await else {
                    continue;
                };
                let _ = lane.opened(&down);
                let _ = voice.playout(now);
                if let Ok(mut played) = played.lock() {
                    played.extend(voice.played());
                    let len = played.len();
                    if len > 48_000 {
                        played.drain(..len - 48_000);
                    }
                }
            }
        });
        here.start_capture("tone").expect("the test's tone");

        // The native device hears the page's tone, and the call draws its
        // picture.
        wait_heard("the device hears the page's tone", &left.probe, true).await;
        let deadline = tilo_test_env::hang_deadline();
        loop {
            let (pictures, _) = here.pictures(0);
            if !pictures.is_empty() {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "the call draws the page's picture"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }

        // The page hears the device's tone with it.
        let deadline = tilo_test_env::hang_deadline();
        loop {
            let playing = level(&page_probe, Duration::from_millis(200)).await > HEARD;
            if playing {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "the page hears the device's tone"
            );
        }

        turning.abort();
        here.close();
        answering
            .shutdown()
            .await
            .expect("the bridge's endpoint closes");
    }
}
