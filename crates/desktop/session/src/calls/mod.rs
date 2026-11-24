//! Voice calls on the peer network: the channel this device's call is in,
//! and who is in the channels every workspace this session takes part in shows.
//! Being in a call is the presence every member reads: joining publishes the
//! channel, leaving stops saying it, and a channel's roster is what the members in
//! it say of themselves. A channel id is the whole of a call's identity on the wire.
//! What a call carries is the workspace's live session ([`tilo_live::live_session`]): its
//! voice is one more sealed track, and its pictures are tracks, one per camera and
//! screen layer a device shows, so a layer nobody draws is never encoded. A page in
//! a call publishes and reads its pictures on its own joined live session, as one more
//! device of the workspace.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use collab_types::caps::DeviceCap;
use media::realtime::InputFormat;
use media::realtime::capture::{CaptureConfig, VideoCapture};
use runtime_collab::native_media::{
    BridgedPerson, Input, Issue, Picture, PictureLayer, PictureSource, Roster, Shown, State,
};
use tilo_desktop_model::WorkspaceId;
use tilo_live::live::Track;
use tilo_live::live_session::Heard;
use tilo_live::roster::Roster as LiveRoster;
use tilo_live::voice::{self, VoiceObject};
use tilo_lock::MutexRecover as _;
use tilo_native_media::{CallTarget, MediaFactory, MediaSession, Prepare, Rosters, ScreenShare};

use crate::sync::{self, LiveCall};
use media::voice::call::{self, Audio, Source, Voice};

mod live_pictures;
mod pictures;

/// How often a live call moves what it carries: one frame's worth, so a captured frame is sealed
/// and sent as soon as the microphone or the camera has it.
const PUMP: Duration = Duration::from_millis(10);
/// Voice objects one workspace's live session may have heard before a call plays them: a call that starts
/// late plays the newest of them, never a backlog.
const HEARD: usize = 256;
/// How long a person the call heard counts as speaking after their last frame:
/// what a speaking ring fades from, as the page's own hold.
const SPEAKING_HOLD: Duration = Duration::from_millis(300);
/// Bytes of pictures one workspace's live session may have heard before its calls draw them: a call that
/// stops taking them drops streams whole, which start over at their next keyframe.
const CARRIED_PICTURES: usize = 8 * 1024 * 1024;
/// Pictures one workspace's live session may have heard before its calls draw them: a burst of them is a
/// few passes' worth, never a backlog.
const CARRIED: usize = 256;
/// The largest desktop a shared screen is captured at: the screen's largest layer, so nothing is
/// captured that no tile could show.
const SCREEN_MAX: (u32, u32) = {
    let (width, height) = pictures::Layer::High.size(pictures::Source::Screen);
    (width as u32, height as u32)
};
/// Bytes of received pictures one answer carries at most: several tiles of every layer, so a burst
/// of them is drawn in one pass.
const PICTURES_BUDGET: usize = 1024 * 1024;

/// The calls of every workspace this session takes part in.
pub(crate) struct Calls {
    /// The calls here, as the people in them say.
    live: Mutex<Vec<LiveCall>>,
    /// Who is in each channel, as the members say: what a voice channel shows.
    rosters: Mutex<Vec<Roster>>,
    /// What the rosters last said, to wake the apps watching them only when they moved.
    said: Mutex<String>,
    /// Woken whenever a workspace's calls may have changed.
    changed: Arc<tokio::sync::Notify>,
    /// Woken whenever a call may have pictures to draw: a live session heard one, or a call changed what
    /// it shows.
    arrived: Arc<tokio::sync::Notify>,
    /// This session's live session in each workspace, as it stands: what a call's voice is published
    /// into and heard from.
    live_sessions: Mutex<BTreeMap<String, crate::live::Sessions>>,
    /// What each workspace's live session heard since the last call, newest last: the calls of that
    /// workspace play it.
    heard: Mutex<BTreeMap<String, VecDeque<Heard>>>,
    /// What the live session of each workspace showed for a call of `channel`,
    /// newest last: that call draws it, and no other.
    pictures: Mutex<BTreeMap<(String, String), Carried>>,
    /// Who is in each channel, as a device of it: the person each device in a call is, and the
    /// revision of that map.
    people: Mutex<BTreeMap<String, People>>,
    /// The revision of the next map of people a channel gets.
    people_revision: AtomicU64,
    /// The next voice object each workspace publishes. One counter for the session, so a stream
    /// keeps its place across a call that ends and comes back, and across the microphone being
    /// muted.
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
            arrived: Arc::new(tokio::sync::Notify::new()),
            live_sessions: Mutex::new(BTreeMap::new()),
            heard: Mutex::new(BTreeMap::new()),
            pictures: Mutex::new(BTreeMap::new()),
            people: Mutex::new(BTreeMap::new()),
            people_revision: AtomicU64::new(1),
            audio: Mutex::new(BTreeMap::new()),
        })
    }

    /// Take in the calls everyone here says they are in, and the channel this device joined (if
    /// any), so a voice channel shows a join or a leave on this pass.
    pub(crate) fn follow(&self, sync: &sync::Client, own: Option<&sync::InCall>) {
        let live = sync.calls();
        // This device's own call: nobody here hears it from this session, so it is said from what
        // the session itself published.
        let person = sync.own().user;
        let device = *sync.publisher().as_bytes();
        // What the calls of the workspaces this device is in allow, and whose each member device
        // is: whom a channel list leaves out.
        let mut rules = BTreeMap::new();
        let mut persons = BTreeMap::new();
        if let Some(engine) = sync.engine() {
            for workspace in live_sessions_of(sync).keys() {
                let (its_rules, its_persons) = engine.call_gate(workspace);
                rules.extend(its_rules);
                persons.extend(its_persons);
            }
        }
        let channels = tilo_live::calls::channels(
            &live,
            own.map(|call| tilo_live::calls::Own {
                device,
                person: person.as_deref(),
                call,
            }),
            &persons,
            &rules,
        );
        let rosters: Vec<Roster> = channels.iter().map(roster_of).collect();
        // The other devices of each channel: this one's pictures are never heard back here.
        let devices: BTreeMap<String, BTreeMap<[u8; 32], String>> = channels
            .iter()
            .map(|channel| {
                let mut persons = channel.persons();
                persons.remove(&device);
                (channel.channel.clone(), persons)
            })
            .filter(|(_, persons)| !persons.is_empty())
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
        // Who the callers of each channel are, device by device: a call re-reads its own channel's
        // map when its revision moves, and shows the new devices what the app shows of their
        // people.
        {
            let mut held = self.people.lock_recover();
            held.retain(|channel, _| devices.contains_key(channel));
            for (channel, by_device) in devices {
                if held
                    .get(&channel)
                    .is_some_and(|people| people.by_device == by_device)
                {
                    continue;
                }
                let revision = self.people_revision.fetch_add(1, Ordering::Relaxed);
                held.insert(
                    channel,
                    People {
                        by_device,
                        revision,
                    },
                );
            }
        }
        let live_sessions = live_sessions_of(sync);
        {
            let mut held = self.live_sessions.lock_recover();
            if held.len() != live_sessions.len() || held.keys().ne(live_sessions.keys()) {
                *held = live_sessions;
            }
        }
        // What the live sessions heard and showed since the last pass, each workspace's own: the calls
        // playing it take it.
        for (workspace, heard) in sync.take_call_audio() {
            self.hear(&workspace, heard);
        }
        for (workspace, heard) in sync.take_call_pictures() {
            self.hear(&workspace, heard);
        }
        if moved {
            self.changed.notify_waiters();
        }
    }

    /// Woken whenever a call may have pictures to draw, or its demand moved.
    pub(crate) fn arrived(&self) -> Arc<tokio::sync::Notify> {
        Arc::clone(&self.arrived)
    }

    /// Wake the passes waiting on [`Self::arrived`].
    pub(crate) fn pictures_arrived(&self) {
        self.arrived.notify_waiters();
    }

    /// The live session `workspace` 's call's voice runs on, while this session is in the workspace's
    /// live session.
    pub(crate) fn live_session(&self, workspace: &str) -> Option<crate::live::Sessions> {
        self.live_sessions.lock_recover().get(workspace).cloned()
    }

    /// Put one object `workspace` 's live session heard where the calls playing that workspace take it:
    /// the voice it plays, or the pictures it draws.
    fn hear(&self, workspace: &str, heard: Heard) {
        if heard.track == Track::Audio {
            voice_in(
                self.heard
                    .lock_recover()
                    .entry(workspace.to_owned())
                    .or_default(),
                heard,
            );
        } else {
            // A picture belongs to the call of the channel its sender is a caller of: that call's
            // app draws it, and no other call of the workspace does.
            let channels: Vec<String> = self
                .people
                .lock_recover()
                .iter()
                .filter(|(_, people)| people.by_device.contains_key(&heard.sender))
                .map(|(channel, _)| channel.clone())
                .collect();
            let mut pictures = self.pictures.lock_recover();
            for channel in channels {
                pictures
                    .entry((workspace.to_owned(), channel))
                    .or_default()
                    .carry(heard.clone());
            }
            drop(pictures);
            self.pictures_arrived();
        }
    }

    /// What `workspace` 's live session heard since the last call, oldest first.
    fn took(&self, workspace: &str) -> Vec<Heard> {
        self.heard
            .lock_recover()
            .get_mut(workspace)
            .map(|bucket| bucket.drain(..).collect())
            .unwrap_or_default()
    }

    /// What the live session of `workspace` showed for `channel` 's call since the last pass, oldest
    /// first, and the streams that lost a picture since: nothing of those is drawn until their next
    /// keyframe.
    fn took_pictures(
        &self,
        workspace: &str,
        channel: &str,
    ) -> (Vec<Heard>, Vec<([u8; 32], Track)>) {
        self.pictures
            .lock_recover()
            .get_mut(&(workspace.to_owned(), channel.to_owned()))
            .map(Carried::take)
            .unwrap_or_default()
    }

    /// The sequence of the next voice object this device publishes in `workspace` .
    fn next_audio(&self, workspace: &str) -> u32 {
        let mut audio = self.audio.lock_recover();
        let next = audio.entry(workspace.to_owned()).or_insert(0);
        let sequence = *next;
        *next = next.wrapping_add(1);
        drop(audio);
        sequence
    }

    /// What `channel` is called, as the people in it say; empty when nobody named it.
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

    /// Whether `speaker`, a bridged person, is a member of `channel`'s call the
    /// call lets speak: the only bridged voice a call plays.
    fn bridged_may_speak(&self, channel: &str, speaker: &[u8; 32]) -> bool {
        let id = hex(speaker);
        self.rosters.lock_recover().iter().any(|roster| {
            roster.channel_id == channel
                && roster.bridged.iter().any(|person| person.person == id)
                && !roster.muted.contains(&id)
        })
    }

    /// The devices the workspace says are in `channel` 's call, this one among them: whose objects
    /// the call plays and draws.
    fn callers(&self, channel: &str) -> Vec<[u8; 32]> {
        let live = self.live.lock_recover();
        live.iter()
            .filter(|call| call.channel == channel)
            .flat_map(|call| &call.people)
            .filter_map(|caller| {
                tilo_failure::ignored!(
                    "a caller id that is not an endpoint names no device to hear",
                    caller.id.parse::<iroh::EndpointId>().ok()
                )
            })
            .map(|device| *device.as_bytes())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }
}

impl Calls {
    /// The person each device in `channel` 's call is, as the callers said: what the pictures a
    /// device sends are named by, and whose tile a device feeds. A device whose caller names no
    /// person is left out, its pictures unnamed.
    fn persons(&self, channel: &str) -> BTreeMap<[u8; 32], String> {
        self.people
            .lock_recover()
            .get(channel)
            .map(|people| people.by_device.clone())
            .unwrap_or_default()
    }

    /// How many times the devices of `channel`'s call, or the people they
    /// are, changed: a call tells the devices it finds here what it shows,
    /// and nothing while this stays as it was.
    fn persons_revision(&self, channel: &str) -> u64 {
        self.people
            .lock_recover()
            .get(channel)
            .map(|people| people.revision)
            .unwrap_or_default()
    }
}

/// Who is in one channel, as its callers say: the person each device is, and the revision of that
/// map.
#[derive(Default)]
struct People {
    by_device: BTreeMap<[u8; 32], String>,
    revision: u64,
}

/// The roster a voice channel shows of `channel`'s call: who is in it, and what each of them shows.
fn roster_of(channel: &tilo_live::calls::Channel) -> Roster {
    Roster {
        channel_id: channel.channel.clone(),
        people: channel.people(),
        cameras: channel.cameras(),
        screens: channel.screens(),
        muted: channel.muted(),
        deafened: channel.deafened(),
        bridges: channel.bridges(),
        bridged: channel
            .bridged()
            .into_iter()
            .map(|person| BridgedPerson {
                person: person.person,
                service: person.service,
                account: person.account,
                name: person.name,
                avatar: person.avatar,
            })
            .collect(),
    }
}

/// The workspace live sessions this session is in now: a call's voice and pictures run on the one its
/// workspace is in, and are no longer carried once this device leaves the workspace.
pub(crate) fn live_sessions_of(sync: &sync::Client) -> BTreeMap<String, crate::live::Sessions> {
    sync.live_sessions()
}

/// Keep `heard` with the newest voice `bucket` holds: a call reads only what arrived since its last
/// pass.
fn voice_in(bucket: &mut VecDeque<Heard>, heard: Heard) {
    bucket.push_back(heard);
    while bucket.len() > HEARD {
        bucket.pop_front();
    }
}

/// Pictures one workspace's calls have not drawn yet, and the streams that lost one on the way:
/// past the bound, the oldest stream goes whole, and starts over at its next keyframe.
#[derive(Default)]
struct Carried {
    queue: VecDeque<Heard>,
    bytes: usize,
    lost: BTreeSet<([u8; 32], Track)>,
}

impl Carried {
    /// Queue one picture, dropping whole streams to stay within the bound.
    fn carry(&mut self, heard: Heard) {
        let key = (heard.sender, heard.track);
        if self.lost.contains(&key) {
            return;
        }
        self.bytes = self.bytes.saturating_add(heard.body.len());
        self.queue.push_back(heard);
        while self.queue.len() > CARRIED || self.bytes > CARRIED_PICTURES {
            let Some(oldest) = self.queue.front() else {
                break;
            };
            let stream = (oldest.sender, oldest.track);
            let mut bytes = self.bytes;
            self.queue.retain(|held| {
                let keep = (held.sender, held.track) != stream;
                if !keep {
                    bytes = bytes.saturating_sub(held.body.len());
                }
                keep
            });
            self.bytes = bytes;
            self.lost.insert(stream);
            if stream == key {
                return;
            }
        }
    }

    /// What was queued, and the streams that lost a picture, for the call playing in this
    /// workspace.
    fn take(&mut self) -> (Vec<Heard>, Vec<([u8; 32], Track)>) {
        let lost = self.lost.iter().copied().collect();
        self.lost.clear();
        (self.queue.drain(..).collect(), lost)
    }
}

/// What voice channel `channel` is called, as the app that holds it published the channel on the
/// device's dispatch: the label of the item `channel` of any of its resources
/// (`tl:<app>:<resource>/<channel>`) among `published`, the rows of the apps' listings; `None`
/// when the app publishes no such item.
pub(crate) fn channel_name(
    published: &[tilo_actions::remote::CatalogueRow],
    (workspace, app): (&str, &str),
    channel: &str,
) -> Option<String> {
    let prefix = format!("tl:{app}:");
    published
        .iter()
        .filter(|row| {
            tilo_actions::remote::application_provider(&row.provider) == Some((workspace, app))
        })
        .find_map(|row| {
            let action = row.action.as_ref()?;
            action
                .address
                .resource
                .strip_prefix(&prefix)
                .and_then(|id| id.split_once('/'))
                .is_some_and(|(_, item)| item == channel)
                .then(|| action.resource_label.trim().to_owned())
        })
        .filter(|name| !name.is_empty())
}

/// The devices a workspace's governance admits, read fresh: what a call's roster is made of, and
/// who this device accepts frames from.
pub(crate) type Admitted = Arc<dyn Fn(&str) -> Vec<[u8; 32]> + Send + Sync>;

/// Where a call's carrier reads what only the session knows: its endpoint,
/// the channel service, the workspace's governance, the person's answers,
/// and what the audio runs on.
pub(crate) struct Sources {
    /// The calls of the workspaces this session takes part in, as the loop heard them: who else is
    /// in the channel this device joins.
    pub(crate) calls: Arc<Calls>,
    /// The session's own endpoint, which names this device. `None` for a session taking part in no
    /// transport, which carries no call.
    pub(crate) endpoint: Option<Arc<std::sync::OnceLock<iroh::Endpoint>>>,
    /// This device's channel keys: the ones a call in the channel seals its audio with.
    pub(crate) keys: Option<tilo_workspace_keys::KeyProvider>,
    /// The devices `workspace` admits, which a call's roster names.
    pub(crate) roster: Option<Admitted>,
    /// The person's prompts: the microphone is asked for through them.
    pub(crate) questions: Option<tilo_service::AskSender>,
    /// What the call hears and is heard by.
    pub(crate) audio: Audio,
    /// This person, as the workspace names them: whose tile their own camera feeds.
    pub(crate) person: Option<String>,
    /// What this device's camera captures from.
    pub(crate) camera: pictures::Camera,
    /// Where a test reads the samples the call played, in place of a speaker.
    #[cfg(test)]
    pub(crate) played: Option<Arc<Mutex<Vec<f32>>>>,
}

/// This session as the carrier of a call: being in the channel is joining
/// it, which the session's presence then says to everyone in the workspace;
/// the audio and the pictures travel the workspace's live session.
pub(crate) struct Factory {
    sources: Arc<Sources>,
    /// The person's answers about the microphone: the prompt the session's apps get, so one
    /// allowance covers every call of the session.
    consents: Option<tilo_portal::consent::Consents>,
}

impl Sources {
    /// What a session's own calls read: its endpoint, the channel service,
    /// the workspace's governance, and the person's prompts.
    #[must_use]
    pub(crate) fn carrier(
        calls: Arc<Calls>,
        endpoint: Arc<std::sync::OnceLock<iroh::Endpoint>>,
        keys: Option<tilo_workspace_keys::KeyProvider>,
        roster: Admitted,
        questions: Option<tilo_service::AskSender>,
        person: Option<String>,
    ) -> Self {
        Self {
            calls,
            endpoint: Some(endpoint),
            keys,
            roster: Some(roster),
            questions,
            audio: Audio::Devices,
            person,
            camera: pictures::Camera::from_env(),
            #[cfg(test)]
            played: None,
        }
    }
}

impl Factory {
    #[must_use]
    pub(crate) fn new(sources: Sources) -> Self {
        let consents = sources.questions.clone().map(|questions| {
            tilo_portal::consent::Consents::new(questions, tilo_peer_state::DeviceGrants::unkept())
        });
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

/// Start this device's side of `target` 's call, or say why it cannot carry one here.
async fn open(
    sources: &Sources,
    consents: Option<tilo_portal::consent::Consents>,
    target: CallTarget,
) -> Result<Box<dyn MediaSession>, Issue> {
    let Some(endpoint) = sources
        .endpoint
        .as_ref()
        .and_then(|slot| slot.get().cloned())
    else {
        // No transport, no call.
        return Err(Issue::Unavailable);
    };
    let workspace_id = WorkspaceId::parse(&target.workspace).ok_or(Issue::Unavailable)?;
    let (access, keys) = call_keys(sources, workspace_id)?;
    let roster = admitted(sources, &target.workspace);
    let own = *endpoint.id().as_bytes();
    let video = pictures::Video::new(own);
    let (voice, microphones, offered) = opened_microphones(sources.audio).await?;
    let live = keys.is_some();
    let current = keys.map(|keys| Current { keys, roster });
    let shared = Arc::new(Shared {
        video,
        volumes: Mutex::new(BTreeMap::new()),
        volumes_sent: Mutex::new(BTreeMap::new()),
        screen: Arc::clone(screen_share()),
        showing: Mutex::new(Vec::new()),
        streams: Mutex::new(BTreeMap::new()),
        person: sources.person.clone(),
        presence: Arc::clone(&sources.calls),
        camera: sources.camera,
        camera_choice: Mutex::new(None),
        own,
        access,
        workspace_id,
        workspace: target.workspace.clone(),
        admitted: sources.roster.clone(),
        current: Mutex::new(current),
        side: Mutex::new(Side {
            voice,
            camera: None,
            screen: None,
        }),
        shown_devices: Mutex::new(BTreeSet::new()),
        shows_at: AtomicU64::new(0),
        pictures_at: AtomicU64::new(u64::MAX),
        shows_dirty: AtomicBool::new(false),
        sources: microphones,
        caller: tilo_portal::PortalCaller {
            app_id: target.app.clone(),
            workspace: Some(target.workspace.clone()),
            sandboxed: false,
            portals: None,
        },
        call: target.channel.clone(),
        consents,
        live: AtomicBool::new(live),
        closed: AtomicBool::new(false),
        deafened: AtomicBool::new(false),
        issue: Mutex::new(None),
        speaking: Mutex::new(BTreeMap::new()),
        #[cfg(test)]
        played: sources.played.clone(),
    });
    tokio::spawn(run(Arc::clone(&shared)));
    Ok(Box::new(Call {
        shared,
        inputs: offered,
    }))
}

/// The keys the call's channel holds, or why the call cannot be carried: keys that are on their way
/// still carry it, since it opens joining the channel and the worker takes them when they arrive.
/// Only a device the channel has no keys for at all leaves it uncarried.
fn call_keys(
    sources: &Sources,
    workspace_id: WorkspaceId,
) -> Result<
    (
        tilo_workspace_keys::SharedKeys,
        Option<key_group::ContentKeys>,
    ),
    Issue,
> {
    let access = sources
        .keys
        .as_ref()
        .and_then(tilo_workspace_keys::KeyProvider::get)
        .ok_or(Issue::Unavailable)?;
    let keys = match access.access(workspace_id).0 {
        tilo_workspace_keys::KeyAccess::Ready(keys) => Some(keys),
        tilo_workspace_keys::KeyAccess::Waiting => None,
        _ => return Err(Issue::Unavailable),
    };
    Ok((access, keys))
}

/// What a call's audio said, as the app hears it.
fn voice_issue(error: call::CallError) -> Issue {
    match error {
        call::CallError::Unavailable => Issue::Unavailable,
        call::CallError::CaptureFailed | call::CallError::SourceLost => Issue::SourceLost,
        call::CallError::TransportFailed | call::CallError::Unsupported => Issue::TransportFailed,
    }
}

/// This device's microphones, opened for the call, and what the app is offered of them.
async fn opened_microphones(audio: Audio) -> Result<(Voice, Vec<Source>, Vec<Input>), Issue> {
    let voice = tokio::task::spawn_blocking(move || Voice::open(audio))
        .await
        .map_err(|_| Issue::Unavailable)?
        .map_err(voice_issue)?;
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

/// The devices `workspace` admits: who may send to this device in its channels' calls.
fn admitted(sources: &Sources, workspace: &str) -> LiveRoster {
    let devices = sources
        .roster
        .as_ref()
        .map(|admitted| admitted(workspace))
        .unwrap_or_default();
    LiveRoster::new(devices)
}

/// One call this device is in, as the call service holds it: where the call stands, and what it
/// sends.
pub(crate) struct Call {
    shared: Arc<Shared>,
    inputs: Vec<Input>,
}

impl MediaSession for Call {
    fn tick(&mut self, _now: Instant) -> Result<State, Issue> {
        // A call the person left is over: whatever its audio said in the same moment — a stream
        // that ended with the call, say — is not why it did.
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

    fn cameras(&self) -> (Vec<Input>, Option<String>) {
        self.shared.cameras()
    }

    fn select_camera(&mut self, camera: &str) -> Result<(), Issue> {
        self.shared.select_camera(camera)
    }

    fn set_screen(&mut self, on: bool) -> Result<(), Issue> {
        self.shared.set_screen(on)
    }

    fn speaking(&self) -> Vec<String> {
        self.shared.speaking()
    }

    fn pictures(&mut self, after: u64) -> (Vec<Picture>, u64) {
        self.shared.pictures(after)
    }

    fn pictures_changed(&self) -> Option<Arc<tokio::sync::Notify>> {
        Some(self.shared.video.signal())
    }

    fn set_volume(&mut self, member: &str, percent: u16) {
        self.shared.set_volume(member, percent);
    }

    fn volumes(&self) -> Vec<(String, u16)> {
        self.shared.volumes()
    }

    fn close(&mut self) {
        self.shared.close();
    }
}

/// The channel's keys and who may read them, and what the workspace's governance admitted at the
/// same time: what seals this call's frames.
struct Current {
    keys: key_group::ContentKeys,
    roster: LiveRoster,
}

/// This call's own side: what it plays, the microphone it seals frames with while the person is
/// heard, and the pictures it sends while they are shown.
struct Side {
    voice: Voice,
    /// This device's camera, while the person has it on in the call.
    camera: Option<CameraSend>,
    /// The screen share's encoder, while the person shares their screen.
    screen: Option<Arc<pictures::Sending>>,
}

/// This device's camera: the capture feeding the call's encoder thread, and that thread, which ends
/// when the last handle to it goes.
struct CameraSend {
    /// Dropping the last handle ends the encoder thread and what it sends.
    sending: Arc<pictures::Sending>,
    /// Dropping the capture stops the camera: no frame is published after.
    _capture: VideoCapture,
}

struct Shared {
    /// This device's key: whose pictures the app's are.
    own: [u8; 32],
    /// Where the channel's keys come from when its membership moves.
    access: tilo_workspace_keys::SharedKeys,
    /// The workspace id the channel's keys and governance are read by.
    workspace_id: WorkspaceId,
    workspace: String,
    /// The workspace's admitted devices, re-read while the call lasts.
    admitted: Option<Admitted>,
    /// The channel's keys and roster, once this device holds them: the call is up from then, and
    /// joining until.
    current: Mutex<Option<Current>>,
    side: Mutex<Side>,
    /// The microphones this call may send.
    sources: Vec<Source>,
    /// This call's pictures: what this device sends, and what arrived here for the app to draw.
    video: Arc<pictures::Video>,
    /// The desktop this session shares, which this call encodes while the person shares it.
    screen: Arc<ScreenShare>,
    /// What the app shows of the people in this call, as it last said.
    showing: Mutex<Vec<Shown>>,
    /// The devices this call has shown the app's layers to, on the live session: one that left the call is
    /// unsubscribed, and the live session stops reading its pictures.
    shown_devices: Mutex<BTreeSet<[u8; 32]>>,
    /// The revision of the call's people this call last showed itself to,
    /// and the live session's picture demand it last woke its encoders for.
    shows_at: AtomicU64,
    pictures_at: AtomicU64,
    /// The app changed what it shows, and the call has not told the live session since: the app asks from
    /// wherever its question arrives, and the live session's tracks are made from the call's own task.
    shows_dirty: AtomicBool,
    /// This person, as the workspace names them: whose tile their own camera feeds, and what their
    /// own pictures are labelled.
    person: Option<String>,
    /// The calls this session takes part in, as presence said them: who is in this call, and which
    /// person each device is.
    presence: Arc<Calls>,
    /// What this device's camera captures from.
    camera: pictures::Camera,
    /// The camera the person chose to send from, by its id; the first offered until they choose.
    camera_choice: Mutex<Option<String>>,
    /// The timestamp each device's voice stream was last heard at, in this call: a stream that
    /// starts over is recognised by its own bytes.
    streams: Mutex<BTreeMap<[u8; 32], u64>>,
    caller: tilo_portal::PortalCaller,
    call: String,
    consents: Option<tilo_portal::consent::Consents>,
    /// The channel's keys are here and the call is carried: the call is up.
    live: AtomicBool,
    closed: AtomicBool,
    deafened: AtomicBool,
    /// Why the call ended, once it did.
    issue: Mutex<Option<Issue>>,
    /// When each person's voice was last heard in this call, by speaker id: who
    /// the speaking rings follow.
    speaking: Mutex<BTreeMap<[u8; 32], Instant>>,
    /// How loud this device plays each member of the call, by person, in percent of as they
    /// arrive: what the person set here, which nobody else's call hears.
    volumes: Mutex<BTreeMap<String, u16>>,
    /// The volumes the call's playout was last told, by the device each person is heard from:
    /// sent again only when the map or the call's people move.
    volumes_sent: Mutex<BTreeMap<String, u16>>,
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
            && !consents.allowed(&self.caller, DeviceCap::Microphone)
        {
            // The person has not allowed their microphone yet: ask them through the session's own
            // sheet, and open nothing until they answer. Asking again while the question waits is
            // refused by the service itself.
            let consents = consents.clone();
            let caller = self.caller.clone();
            let handle = format!("call-microphone:{}", self.call);
            let question = tilo_portal::consent::AccessQuestion {
                capability: DeviceCap::Microphone.name().to_owned(),
                reason: Some(crate::i18n::t("call-microphone-reason")),
                may_remember: false,
                text: None,
            };
            tokio::spawn(async move {
                if let Err(error) = consents
                    .decide(&caller, &handle, DeviceCap::Microphone, question)
                    .await
                {
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
            .start(&source, still_current)
            .map_err(voice_issue)?;
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

    /// Show these people's pictures here, each at the layer named: every device of theirs in the
    /// call is told, and encodes only that. The app asks from wherever its question arrives, so the
    /// live session's tracks are made on the call's own pass, from what is recorded here.
    fn show(&self, shown: Vec<Shown>) {
        // This person's own camera tile is fed here, at the largest layer the app shows of them.
        let own = self.person.as_ref().and_then(|own| {
            shown
                .iter()
                .filter(|picture| picture.source == PictureSource::Camera && &picture.person == own)
                .map(|picture| layer_of(picture.layer))
                .max()
        });
        self.video.own_tile(own);
        *self.showing.lock_recover() = shown;
        self.shows_dirty.store(true, Ordering::Release);
        self.presence.pictures_arrived();
    }

    /// Tell every device of the call what this device shows of it: nothing of a device whose person
    /// the app no longer shows, and nothing of a device the call lost.
    fn send_showing(&self, persons: &BTreeMap<[u8; 32], String>) {
        let showing = self.showing.lock_recover().clone();
        let devices: BTreeSet<[u8; 32]> = persons.keys().copied().collect();
        let live_session = self.presence.live_session(&self.workspace);
        {
            let mut shown = self.shown_devices.lock_recover();
            let gone: Vec<[u8; 32]> = shown.difference(&devices).copied().collect();
            if let Some(live_session) = &live_session {
                for device in gone {
                    live_session.show(device, &[]);
                }
            }
            shown.clone_from(&devices);
        }
        for device in devices {
            let (camera, screen) = layers_shown(&showing, persons.get(&device));
            if let Some(live_session) = &live_session {
                let tracks: Vec<Track> = camera
                    .iter()
                    .map(|layer| Track::Camera(*layer))
                    .chain(screen.iter().map(|layer| Track::Screen(*layer)))
                    .collect();
                live_session.show(device, &tracks);
            }
        }
    }

    /// Turn this person's camera on in the call, or off: the capture feeds the call's encoder,
    /// which sends only the layers someone shows.
    fn set_camera(self: &Arc<Self>, on: bool) -> Result<(), Issue> {
        if self.closed() {
            return Err(Issue::CameraLost);
        }
        if !on {
            // Dropping the capture and the sender stops the camera and its encoder.
            self.side.lock_recover().camera = None;
            return Ok(());
        }
        if !self.ask(
            DeviceCap::Camera,
            "call-camera",
            &crate::i18n::t("call-camera-reason"),
        ) {
            return Err(Issue::PermissionDenied);
        }
        let device = self.chosen_camera().ok_or(Issue::CameraLost)?.device;
        self.start_camera(device)
    }

    /// The camera a call sends from: the one the person chose, or the first offered.
    fn chosen_camera(&self) -> Option<pictures::Offered> {
        let offered = self.camera.offered();
        let chosen = self.camera_choice.lock_recover().clone();
        chosen
            .and_then(|id| offered.iter().find(|camera| camera.id == id).cloned())
            .or_else(|| offered.into_iter().next())
    }

    /// The cameras the person may choose between, and the one the call sends from.
    fn cameras(&self) -> (Vec<Input>, Option<String>) {
        let chosen = self.chosen_camera().map(|camera| camera.id);
        let offered = self
            .camera
            .offered()
            .into_iter()
            .map(|camera| Input {
                id: camera.id,
                label: camera.label,
            })
            .collect();
        (offered, chosen)
    }

    /// Send the camera from `id`: a camera that is on moves to it now, without asking again,
    /// since the person allowed the camera when they turned it on.
    fn select_camera(self: &Arc<Self>, id: &str) -> Result<(), Issue> {
        if self.closed() {
            return Err(Issue::CameraLost);
        }
        let offered = self
            .camera
            .offered()
            .into_iter()
            .find(|camera| camera.id == id)
            .ok_or(Issue::CameraLost)?;
        let previous = self.camera_choice.lock_recover().replace(id.to_owned());
        let on = self.side.lock_recover().camera.is_some();
        if !on || previous.as_deref() == Some(id) {
            return Ok(());
        }
        // A camera that would not start leaves the one that sends, and the choice, as they were.
        self.start_camera(offered.device).inspect_err(|_| {
            *self.camera_choice.lock_recover() = previous;
        })
    }

    /// Capture from `device` and send it in the call, in place of whatever the camera sent before.
    fn start_camera(
        self: &Arc<Self>,
        device: media::realtime::capture::CaptureDevice,
    ) -> Result<(), Issue> {
        let sending = Arc::new(
            pictures::Sending::start(pictures::Source::Camera, pictures::CAMERA_FPS, &self.sink())
                .map_err(|_| Issue::CameraLost)?,
        );
        let feed = Arc::downgrade(&sending);
        let (width, height) = pictures::CAMERA_SIZE;
        let capture = VideoCapture::start(
            &CaptureConfig {
                device,
                width,
                height,
                fps: pictures::CAMERA_FPS,
            },
            Box::new(move |frame| {
                let Some(sending) = feed.upgrade() else {
                    return;
                };
                sending.publish(pictures::Frame {
                    pixels: Arc::from(frame.nv12),
                    width: frame.width,
                    height: frame.height,
                    format: InputFormat::Nv12,
                    captured_at: frame.captured_at,
                });
            }),
        )
        .map_err(|_| Issue::CameraLost)?;
        self.side.lock_recover().camera = Some(CameraSend {
            sending,
            _capture: capture,
        });
        Ok(())
    }

    /// Share this person's screen in the call, or stop: the session's own pass hands the share its
    /// frames, and the call's encoder encodes them for the layers someone shows.
    fn set_screen(self: &Arc<Self>, on: bool) -> Result<(), Issue> {
        if self.closed() {
            return Err(Issue::ScreenLost);
        }
        if !on {
            self.side.lock_recover().screen = None;
            self.screen.set_sharing(false);
            return Ok(());
        }
        if !self.ask(
            DeviceCap::Screen,
            "call-screen",
            &crate::i18n::t("call-screen-reason"),
        ) {
            return Err(Issue::PermissionDenied);
        }
        let sending = Arc::new(
            pictures::Sending::start(pictures::Source::Screen, pictures::SCREEN_FPS, &self.sink())
                .map_err(|_| Issue::ScreenLost)?,
        );
        self.screen.set_sharing(true);
        self.side.lock_recover().screen = Some(sending);
        Ok(())
    }

    /// Ask the person through the session's own prompt for `capability` : whether they already
    /// allowed it for this call. `reason` is what the sheet says the call wants it for, in the
    /// person's language. A question that was not answered yet is not asked again; the service
    /// refuses the second.
    fn ask(&self, device: DeviceCap, handle: &str, reason: &str) -> bool {
        let Some(consents) = &self.consents else {
            return true;
        };
        if consents.allowed(&self.caller, device) {
            return true;
        }
        let consents = consents.clone();
        let caller = self.caller.clone();
        let handle = format!("{handle}:{}", self.call);
        let question = tilo_portal::consent::AccessQuestion {
            capability: device.name().to_owned(),
            reason: Some(reason.to_owned()),
            may_remember: false,
            text: None,
        };
        tokio::spawn(async move {
            let _ = consents.decide(&caller, &handle, device, question).await;
        });
        false
    }

    /// Who the call heard talking lately, each once and sorted: a bridged
    /// person among them by the id every call view knows them as.
    fn speaking(&self) -> Vec<String> {
        let now = Instant::now();
        let mut speaking: Vec<String> = self
            .speaking
            .lock_recover()
            .iter()
            .filter(|(_, heard)| now.saturating_duration_since(**heard) < SPEAKING_HOLD)
            .map(|(speaker, _)| hex(speaker))
            .collect();
        speaking.sort();
        speaking
    }

    /// The pictures that arrived here for the app, oldest first, named by the person whose device
    /// sent them.
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
                // A device the callers do not name a person for: the app has nobody to draw its
                // picture under.
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

    /// Play `member` (a person of the call) at `percent` of as they arrive, on this device only:
    /// what the person set for them here, never what anyone else's call changes.
    pub(crate) fn set_volume(&self, member: &str, percent: u16) {
        let mut volumes = self.volumes.lock_recover();
        if percent == u16::from(runtime_collab::native_media::MAX_VOLUME_PERCENT) / 2 {
            volumes.remove(member);
        } else {
            volumes.insert(member.to_owned(), percent);
        }
        drop(volumes);
        self.apply_volumes();
    }

    /// How loud this device plays each member it was set, sorted by person: what the view's
    /// volume controls show.
    pub(crate) fn volumes(&self) -> Vec<(String, u16)> {
        self.volumes
            .lock_recover()
            .iter()
            .map(|(person, percent)| (person.clone(), *percent))
            .collect()
    }

    /// Tell the call's playout how loud to play each device: each device this call's channel
    /// says is a person's plays at that person's volume, and everyone else as they arrive.
    fn apply_volumes(&self) {
        let people = self
            .presence
            .people
            .lock_recover()
            .get(&self.call)
            .map(|people| people.by_device.clone())
            .unwrap_or_default();
        let volumes = self.volumes.lock_recover();
        let mut wanted = BTreeMap::new();
        for (device, person) in people {
            if let Some(&percent) = volumes.get(&person)
                && percent != 100
            {
                wanted.insert(hex(&device), percent);
            }
        }
        drop(volumes);
        let mut sent = self.volumes_sent.lock_recover();
        if *sent == wanted {
            return;
        }
        *sent = wanted.clone();
        self.side.lock_recover().voice.set_volumes(wanted);
    }

    /// Move the pictures once: the desktop the pass just drew goes to the encoder, and a locked
    /// screen's last picture is forgotten rather than sent again.
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
        if let Some(frame) = pictures::even_rgba(width, height, Arc::from(rgba)) {
            sending.publish(frame);
        }
    }

    fn close(&self) {
        if self.closed.swap(true, Ordering::AcqRel) {
            return;
        }
        let mut side = self.side.lock_recover();
        side.camera = None;
        // The desktop's share is the session's, which this call stops only if it was the one
        // sharing it.
        let shared_screen = side.screen.take().is_some();
        side.voice.close();
        drop(side);
        if shared_screen {
            self.screen.set_sharing(false);
        }
        self.video.close();
    }

    fn closed(&self) -> bool {
        self.closed.load(Ordering::Acquire)
    }

    /// Move the call's audio once: publish what the microphone captured on the workspace's live session,
    /// and play what the call's devices published there.
    /// The live session carries one voice track per device per workspace, so what it heard is played only
    /// from the devices this call's channel says are in it. A deafened call drops the frames rather
    /// than holding them.
    fn pump(&self) {
        // Who plays how loud may have moved: a person's volume, or the devices they are heard
        // from joining the call.
        self.apply_volumes();
        let now = Instant::now();
        let deafened = self.deafened.load(Ordering::Acquire);
        let heard = if deafened {
            Vec::new()
        } else {
            self.presence.took(&self.workspace)
        };
        let mut speaking: Vec<[u8; 32]> = Vec::new();
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
                // A bridge carries the voice of the people it brings into the
                // call: the frame names whose it is, and only a bridged person
                // the call lets speak is played.
                let speaker = object.speaker.unwrap_or(sender);
                if speaker != sender && !self.presence.bridged_may_speak(&self.call, &speaker) {
                    continue;
                }
                // A stream that starts over — the device left the call and
                // came back, or its page reloaded — is a new stream: what was
                // heard from it before says nothing about this frame's place,
                // so the playout starts over with it rather than holding it
                // behind where the last one got.
                let restarted = {
                    let mut streams = self.streams.lock_recover();
                    let restarted = streams
                        .get(&speaker)
                        .is_some_and(|last| object.timestamp < *last);
                    streams.insert(speaker, object.timestamp);
                    restarted
                };
                if restarted {
                    side.voice.forget(speaker);
                }
                if side
                    .voice
                    .receive(
                        speaker,
                        object.sequence,
                        object.timestamp,
                        object.payload,
                        now,
                    )
                    .is_err()
                {
                    break;
                }
                speaking.push(speaker);
            }
            drop(side);
        }
        if !speaking.is_empty() {
            let mut held = self.speaking.lock_recover();
            held.extend(speaking.into_iter().map(|speaker| (speaker, now)));
        }
        let mut side = self.side.lock_recover();
        let captured = match side.voice.pump(now).map_err(voice_issue) {
            Ok(captured) => captured,
            Err(issue) => {
                // The person's microphone stopped: capture is off from now on, and the call says
                // why. Its place in the stream stays where it is, so starting capture again is
                // heard at once.
                *self.issue.lock_recover() = Some(issue);
                return;
            }
        };
        let live_session = self.presence.live_session(&self.workspace);
        if let Some(live_session) = live_session {
            for frame in captured {
                let sequence = self.presence.next_audio(&self.workspace);
                let Ok(body) = (VoiceObject {
                    sequence: u64::from(sequence),
                    timestamp: frame.timestamp_48k,
                    duration: voice::FRAME_SAMPLES,
                    speaker: None,
                    payload: frame.payload,
                })
                .encode() else {
                    continue;
                };
                if live_session.publish(Track::Audio, sequence, &body).is_err() {
                    // The live session ended: what follows is published when this session's pass joins it
                    // again.
                    break;
                }
            }
        }
        if let Err(issue) = side.voice.playout(now).map_err(voice_issue) {
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

    /// Re-read what the workspace admits and what the channel's keys are: a membership change
    /// reaches the call as it happens, and a channel whose keys were still on their way when the
    /// call joined brings it up.
    fn refresh(&self) {
        let roster = self
            .admitted
            .as_ref()
            .map(|admitted| LiveRoster::new(admitted(&self.workspace)));
        let (granted, _) = self.access.access(self.workspace_id);
        let keys = match granted {
            tilo_workspace_keys::KeyAccess::Ready(keys) => Some(keys),
            // Keys still on their way keep a joining call joining.
            tilo_workspace_keys::KeyAccess::Waiting => None,
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
                        current.roster = roster;
                    }
                }
                (Some(current), _) => {
                    if let Some(roster) =
                        roster.filter(|roster| roster.devices().ne(current.roster.devices()))
                    {
                        current.roster = roster;
                    }
                }
                (None, Some(keys)) => {
                    let roster = roster.unwrap_or_default();
                    *held = Some(Current { keys, roster });
                    self.live.store(true, Ordering::Release);
                }
                (None, None) => {}
            }
            drop(held);
        }
    }
}

/// Carry `shared` 's call: move what it carries until it ends, and draw the pictures the live session
/// heard the moment they arrive.
async fn run(shared: Arc<Shared>) {
    // The audio moves on the call's own clock, one frame's worth at a time;
    // a picture is drawn as soon as the live session heard it, never on a pass.
    let arrived = shared.presence.arrived();
    // Whether the call is live is the channel's keys to say, not this task's starting: a call whose
    // keys are still on their way joins the channel and says so until `refresh` brings it up.
    let mut beat = tokio::time::interval(PUMP);
    beat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut ticks = 0_u32;
    loop {
        if shared.closed() {
            break;
        }
        // Wait on the call's clock and on the live session in the same pass: the wake is registered before
        // the pictures are taken, so one that arrives while they are taken wakes the next pass
        // rather than the next tick.
        let arrived = arrived.notified();
        tokio::pin!(arrived);
        arrived.as_mut().enable();
        shared.pump();
        shared.pump_video();
        shared.pump_pictures();
        ticks = ticks.wrapping_add(1);
        if ticks.is_multiple_of(100) {
            shared.refresh();
        }
        tokio::select! {
            _ = beat.tick() => {}
            () = &mut arrived => {}
        }
    }
    shared.close();
    shared.unsubscribe_pictures();
}

/// The desktop this session shares: the pass that draws it hands it the frames, and every call
/// sharing it encodes what it took.
pub(crate) fn screen_share() -> &'static Arc<ScreenShare> {
    static SHARE: std::sync::OnceLock<Arc<ScreenShare>> = std::sync::OnceLock::new();
    SHARE.get_or_init(|| Arc::new(ScreenShare::new()))
}

/// When the screen a call shares next needs a frame of the shell; `None` while nothing is shared, or
/// a locked desktop withholds it.
pub(crate) fn next_screen_frame() -> Option<Instant> {
    screen_share().next_frame(Instant::now())
}

/// Hand the desktop this session shares the frame the pass just drew: the largest screen layer at
/// most, and never a locked screen.
pub(crate) fn share_screen(compositor: &mut tilo_compositor::Compositor) {
    let locked = compositor.locked();
    hand_screen(screen_share(), locked, || {
        crate::capture::video(compositor, SCREEN_MAX)
    });
}

/// Hand `share` what `draw` draws of the desktop, unless a locked desktop stops it first: locking
/// forgets the frame the share held, so nothing of the screen before the lock is ever sent, now or
/// after unlocking.
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

/// The layers of each source this device shows of `person` : nothing at all for a device whose
/// person the app shows nowhere.
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
mod tests;
