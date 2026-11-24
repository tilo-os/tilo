//! One device's live session in one scope: what it publishes, sealed
//! under the scope's channel content keys, and what it hears from the
//! scope's other devices.
//!
//! A [`LiveSession`] is one carrier session ([`crate::carrier`]) with one broadcast
//! of its own ([`crate::live::Namespace`] of this device) and one reader per
//! roster device and track. Every object it sends is sealed with
//! `Sealer`; every object it receives is opened with `Opener`, so what the
//! relay copies is opaque to the relay and a frame from a device the roster
//! does not name is refused before it is decrypted.
//!
//! Presence, the activity note and the share track are tracks every live session publishes.
//! A call's voice and pictures, a shared window and a window's page are not: a
//! call's audio, a camera layer, a screen layer, a window or a page track exists
//! only while a peer asks for it
//! ([`LiveSession::show`]), and a device encodes a layer or a window, or captures a
//! page, only while it is asked for ([`LiveSession::wanted_pictures`],
//! [`LiveSession::wanted_windows`], [`LiveSession::wanted_pages`]), so a picture, a window
//! or a page nobody watches costs no bitrate and no capture.
//!
//! A call's media names its call ([`crate::live::call_track`]): a device sends it, and reads it
//! of the others, only for the call its presence says it is in, sealed under that
//! call's keys and opened only from the devices its rule lets speak. A relay holds each call
//! track to the call's rule too ([`crate::relay::SessionAuthorization::call`]), whatever its
//! sender's presence says.
//!
//! The carrier is whatever [`Dial`] this device has: iroh to the device that
//! serves the workspace ([`crate::native::Iroh`]). This module does not name
//! it.
//!
//! A carrier goes away for reasons no workspace controls — a network dropped,
//! a machine slept — so a live session keeps the dial it joined with and opens
//! another carrier when its own goes ([`Dial::redials`]). While it is gone,
//! nobody it was reading is drawn as here: what a peer last said of itself is
//! heard again from the broadcast the next carrier carries.
//!
//! A live session reads at most [`MAX_HEARD`] devices at once. Every device it reads
//! is three subscriptions it and its relay hold for as long as the device is
//! there, and that, not the transport, is what a further device costs. A
//! device beyond the bound is not heard, and the live session says how many those
//! are ([`LiveSession::unheard`]): nobody is absent without a word.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::future::Future;
use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};
use std::time::Duration;

use crate::frame::{LiveKeys, Opener, Refusal, Sealer, Stream, TrackName};
use crate::page::{self, PageObject};
use crate::roster::Roster;
use crate::video::{self, Layer, Source, VideoObject};
use moq_net::origin::{Consumer, Producer};
use tilo_lock::MutexRecover as _;

use crate::Task;
use crate::admit::{AdmissionError, Connected, Dial, OriginTask, origin_config};
use crate::live::{self, Delivery, Namespace, Track, exact};
use crate::scope::Scope;
use crate::voice::{self, VoiceObject};

/// The tracks a live session reads of every device it hears: the workspace's presence, the voice
/// of the call it is in, the activity note and what a shared window's viewers and host say to each
/// other ([`crate::share`]). Presence, the activity note and the share track are published by
/// every live session; a call's voice, like a call's pictures ([`Track::Camera`],
/// [`Track::Screen`]), exists only while someone in the call asks for it.
pub const TRACKS: [Track; 4] = [Track::Presence, Track::Audio, Track::Activity, Track::Share];

/// Devices of a scope one live session reads at once: the ones it hears.
///
/// Each device read is one subscription per track of [`TRACKS`], held by this
/// live session and by its relay for as long as the device is in the scope. Measured
/// with every device of a live session and its relay in one process, each device a
/// device hears held about 250 KiB of them when a device was read on three tracks,
/// so what a live session costs grows with the square of its devices: 277 MiB for
/// thirty-three devices that all hear each other, a gibibyte for sixty-four and
/// 4.3 GiB for a hundred and twenty-eight, and the share track adds a fourth
/// subscription per device to that. Thirty-two keeps a relay, which is a member's own device,
/// within what it can hold, and is about what a session's transport let one
/// device read before this was a number anyone chose. Devices beyond it are in the scope, are read by
/// whoever has room for them, and are counted here as not heard
/// ([`LiveSession::unheard`]). The carrier allows an admitted connection the
/// streams this many devices take ([`crate::native::MAX_BI_STREAMS`]).
pub const MAX_HEARD: usize = 32;

/// Who of a scope's devices a live session reads now, and who it does not.
#[derive(Default)]
struct Hearing {
    /// The devices read, each with how many of its tracks' readers hold it:
    /// at most [`MAX_HEARD`] devices.
    held: BTreeMap<[u8; 32], usize>,
    /// The devices carried here that no reader holds: they wait for one to let go.
    beyond: BTreeSet<[u8; 32]>,
    /// Whether the presence readers read ([`LiveSession::hear_presence`]).
    presence: bool,
    /// The readers waiting for a hold, or for presence to be read.
    waiting: Vec<Waker>,
}

/// Objects one live session keeps before it drops the oldest: a jitter buffer, not a
/// backlog.
const QUEUE: usize = 256;
/// Pictures one received track keeps before it starts over at the next
/// keyframe: a decoder never sees a picture whose reference was dropped.
const PICTURES_QUEUED: usize = 8;
/// Join budget: the transport, the admission preface and the setup.
const JOINT: Duration = Duration::from_secs(10);
/// Bytes one live session's own publication may cache at its ingress.
const INGRESS: u64 = 2 * 1024 * 1024;
/// Bytes a live session caches of what it hears.
const EGRESS: u64 = 4 * 1024 * 1024;
/// Longest anything a live session publishes is retained by its own origin.
const RETAIN: Duration = Duration::from_secs(30);

/// Why a live session could not start, or stopped carrying something.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum LiveError {
    /// The relay refused the grant.
    #[error("the live session was not admitted")]
    Denied,
    /// The relay is admitting as many sessions as it takes at once and took
    /// nothing of this one: it judged nobody, so this says nothing about the
    /// grant, and asking again is the remedy.
    #[error("the live session's relay is admitting as many devices as it takes at once")]
    Busy,
    /// The carrier or its setup failed.
    #[error("the live session is unavailable")]
    Unavailable,
    /// A frame was not sealed or opened: no key for its epoch, a sender off
    /// the roster, or bytes that do not authenticate.
    #[error("the frame was not sealed or opened: {0}")]
    Refused(Refusal),
    /// The body is not an object of `track`'s profile: a `tilo-voice/1`
    /// object that a sealed payload still fits in, on the audio track.
    #[error("the object does not fit its track")]
    Invalid,
    /// Nobody shows the picture's layer, so nothing of it is encoded.
    #[error("no one shows this picture layer")]
    Unwanted,
    /// The picture cannot join its layer's group: the layer starts over at a
    /// keyframe.
    #[error("the picture layer needs a keyframe")]
    Unkeyed,
    /// The session has ended.
    #[error("the live session has ended")]
    Ended,
    /// This device is in no call, holds none of its call's keys yet, or its call does not let it
    /// speak ([`LiveSession::set_calls`]).
    #[error("the call does not let this device speak")]
    Withheld,
}

/// The call a live session is in: the call its own presence says, held to the call's rule.
struct InCall {
    /// The call's tag, as its tracks name it ([`collab_types::calls::tag`]).
    tag: String,
    /// The generation of the call's rule its keys are ([`collab_types::calls::media_document`]).
    generation: u64,
    /// The keys its media is sealed and opened under: its generation's, which only the devices
    /// the call admits are given.
    keys: LiveKeys,
    /// The devices the call lets speak: the only ones whose media is opened.
    roster: Roster,
    /// Whether the call lets this device speak.
    publish: bool,
    opener: Opener,
}

/// The keys of one call's media ([`LiveSession::set_call_keys`]): its channel, the generation of
/// its rule they are, and the keys.
type CallKeys = (String, u64, LiveKeys);

/// What the workspace's calls allow, and the person each device is ([`LiveSession::set_calls`]).
type CallRules = (
    BTreeMap<String, crate::calls::CallRule>,
    BTreeMap<[u8; 32], String>,
);

impl From<AdmissionError> for LiveError {
    fn from(error: AdmissionError) -> Self {
        match error {
            AdmissionError::Denied | AdmissionError::Invalid => Self::Denied,
            AdmissionError::Busy => Self::Busy,
            AdmissionError::Unavailable => Self::Unavailable,
        }
    }
}

impl From<Refusal> for LiveError {
    fn from(refusal: Refusal) -> Self {
        Self::Refused(refusal)
    }
}

/// What one live session joins: the scope, this device, the keys it seals and opens
/// under, and who may send in it.
#[derive(Clone)]
pub struct Join {
    pub scope: Scope,
    pub device: [u8; 32],
    /// The scope's channel content keys, or — for a device that publishes
    /// without them — its own media keys ([`LiveKeys`]).
    pub keys: LiveKeys,
    pub roster: Roster,
    /// Whether this device publishes its own broadcast, or only listens.
    pub publish: bool,
    /// Whether this live session reads its peers' presence from the start
    /// ([`LiveSession::hear_presence`]).
    pub presence: bool,
}

/// One object a live session heard, already opened.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Heard {
    /// The device that sealed it.
    pub sender: [u8; 32],
    /// Which of its tracks it came from.
    pub track: Track,
    /// The object's sequence on that track.
    pub sequence: u32,
    /// The opened body; empty for [`Heard::left`].
    pub body: Vec<u8>,
}

impl Heard {
    /// `sender`'s presence ended: its broadcast went away with the session that published it,
    /// however that session ended — it said it left, it closed, or the host lost it. No frame it
    /// sealed is empty, so a board takes this one as the sender leaving
    /// ([`crate::presence::Board::heard`]).
    #[must_use]
    pub const fn left(sender: [u8; 32]) -> Self {
        Self {
            sender,
            track: Track::Presence,
            sequence: 0,
            body: Vec::new(),
        }
    }
}

/// One track this live session publishes because a peer asked for it: a picture
/// layer or a window's page.
struct Demand {
    track: moq_net::track::Producer,
    group: Option<moq_net::group::Producer>,
    next_group: u64,
    in_group: u64,
    /// Ends the track once nobody reads it ([`watch_unused`]).
    _unused: Task,
}

impl Demand {
    /// Write one object of the track: the object that starts a group — a
    /// picture's keyframe, a page's whole frame — opens the group.
    fn write(
        &mut self,
        keyframe: bool,
        timestamp: moq_net::Timestamp,
        bytes: Vec<u8>,
    ) -> Result<(), LiveError> {
        if keyframe {
            if let Some(group) = self.group.take() {
                let _ = group.finish();
            }
            self.group = Some(
                self.track
                    .create_group(moq_net::group::Info::from(self.next_group))
                    .map_err(|_| LiveError::Unkeyed)?,
            );
            self.next_group = self.next_group.saturating_add(1);
            self.in_group = 0;
        }
        if self.in_group >= video::MAX_GROUP_OBJECTS {
            self.group = None;
            return Err(LiveError::Unkeyed);
        }
        let group = self.group.as_mut().ok_or(LiveError::Unkeyed)?;
        group
            .write_frame(timestamp, bytes)
            .map_err(|_| LiveError::Unkeyed)?;
        self.in_group = self.in_group.saturating_add(1);
        Ok(())
    }
}

/// The tracks of one live session its readers asked for, as they asked.
#[derive(Default)]
struct Demands {
    /// The tracks someone asked this device for, by track name: exactly what
    /// it encodes ([`LiveSession::wanted_pictures`]).
    out: BTreeMap<String, Demand>,
    /// Where a track asked for again numbers its groups: one past the last
    /// group of its earlier acceptance, so a subscriber that asks again is
    /// never sent a group it already read.
    next_group: BTreeMap<String, u64>,
    /// Tracks whose reader needs a keyframe: they were asked for afresh, or
    /// a picture could not join its group.
    keyframes: BTreeSet<Track>,
    /// Received tracks that dropped a picture: nothing of them is delivered
    /// until a keyframe.
    behind: BTreeSet<([u8; 32], Track)>,
}

/// What a live session shares with its readers.
pub(crate) struct Shared {
    pub(crate) join: Join,
    /// What this live session publishes into, and what its carrier is subscribed out of: the same two the
    /// carrier was opened with, for a carrier dialled again after the last one went
    /// ([`LiveSession`]'s).
    outgoing: Producer,
    incoming: Producer,
    opener: Mutex<Opener>,
    /// The keys `publish` seals under: the join's, until the channel's epoch
    /// moves ([`LiveSession::set_keys`]).
    keys: Mutex<LiveKeys>,
    roster: Mutex<Roster>,
    /// How many readers each device the roster named has running now: a
    /// device is read by exactly one set of them.
    reading: Mutex<BTreeSet<[u8; 32]>>,
    /// Which of the devices carried here those readers read, and which they
    /// do not ([`LiveSession::unheard`]).
    hearing: Mutex<Hearing>,
    /// Told whenever [`LiveSession::unheard`] says otherwise.
    unheard_hook: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
    published: Mutex<std::collections::BTreeMap<String, moq_net::track::Producer>>,
    /// This device's broadcast, kept for as long as the live session lives: dropping
    /// it withdraws the announcement its subscribers are reading.
    _broadcast: Option<moq_net::broadcast::Producer>,
    /// The peers' requests for picture tracks of this device's broadcast,
    /// taken once by the live session's acceptor.
    picture_requests: Mutex<Option<moq_net::broadcast::Dynamic>>,
    /// The picture layers someone shows of this device, and the groups they
    /// are written in.
    demands: Mutex<Demands>,
    /// Advances whenever a demanded track is asked for or ends, or an object
    /// could not join its group: what a publisher reads to encode a picture or
    /// capture a page.
    demand_revision: AtomicU64,
    heard: Mutex<VecDeque<Heard>>,
    /// How many objects this live session opened from its peers, and why it could not
    /// open what a reader got: the answer a member needs when it hears nobody
    /// who is in the roster.
    heard_total: AtomicU64,
    /// Per device, how many presence objects were opened from it and how many times it was
    /// heard leaving: whether a member that lists nobody hears that device at all.
    presence_from: Mutex<BTreeMap<[u8; 32], (u64, u64)>>,
    refusals: Mutex<BTreeMap<String, usize>>,
    ended: Mutex<bool>,
    /// What the workspace's calls allow, and the person each device is.
    call_rules: Mutex<CallRules>,
    /// The channel this device's own presence says it is in.
    own_call: Mutex<Option<String>>,
    /// The keys of the call media this device was handed last.
    call_keys: Mutex<Option<CallKeys>>,
    /// The call this device is in, held to its rule: what call media it sends and reads.
    call: Mutex<Option<InCall>>,
    /// Advances whenever the call moves: a reader of call media reads again under the call's name.
    call_revision: AtomicU64,
    wake: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
    /// Told once, when the live session ends: what rejoins the scope elsewhere.
    ended_hook: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
}

impl Shared {
    /// The one live session of one device in one scope, with its own broadcast
    /// announced in `outgoing`.
    fn new(join: &Join, outgoing: &Producer, incoming: &Producer) -> Result<Arc<Self>, LiveError> {
        // A live session whose keys cannot seal this device's presence is not one
        // this device may join: the scope's keys are its membership.
        if join.publish {
            let name = live::track(Track::Presence);
            Sealer::new(
                &join.keys,
                join.device,
                stream(Track::Presence),
                TrackName::new(&join.scope, name.as_str()),
            )?;
        }
        let (broadcast, published, requests) = if join.publish {
            let path = Namespace::new(&join.scope, join.device).path();
            let broadcast = outgoing
                .create_broadcast(path.as_str())
                .map_err(|_| LiveError::Unavailable)?;
            // Every track exists before the broadcast is announced: a relay
            // copies the set it finds announced, and a track that appeared
            // later would never be copied to the subscribers already there.
            let mut published = std::collections::BTreeMap::new();
            // A call's voice names its call, and is sent only while someone in it asks for it.
            for track in TRACKS
                .into_iter()
                .filter(|track| !live::is_call_media(*track))
            {
                let name = live::track(track);
                let producer = broadcast
                    .create_track(name.as_str(), track_info(track))
                    .map_err(|_| LiveError::Unavailable)?;
                published.insert(name, producer);
            }
            // Taken before the announcement, like the relay's own forwarding:
            // a request that arrives with the announcement is not one to miss.
            let requests = broadcast.dynamic();
            broadcast
                .announce(moq_net::origin::Route::default())
                .map_err(|_| LiveError::Unavailable)?;
            (Some(broadcast), published, Some(requests))
        } else {
            (None, std::collections::BTreeMap::new(), None)
        };
        Ok(Arc::new(Self {
            opener: Mutex::new(Opener::new(join.keys.clone(), join.roster.clone())),
            keys: Mutex::new(join.keys.clone()),
            roster: Mutex::new(join.roster.clone()),
            reading: Mutex::new(BTreeSet::new()),
            hearing: Mutex::new(Hearing {
                presence: join.presence,
                ..Hearing::default()
            }),
            unheard_hook: Mutex::new(None),
            join: join.clone(),
            outgoing: outgoing.clone(),
            incoming: incoming.clone(),
            published: Mutex::new(published),
            _broadcast: broadcast,
            picture_requests: Mutex::new(requests),
            demands: Mutex::new(Demands::default()),
            demand_revision: AtomicU64::new(0),
            heard: Mutex::new(VecDeque::new()),
            heard_total: AtomicU64::new(0),
            presence_from: Mutex::new(BTreeMap::new()),
            refusals: Mutex::new(BTreeMap::new()),
            ended: Mutex::new(false),
            call_rules: Mutex::default(),
            own_call: Mutex::new(None),
            call_keys: Mutex::new(None),
            call: Mutex::new(None),
            call_revision: AtomicU64::new(0),
            wake: Mutex::new(None),
            ended_hook: Mutex::new(None),
        }))
    }

    /// What the carrier publishes: exactly this device's own broadcast.
    fn publication(&self) -> Option<Consumer> {
        if !self.join.publish {
            return None;
        }
        let path = Namespace::new(&self.join.scope, self.join.device).path();
        exact(&[path.as_str()]).and_then(|names| self.outgoing.consume().scope("", &names).ok())
    }

    /// What the carrier subscribes to: every device of this scope.
    fn subscription(&self) -> Result<Option<Producer>, LiveError> {
        let prefix = live::devices(&self.join.scope);
        let pattern =
            moq_net::Pattern::subtree(prefix.as_str()).map_err(|_| LiveError::Unavailable)?;
        let scoped = self
            .incoming
            .scope("", &moq_net::Patterns::from(pattern))
            .map_err(|_| LiveError::Unavailable)?;
        Ok(Some(scoped))
    }

    pub(crate) fn ended(&self) -> bool {
        *self.ended.lock_recover()
    }
    pub(crate) fn end(&self) {
        let first = {
            let mut ended = self.ended.lock_recover();
            !std::mem::replace(&mut *ended, true)
        };
        if first {
            self.forget_readers();
            let hook = self.ended_hook.lock_recover().clone();
            if let Some(hook) = hook {
                hook();
            }
            self.wake();
            // A reader waiting for a hold has nothing left to wait for.
            self.wake_waiting();
        }
    }
    /// How many devices are carried here that this live session does not hear.
    fn unheard(&self) -> usize {
        self.hearing.lock_recover().beyond.len()
    }
    /// Tell whoever reads [`LiveSession::unheard`] when it says otherwise than
    /// `before`.
    fn unheard_moved(&self, before: usize) {
        if self.unheard() == before {
            return;
        }
        let hook = self.unheard_hook.lock_recover().clone();
        if let Some(hook) = hook {
            hook();
        }
    }
    /// Take a hold for `device`, whose broadcast is carried here: at once
    /// when its other tracks' readers hold one or one is free, and otherwise
    /// when one is given back, until when the device counts as beyond what
    /// this live session reads.
    fn poll_hold(&self, device: [u8; 32], cx: &Context<'_>) -> Poll<()> {
        let mut hearing = self.hearing.lock_recover();
        let before = hearing.beyond.len();
        if let Some(readers) = hearing.held.get_mut(&device) {
            *readers = readers.saturating_add(1);
            return Poll::Ready(());
        }
        let held = hearing.held.len() < MAX_HEARD;
        if held {
            hearing.held.insert(device, 1);
            hearing.beyond.remove(&device);
        } else {
            hearing.beyond.insert(device);
            if !hearing
                .waiting
                .iter()
                .any(|waiting| waiting.will_wake(cx.waker()))
            {
                hearing.waiting.push(cx.waker().clone());
            }
        }
        drop(hearing);
        self.unheard_moved(before);
        if held { Poll::Ready(()) } else { Poll::Pending }
    }
    fn hears_presence(&self) -> bool {
        self.hearing.lock_recover().presence
    }
    /// Ready once the presence readers read as `on` says, or `device` has
    /// nothing left to be read for.
    fn poll_presence(&self, device: [u8; 32], on: bool, cx: &Context<'_>) -> Poll<()> {
        if self.ended() || !self.admits(device) {
            return Poll::Ready(());
        }
        let mut hearing = self.hearing.lock_recover();
        if hearing.presence == on {
            return Poll::Ready(());
        }
        if !hearing
            .waiting
            .iter()
            .any(|waiting| waiting.will_wake(cx.waker()))
        {
            hearing.waiting.push(cx.waker().clone());
        }
        Poll::Pending
    }
    /// One reader let go of `device`'s hold: with the last of them the hold
    /// is free, and whoever waits for one is told.
    fn leave_hold(&self, device: [u8; 32]) {
        let mut hearing = self.hearing.lock_recover();
        let left = hearing.held.get_mut(&device).map(|readers| {
            *readers = readers.saturating_sub(1);
            *readers
        });
        let freed = left == Some(0);
        if freed {
            hearing.held.remove(&device);
        }
        drop(hearing);
        if freed {
            self.wake_waiting();
        }
    }
    /// `device`, which waited for a hold, is carried here no longer, or is
    /// not read any more: it is not one this live session does not hear.
    fn stops_waiting(&self, device: [u8; 32]) {
        let before = {
            let mut hearing = self.hearing.lock_recover();
            let before = hearing.beyond.len();
            hearing.beyond.remove(&device);
            before
        };
        self.unheard_moved(before);
    }
    /// Have every reader that waits for a hold look again.
    fn wake_waiting(&self) {
        let waiting = std::mem::take(&mut self.hearing.lock_recover().waiting);
        for waiting in waiting {
            waiting.wake();
        }
    }
    /// Nothing this live session read is heard here any more: every device it was reading has left, as
    /// whoever draws them sees it. Said when the carrier goes and when the live session ends, so a member
    /// whose connection dropped is drawn away at once instead of where they last were, and is drawn
    /// again from the next broadcast of them that is read.
    fn forget_readers(&self) {
        let reading: Vec<[u8; 32]> = self.reading.lock_recover().iter().copied().collect();
        for device in reading {
            self.deliver(Heard::left(device));
        }
    }
    pub(crate) fn wake(&self) {
        let wake = self.wake.lock_recover().clone();
        if let Some(wake) = wake {
            wake();
        }
    }
    fn deliver(&self, heard: Heard) {
        if heard.track == Track::Presence {
            let left = heard.body.is_empty();
            let mut from = self.presence_from.lock_recover();
            let counts = from.entry(heard.sender).or_default();
            if left {
                counts.1 = counts.1.saturating_add(1);
            } else {
                counts.0 = counts.0.saturating_add(1);
            }
            drop(from);
        }
        {
            let mut queue = self.heard.lock_recover();
            if queue.len() >= QUEUE {
                queue.pop_front();
            }
            queue.push_back(heard);
        }
        self.heard_total.fetch_add(1, Ordering::Relaxed);
        self.wake();
    }
    /// The name the call media `track` of `device` is read under now, and the call's revision it
    /// names: `None` while this device is in no call or its call does not let `device` speak.
    fn call_name(&self, track: Track, device: [u8; 32]) -> Option<(String, u64)> {
        let revision = self.call_revision.load(Ordering::Acquire);
        let name = self
            .call
            .lock_recover()
            .as_ref()
            .filter(|call| call.roster.admits(device))
            .map(|call| live::call_track(track, &call.tag))?;
        Some((name, revision))
    }

    /// Ready once the call moved past `revision`, or the live session ended.
    fn poll_call_moved(&self, revision: u64, cx: &Context<'_>) -> Poll<()> {
        let mut hearing = self.hearing.lock_recover();
        if self.ended() || self.call_revision.load(Ordering::Acquire) != revision {
            return Poll::Ready(());
        }
        if !hearing
            .waiting
            .iter()
            .any(|waiting| waiting.will_wake(cx.waker()))
        {
            hearing.waiting.push(cx.waker().clone());
        }
        Poll::Pending
    }

    /// What this device's call media on `track` is sealed with and named now: its call's keys
    /// and the call's name for the track ([`LiveError::Withheld`] outside a call, before its keys
    /// are here, or where it may not speak).
    fn call_sealer(&self, track: Track) -> Result<(Sealer, String), LiveError> {
        let (keys, name) = self
            .call
            .lock_recover()
            .as_ref()
            .filter(|call| call.publish)
            .map(|call| (call.keys.clone(), live::call_track(track, &call.tag)))
            .ok_or(LiveError::Withheld)?;
        let sealer = Sealer::new(
            &keys,
            self.join.device,
            stream(track),
            TrackName::new(&self.join.scope, name.as_str()),
        )?;
        Ok((sealer, name))
    }

    /// The call media keys this device needs now: the channel its presence says it is in, and
    /// the generation of that call's rule.
    fn call_keys_wanted(&self) -> Option<(String, u64)> {
        let channel = self.own_call.lock_recover().clone()?;
        let generation = self
            .call_rules
            .lock_recover()
            .0
            .get(&channel)
            .map_or(0, |rule| rule.generation);
        Some((channel, generation))
    }

    /// Hold the call this device's presence says to the call's rule, under the call keys and
    /// roster it has now: in no call until it holds the keys of the rule's generation, so
    /// nothing is sealed under the keys of one a change left behind. A call that moved to another
    /// channel or other keys is read again under them, and what was asked of this device in
    /// another call ends.
    fn settle_call(&self) {
        let wanted = self.call_keys_wanted();
        let next = wanted.and_then(|(channel, generation)| {
            let keys = self
                .call_keys
                .lock_recover()
                .as_ref()
                .filter(|(held, at, _)| *held == channel && *at == generation)
                .map(|(_, _, keys)| keys.clone())?;
            let (rules, persons) = &*self.call_rules.lock_recover();
            let devices: Vec<[u8; 32]> = self.roster.lock_recover().devices().collect();
            let roster = crate::calls::speakers(&channel, rules, persons, devices);
            Some(InCall {
                tag: collab_types::calls::tag(&channel),
                generation,
                opener: Opener::new(keys.clone(), roster.clone()),
                keys,
                roster,
                publish: crate::calls::may(&channel, rules, persons, self.join.device).publish,
            })
        });
        let tag = next.as_ref().map(|call| call.tag.clone());
        let moved = {
            let mut call = self.call.lock_recover();
            let moved = match (&*call, &next) {
                (Some(held), Some(next)) => {
                    held.tag != next.tag
                        || held.generation != next.generation
                        || held.keys.epoch() != next.keys.epoch()
                        || held.roster != next.roster
                        || held.publish != next.publish
                }
                (None, None) => false,
                _ => true,
            };
            *call = next;
            moved
        };
        if !moved {
            return;
        }
        // What was asked of this device in a call it is not in ends with it.
        self.demands.lock_recover().out.retain(|name, _| {
            live::parse_call_track(name).is_none_or(|(_, call)| Some(call) == tag.as_deref())
        });
        self.call_revision.fetch_add(1, Ordering::AcqRel);
        self.wake_waiting();
        self.demand_moved();
    }

    /// Whether this device is in the call `tag` names.
    fn in_call(&self, tag: &str) -> bool {
        self.call
            .lock_recover()
            .as_ref()
            .is_some_and(|call| call.tag == tag)
    }
    /// One frame a reader got and this live session could not open, by why.
    fn refused(&self, refusal: &Refusal) {
        self.refusals
            .lock_recover()
            .entry(refusal.to_string())
            .and_modify(|count| *count = count.saturating_add(1))
            .or_insert(1);
    }
    /// Queue one received picture, or start its track over at the next
    /// keyframe when its reader fell behind: a decoder is never handed a
    /// picture whose reference was dropped.
    fn deliver_picture(&self, heard: Heard) {
        let key = (heard.sender, heard.track);
        let keyframe = VideoObject::decode(&heard.body).is_ok_and(|object| object.keyframe);
        let mut demands = self.demands.lock_recover();
        if demands.behind.contains(&key) {
            if !keyframe {
                return;
            }
            demands.behind.remove(&key);
        }
        let queued = self
            .heard
            .lock_recover()
            .iter()
            .filter(|object| (object.sender, object.track) == key)
            .count();
        if queued >= PICTURES_QUEUED {
            self.heard
                .lock_recover()
                .retain(|object| (object.sender, object.track) != key);
            if !keyframe {
                demands.behind.insert(key);
                return;
            }
        }
        drop(demands);
        self.deliver(heard);
    }
    /// A picture track someone asked for, or one this live session no longer reads,
    /// or one that needs a keyframe: the encoder reads it.
    fn demand_moved(&self) {
        self.demand_revision.fetch_add(1, Ordering::Relaxed);
        self.wake();
    }
    /// The peers' requests for this device's picture tracks, taken once.
    fn take_demands(&self) -> Option<moq_net::broadcast::Dynamic> {
        self.picture_requests.lock_recover().take()
    }
    /// Would `device`'s frame be opened now? Decided by the roster and the
    /// keys, before any byte of it is read.
    fn admits(&self, device: [u8; 32]) -> bool {
        let roster = self.roster.lock_recover();
        roster.admits(device)
    }
    /// Take the readers' hold on `device`: `false` when they already read it.
    fn claim(&self, device: [u8; 32]) -> bool {
        self.reading.lock_recover().insert(device)
    }
    /// Let go of the readers' hold on `device`.
    fn release(&self, device: [u8; 32]) {
        self.reading.lock_recover().remove(&device);
    }
    /// Open `sender`'s `frame` of `track`, which arrived under `name`: a call's media under its
    /// call's keys, from the devices the call lets speak, and nothing else under them.
    fn open(
        &self,
        sender: [u8; 32],
        track: Track,
        name: &str,
        frame: &[u8],
    ) -> Result<Vec<u8>, Refusal> {
        let sealed_as = TrackName::new(&self.join.scope, name);
        if live::is_call_media(track) {
            let call = self.call.lock_recover();
            let opened = call
                .as_ref()
                .filter(|call| live::call_track(track, &call.tag) == name)
                .map(|call| call.opener.open(sender, stream(track), &sealed_as, frame));
            drop(call);
            return opened.unwrap_or(Err(Refusal::NoKey(0)));
        }
        let opener = self.opener.lock_recover();
        opener.open(sender, stream(track), &sealed_as, frame)
    }

    /// The body of one object `sender` published on its share track, when it is addressed to
    /// this device ([`crate::share::open`]).
    fn open_share(&self, sender: [u8; 32], object: &[u8]) -> Result<Option<Vec<u8>>, Refusal> {
        let keys = self.keys.lock_recover().clone();
        let roster = self.roster.lock_recover().clone();
        crate::share::open(
            (&keys, &roster),
            &self.join.scope,
            self.join.device,
            sender,
            object,
        )
    }
}

/// Start one reader per track for every device the roster names that no
/// reader holds yet. A reader lets go of its device when it ends, so a device
/// that leaves the roster and comes back is read again.
fn read_roster(shared: &Arc<Shared>, readers: &mut Vec<Task>) {
    let devices: Vec<[u8; 32]> = shared.roster.lock_recover().devices().collect();
    for device in devices {
        if device == shared.join.device || !shared.claim(device) {
            continue;
        }
        let guard = Arc::new(ReadGuard(shared.clone(), device));
        for track in TRACKS {
            let (guard, shared) = (Arc::clone(&guard), shared.clone());
            readers.push(Task::spawn(async move {
                let _held = guard;
                receive(shared, device, track).await;
            }));
        }
    }
}

/// Holds one device while its readers run, shared by them: the last one to
/// end releases the device, so the next roster that names it starts readers
/// again.
struct ReadGuard(Arc<Shared>, [u8; 32]);

impl Drop for ReadGuard {
    fn drop(&mut self) {
        self.0.release(self.1);
    }
}

/// One reader's hold on its device ([`Shared::poll_hold`]), let go when the
/// reader stops reading the broadcast it took it for.
struct Hold(Arc<Shared>, [u8; 32]);

impl Drop for Hold {
    fn drop(&mut self) {
        self.0.leave_hold(self.1);
    }
}

/// Take a hold for `device`, whose `broadcast` is carried here, waiting for
/// one while the live session reads as many devices as it reads at once: `None`
/// when the broadcast went away first, the live session ended or the roster names
/// the device no longer.
async fn hold(
    shared: &Arc<Shared>,
    device: [u8; 32],
    broadcast: &moq_net::broadcast::Consumer,
) -> Option<Hold> {
    let mut gone = std::pin::pin!(broadcast.closed());
    let held = std::future::poll_fn(|cx| {
        if shared.ended() || !shared.admits(device) || gone.as_mut().poll(cx).is_ready() {
            return Poll::Ready(false);
        }
        shared.poll_hold(device, cx).map(|()| true)
    })
    .await;
    if held {
        Some(Hold(Arc::clone(shared), device))
    } else {
        shared.stops_waiting(device);
        None
    }
}

/// A wait a device takes up to a quarter off, by its own key and `turn`: the
/// same device waits the same, and two devices almost never do.
///
/// Devices that missed a relay in the same moment (it was away and came back,
/// or it had no room for them) would otherwise all ask it again in the same
/// moment, every time, and meet its bound together again. A quarter keeps the
/// schedule's shape: each wait is still longer than the one before it, and
/// none is longer than the schedule says.
#[must_use]
pub fn spread(wait: Duration, device: [u8; 32], turn: u64) -> Duration {
    let mut hasher = std::hash::DefaultHasher::new();
    device.hash(&mut hasher);
    turn.hash(&mut hasher);
    // A thousand and twenty-four steps of the quarter.
    let step = u32::try_from(hasher.finish() % 1024).unwrap_or(0);
    let quarter = wait.checked_div(4).unwrap_or_default();
    let off = quarter
        .saturating_mul(step)
        .checked_div(1024)
        .unwrap_or_default();
    wait.saturating_sub(off)
}

/// The first wait before a carrier that went away is dialled again, and the longest: a network that
/// dropped is usually back within a second, and one that is gone for longer is dialled now and then
/// rather than never.
const REDIAL_FIRST: Duration = Duration::from_millis(250);
const REDIAL_LONGEST: Duration = Duration::from_secs(8);

/// How long to wait before the `attempt`th redial after a carrier went away: the wait doubles with
/// every attempt that found no relay, up to [`REDIAL_LONGEST`]. A device takes up to a quarter off
/// each ([`spread`]), so the live sessions a relay dropped together do not dial it together.
fn redial_wait(attempt: u32) -> Duration {
    let doublings = attempt.saturating_sub(1).min(8);
    REDIAL_FIRST
        .saturating_mul(1_u32 << doublings)
        .min(REDIAL_LONGEST)
}

/// Watch the carrier the live session runs on, and dial another when it goes away.
///
/// A connection drops for reasons nobody in a workspace controls — a Wi-Fi network moved, a laptop
/// slept, a peer restarted — and the session it carried is what says where everyone is and what a
/// call sounds like. So the live session keeps its session and its dial: the carrier is opened again, with
/// the same grant and the same broadcast, while nothing it had read is drawn as here
/// ([`Shared::forget_readers`]) and is drawn again from the next word that arrives.
///
/// The live session ends instead where no carrier can be opened again: a dial that hands over a carrier
/// once ([`Dial::redials`]), or a relay that will not admit this device now. What holds the live session
/// then joins it again, with the keys and membership it has by then.
async fn reconnect(
    shared: Arc<Shared>,
    dial: Arc<dyn Dial>,
    grant: Arc<Vec<u8>>,
    carrier: Arc<Mutex<Option<Connected>>>,
) {
    let mut attempt: u32;
    loop {
        let Some(session) = carrier
            .lock_recover()
            .as_ref()
            .map(|held| held.session.clone())
        else {
            return;
        };
        let _ = session.closed().await;
        if shared.ended() {
            return;
        }
        shared.forget_readers();
        if !dial.redials() {
            shared.end();
            return;
        }
        // Each carrier that goes away is dialled for afresh: the wait starts over at
        // [`REDIAL_FIRST`], so a network that flickers recovers at once.
        attempt = 0;
        loop {
            attempt = attempt.saturating_add(1);
            let wait = spread(redial_wait(attempt), shared.join.device, u64::from(attempt));
            n0_future::time::sleep(wait).await;
            if shared.ended() {
                return;
            }
            let Ok(subscribe) = shared.subscription() else {
                shared.end();
                return;
            };
            let publish = shared.publication();
            match dial.dial(grant.as_slice(), publish, subscribe, JOINT).await {
                Ok(connected) => {
                    *carrier.lock_recover() = Some(connected);
                    break;
                }
                // The relay will not admit this device now, or this dial has no carrier left to
                // hand over: the live session is over.
                Err(AdmissionError::Denied | AdmissionError::Invalid) => {
                    shared.end();
                    return;
                }
                // The relay is not there yet, or is admitting as many sessions as it takes at
                // once and judged nobody: it is dialled again, waiting longer each time.
                Err(AdmissionError::Unavailable | AdmissionError::Busy) => {}
            }
        }
    }
}

/// One device's sealed session in one scope.
pub struct LiveSession {
    shared: Arc<Shared>,
    /// This device's recent audio objects, newest last: the redundancy its
    /// datagrams carry, so one lost packet costs one frame and not two. Kept
    /// for one move of the call ([`Shared::call_revision`]): what was sealed
    /// before the call moved is never sent again after it.
    sent: Mutex<(u64, VecDeque<Vec<u8>>)>,
    /// The peer tracks this live session reads now: exactly what its app
    /// shows ([`Self::show`]), one reader each.
    shown: Mutex<BTreeMap<([u8; 32], Track), Task>>,
    /// One reader per device of the roster, by track: started when the device
    /// joins the roster, ended by itself once the roster no longer admits it.
    readers: Mutex<Vec<Task>>,
    /// The sequence of the last object this live session said on its share track.
    share_sequence: std::sync::atomic::AtomicU32,
    _demands: Vec<Task>,
    _origins: Vec<OriginTask>,
    /// The carrier this live session runs on now ([`reconnect`] replaces it), and the task watching it.
    _carrier: Option<Carrier>,
}

/// The carrier a live session runs on, and the task that dials another when this one goes.
struct Carrier {
    /// Kept alive here, and swapped by the watch: dropping this closes the session.
    _connected: Arc<Mutex<Option<Connected>>>,
    _watch: Task,
}

impl LiveSession {
    /// Join `join`'s scope through `dial`, admitted with `grant`.
    ///
    /// # Errors
    ///
    /// [`LiveError::Denied`] when the relay refuses the grant,
    /// [`LiveError::Busy`] when the relay took nothing of the session because
    /// it is admitting as many as it takes at once,
    /// [`LiveError::Unavailable`] when the carrier or its setup fails, and
    /// [`LiveError::Refused`] when the scope's keys do not cover this
    /// device's own presence.
    pub async fn join(dial: Arc<dyn Dial>, grant: &[u8], join: Join) -> Result<Self, LiveError> {
        Self::join_within(dial, grant, join, JOINT).await
    }

    /// Join as [`Self::join`], waiting at most `within` for the carrier and
    /// the admission: a device trying the relays of a scope in turn spends no
    /// more than this on one that is not there.
    ///
    /// The dial is kept: a carrier that goes away mid-session — a network that
    /// dropped, a laptop that slept — is dialled again ([`Dial::redials`]), and
    /// the live session carries the same session over it, so what this device says and
    /// hears survives the gap.
    ///
    /// # Errors
    ///
    /// As [`Self::join`].
    pub async fn join_within(
        dial: Arc<dyn Dial>,
        grant: &[u8],
        join: Join,
        within: Duration,
    ) -> Result<Self, LiveError> {
        let (outgoing, outgoing_task) = OriginTask::produce(origin_config(INGRESS, RETAIN));
        let (incoming, incoming_task) = OriginTask::produce(origin_config(EGRESS, RETAIN));
        let shared = Shared::new(&join, &outgoing, &incoming)?;
        let publish = shared.publication();
        let subscribe = shared.subscription()?;
        let connected = dial.dial(grant, publish, subscribe, within).await?;
        let carrier = Arc::new(Mutex::new(Some(connected)));
        let watch = Task::spawn(reconnect(
            shared.clone(),
            Arc::clone(&dial),
            Arc::new(grant.to_vec()),
            Arc::clone(&carrier),
        ));
        Ok(Self::serve(
            shared,
            vec![outgoing_task, incoming_task],
            Some(Carrier {
                _connected: carrier,
                _watch: watch,
            }),
        ))
    }

    /// Join `join`'s scope on the relay this device itself serves: no
    /// connection to itself, and the same sealed objects, published into and
    /// heard from the relay's shared origin ([`crate::relay::Relay::shared`]).
    ///
    /// # Errors
    ///
    /// As [`Self::join`].
    #[cfg(feature = "relay")]
    pub fn local(relay: &crate::relay::Relay, join: &Join) -> Result<Self, LiveError> {
        let shared_origin = relay.shared();
        let shared = Shared::new(join, &shared_origin, &shared_origin)?;
        Ok(Self::serve(shared, Vec::new(), None))
    }

    /// Start the readers of a prepared live session.
    fn serve(shared: Arc<Shared>, origins: Vec<OriginTask>, carrier: Option<Carrier>) -> Self {
        let mut readers = Vec::new();
        let mut demands = Vec::new();
        if let Some(requests) = shared.take_demands() {
            demands.push(Task::spawn(accept_demands(shared.clone(), requests)));
        }
        read_roster(&shared, &mut readers);
        Self {
            shared,
            sent: Mutex::new((0, VecDeque::new())),
            shown: Mutex::new(BTreeMap::new()),
            readers: Mutex::new(readers),
            share_sequence: std::sync::atomic::AtomicU32::new(0),
            _demands: demands,
            _origins: origins,
            _carrier: carrier,
        }
    }

    /// Who this live session publishes as.
    #[must_use]
    pub fn device(&self) -> [u8; 32] {
        self.shared.join.device
    }

    /// Seal `body` and publish it as object `sequence` of `track`.
    ///
    /// Audio is carried on datagrams: `body` is one `tilo-voice/1`
    /// object and only its payload is sealed, so the relay still forwards a
    /// well-formed voice object and never sees the audio itself. Everything
    /// else is one sealed object in a group of its own ([`Self::publish_burst`]).
    ///
    /// # Errors
    ///
    /// [`LiveError::Refused`] when the scope's keys do not cover this
    /// device's `track`, [`LiveError::Ended`] when the session has ended,
    /// [`LiveError::Invalid`] when `body` is not a voice object of the size
    /// the audio datagrams carry, [`LiveError::Withheld`] for audio outside a
    /// call that lets this device speak, [`LiveError::Unwanted`] for audio
    /// nobody in the call asks for, and [`LiveError::Unavailable`] when the
    /// broadcast is gone.
    pub fn publish(&self, track: Track, sequence: u32, body: &[u8]) -> Result<(), LiveError> {
        if track != Track::Audio {
            return self.publish_burst(track, &[(sequence, body)]);
        }
        if self.shared.ended() {
            return Err(LiveError::Ended);
        }
        let revision = self.shared.call_revision.load(Ordering::Acquire);
        let (sealer, name) = self.shared.call_sealer(track)?;
        // The call's voice is sent while someone in the call asks for it, and only then.
        let mut producer = self
            .shared
            .demands
            .lock_recover()
            .out
            .get(&name)
            .map(|out| out.track.clone())
            .ok_or(LiveError::Unwanted)?;
        let object = VoiceObject::decode(body).map_err(|_| LiveError::Invalid)?;
        let at = object.timestamp;
        let payload = sealer.seal(sequence, &object.payload)?;
        let object = VoiceObject { payload, ..object }
            .encode()
            .map_err(|_| LiveError::Invalid)?;
        // The objects before this one again, as far as one datagram
        // carries them: a subscriber that lost the previous packet plays
        // it from this one.
        let mut held = self.sent.lock_recover();
        if held.0 != revision {
            *held = (revision, VecDeque::new());
        }
        let sent = &mut held.1;
        let mut bundle = object.clone();
        for earlier in sent.iter().rev() {
            if bundle.len().saturating_add(earlier.len()) > voice::MAX_BUNDLE_BYTES {
                break;
            }
            bundle.extend_from_slice(earlier);
        }
        sent.push_back(object);
        while sent.len() >= voice::MAX_BUNDLE_OBJECTS {
            sent.pop_front();
        }
        drop(held);
        let timestamp = moq_net::Timestamp::from_scale(at, u64::from(voice::SAMPLE_RATE))
            .unwrap_or(moq_net::Timestamp::ZERO);
        producer
            .insert_datagram(u64::from(sequence), timestamp, bundle)
            .map_err(|_| LiveError::Unavailable)
    }

    /// Seal `objects` and publish them as one group of `track`, in order: a
    /// burst a reader takes whole. A reader passes over a group that newer
    /// ones overtook, so the parts of one burst sent as groups of their own
    /// could each be lost to the next part; as one group, the newest burst
    /// always carries all of it.
    ///
    /// # Errors
    ///
    /// As [`Self::publish`], for a track other than audio.
    pub fn publish_burst(&self, track: Track, objects: &[(u32, &[u8])]) -> Result<(), LiveError> {
        if live::is_call_media(track) {
            return Err(LiveError::Invalid);
        }
        if track == Track::Presence {
            // The call this device says it is in is the one it sends and reads the media of.
            let said = objects
                .iter()
                .rev()
                .find_map(|(_, body)| crate::calls::said_call(body));
            if let Some(said) = said {
                *self.shared.own_call.lock_recover() = said;
                self.shared.settle_call();
            }
        }
        let (sealer, producer) = self.outlet(track)?;
        write_group(&producer, &sealer, objects)
    }

    /// Say `said` on this device's share track, as one burst: what a viewer of a shared window
    /// says to its host and a host to its viewers ([`crate::share`]). Each object is sealed under
    /// the share key of the device whose window it is about.
    ///
    /// # Errors
    ///
    /// [`LiveError::Ended`] when the session has ended, [`LiveError::Refused`] when this device's
    /// keys do not hold the key an object is sealed under or a control is beyond the profile's
    /// bounds, and [`LiveError::Unavailable`] when the broadcast is gone.
    pub fn say(&self, said: &[crate::share::Said]) -> Result<(), LiveError> {
        if said.is_empty() {
            return Ok(());
        }
        if self.shared.ended() {
            return Err(LiveError::Ended);
        }
        let keys = self.shared.keys.lock_recover().clone();
        let mut objects = Vec::with_capacity(said.len());
        for one in said {
            let sequence = self
                .share_sequence
                .fetch_add(1, Ordering::Relaxed)
                .wrapping_add(1);
            let object = crate::share::seal(
                &keys,
                &self.shared.join.scope,
                self.shared.join.device,
                sequence,
                one,
            )?;
            objects.push((sequence, object));
        }
        let name = live::track(Track::Share);
        let producer = self
            .shared
            .published
            .lock_recover()
            .get(name.as_str())
            .cloned()
            .ok_or(LiveError::Unavailable)?;
        let Some(&(last, _)) = objects.last() else {
            return Ok(());
        };
        let mut group = producer
            .create_group(moq_net::group::Info::from(u64::from(last)))
            .map_err(|_| LiveError::Unavailable)?;
        for (sequence, object) in objects {
            let timestamp = moq_net::Timestamp::from_scale(u64::from(sequence), 1)
                .unwrap_or(moq_net::Timestamp::ZERO);
            group
                .write_frame(timestamp, object)
                .map_err(|_| LiveError::Unavailable)?;
        }
        group.finish().map_err(|_| LiveError::Unavailable)?;
        Ok(())
    }

    /// What `track` is sealed with and published on, while the session lasts.
    fn outlet(&self, track: Track) -> Result<(Sealer, moq_net::track::Producer), LiveError> {
        if self.shared.ended() {
            return Err(LiveError::Ended);
        }
        let name = live::track(track);
        let keys = self.shared.keys.lock_recover().clone();
        let sealer = Sealer::new(
            &keys,
            self.shared.join.device,
            stream(track),
            TrackName::new(&self.shared.join.scope, name.as_str()),
        )?;
        // One handle out of the map, so the map's lock is not held across the
        // sealing or the cache write.
        let producer = self
            .shared
            .published
            .lock_recover()
            .get(name.as_str())
            .cloned()
            .ok_or(LiveError::Unavailable)?;
        Ok((sealer, producer))
    }

    /// The next object heard from a peer, or `None` when none is queued.
    #[must_use]
    pub fn try_recv(&self) -> Option<Heard> {
        self.shared.heard.lock_recover().pop_front()
    }

    /// How many objects this live session opened from the peers it reads.
    #[must_use]
    pub fn heard_total(&self) -> u64 {
        self.shared.heard_total.load(Ordering::Relaxed)
    }

    /// Per device, the presence objects opened from it and how many times it was heard leaving.
    #[must_use]
    pub fn presence_from(&self) -> BTreeMap<[u8; 32], (u64, u64)> {
        self.shared.presence_from.lock_recover().clone()
    }

    /// Why this live session could not open what it read, by reason and count: what a
    /// member shows when it is in a scope and hears nobody who is in it.
    #[must_use]
    pub fn refusals(&self) -> BTreeMap<String, usize> {
        self.shared.refusals.lock_recover().clone()
    }

    /// How many devices are in this live session's scope now that it does not hear:
    /// the ones beyond the [`MAX_HEARD`] it reads at once, each read once one
    /// it reads has gone. A device it does not hear is absent from presence,
    /// a call and a shared window on this device, and nothing else says so.
    /// Changes with [`Self::set_unheard_changed`].
    #[must_use]
    pub fn unheard(&self) -> usize {
        self.shared.unheard()
    }

    /// Ask `changed` to run whenever [`Self::unheard`] says otherwise.
    pub fn set_unheard_changed(&self, changed: Arc<dyn Fn() + Send + Sync>) {
        *self.shared.unheard_hook.lock_recover() = Some(changed);
    }

    /// Read the peers' presence from now on, or stop reading it: a device
    /// where nothing shows people neither subscribes to their presence nor
    /// keeps any of it.
    pub fn hear_presence(&self, hear: bool) {
        let moved = {
            let mut hearing = self.shared.hearing.lock_recover();
            std::mem::replace(&mut hearing.presence, hear) != hear
        };
        if moved {
            self.shared.wake_waiting();
        }
    }

    /// Show `tracks` of `sender` here, and stop showing those left out: this
    /// live session asks the peer for exactly these picture layers, windows and
    /// window pages, and its session encodes a layer or a window, or captures
    /// a page, only while a peer asks for it. A track is read from the peer's own broadcast, so what
    /// arrives is [`Shared::deliver`]ed as that peer's objects.
    pub fn show(&self, sender: [u8; 32], tracks: &[Track]) {
        let wanted: BTreeSet<Track> = tracks
            .iter()
            .copied()
            .filter(|track| {
                matches!(
                    track,
                    Track::Camera(_) | Track::Screen(_) | Track::Window(..) | Track::Page(_)
                )
            })
            .collect();
        let mut shown = self.shown.lock_recover();
        let stale: Vec<([u8; 32], Track)> = shown
            .keys()
            .filter(|(device, track)| *device == sender && !wanted.contains(track))
            .copied()
            .collect();
        for key in stale {
            // Dropping the reader's task ends it: the peer is no longer
            // asked for that track.
            shown.remove(&key);
        }
        if sender == self.shared.join.device {
            return;
        }
        for track in wanted {
            if shown.contains_key(&(sender, track)) {
                continue;
            }
            let reader = Task::spawn(receive(self.shared.clone(), sender, track));
            shown.insert((sender, track), reader);
        }
    }

    /// The picture layers someone shows of this device now: exactly what it
    /// encodes. Changes with [`Self::demand_revision`].
    #[must_use]
    pub fn wanted_pictures(&self) -> Vec<(Source, Layer)> {
        self.shared
            .demands
            .lock_recover()
            .out
            .keys()
            .filter_map(|name| {
                let (track, _) = live::parse_call_track(name)?;
                Some((track.picture_of()?, track.layer()))
            })
            .collect()
    }

    /// Whether someone in this device's call asks for its voice now: until
    /// then [`Self::publish`] of audio has nobody to send it to. Changes with
    /// [`Self::demand_revision`].
    #[must_use]
    pub fn voice_wanted(&self) -> bool {
        self.shared.demands.lock_recover().out.keys().any(|name| {
            live::parse_call_track(name).is_some_and(|(track, _)| track == Track::Audio)
        })
    }

    /// The windows whose page someone shows of this device now: exactly what
    /// it captures and sends. Changes with [`Self::demand_revision`].
    #[must_use]
    pub fn wanted_pages(&self) -> Vec<u64> {
        self.shared
            .demands
            .lock_recover()
            .out
            .keys()
            .filter_map(|name| page::parse_track(name))
            .collect()
    }

    /// The shared windows someone shows of this device now, by tab: exactly
    /// what it encodes. Changes with [`Self::demand_revision`].
    #[must_use]
    pub fn wanted_windows(&self) -> Vec<u64> {
        self.shared
            .demands
            .lock_recover()
            .out
            .keys()
            .filter_map(|name| match live::parse_track(name) {
                Some(Track::Window(tab, Layer::High)) => Some(tab),
                _ => None,
            })
            .collect()
    }

    /// Whether a layer of `source` needs a keyframe now, as
    /// [`Self::take_picture_keyframes`] would take.
    #[must_use]
    pub fn wants_picture_keyframe(&self, source: Source) -> bool {
        self.shared
            .demands
            .lock_recover()
            .keyframes
            .iter()
            .any(|held| held.picture_of() == Some(source))
    }

    /// The layers of `source` that need a keyframe now: their tracks were
    /// asked for afresh, or a picture could not join its group. A still
    /// picture sends its new reader nothing until asked.
    #[must_use]
    pub fn take_picture_keyframes(&self, source: Source) -> Vec<Layer> {
        let mut demands = self.shared.demands.lock_recover();
        let layers: Vec<Layer> = demands
            .keyframes
            .iter()
            .filter(|held| held.picture_of() == Some(source))
            .map(|held| held.layer())
            .collect();
        demands
            .keyframes
            .retain(|held| held.picture_of() != Some(source));
        layers
    }

    /// The windows whose page needs a whole frame now, as
    /// [`Self::take_page_keyframes`] would take: their tracks were asked for
    /// afresh, or an object could not join its group.
    #[must_use]
    pub fn take_page_keyframes(&self) -> Vec<u64> {
        let mut demands = self.shared.demands.lock_recover();
        let pages = demands
            .keyframes
            .iter()
            .filter_map(|track| match track {
                Track::Page(tab) => Some(*tab),
                _ => None,
            })
            .collect();
        demands
            .keyframes
            .retain(|track| !matches!(track, Track::Page(_)));
        pages
    }

    /// The shared windows that need a keyframe now: their tracks were asked
    /// for afresh, or a picture could not join its group. A still window sends
    /// its new reader nothing until asked.
    #[must_use]
    pub fn take_window_keyframes(&self) -> Vec<u64> {
        let mut demands = self.shared.demands.lock_recover();
        let windows = demands
            .keyframes
            .iter()
            .filter_map(|track| match track {
                Track::Window(tab, _) => Some(*tab),
                _ => None,
            })
            .collect();
        demands
            .keyframes
            .retain(|track| !matches!(track, Track::Window(..)));
        windows
    }

    /// Advances whenever a demanded track is asked for or ends, or an object
    /// could not join its group: what a publisher reads to encode a picture or
    /// a window, or capture a page.
    #[must_use]
    pub fn demand_revision(&self) -> u64 {
        self.shared.demand_revision.load(Ordering::Relaxed)
    }

    /// Send one picture of `source` on its layer's track, to everyone who
    /// shows that layer. A layer starts with a keyframe, and each keyframe
    /// starts a group.
    ///
    /// The layer is sealed as its own stream: only the payload is sealed, so
    /// the relay still validates the object it forwards and never sees the
    /// picture.
    ///
    /// # Errors
    ///
    /// [`LiveError::Unwanted`] when nobody shows this layer,
    /// [`LiveError::Unkeyed`] for a picture before the layer's first
    /// keyframe or a group past [`video::MAX_GROUP_OBJECTS`] (send a
    /// keyframe), and otherwise as [`Self::publish`].
    pub fn send_picture(&self, source: Source, object: &VideoObject) -> Result<(), LiveError> {
        let track = match source {
            Source::Camera => Track::Camera(object.layer),
            Source::Screen => Track::Screen(object.layer),
        };
        let object = object.clone();
        self.send_keyed(
            track,
            object.keyframe,
            object.timestamp_us,
            &object.payload,
            {
                move |body| {
                    VideoObject {
                        payload: body,
                        ..object
                    }
                    .encode()
                    .map_err(|_| LiveError::Invalid)
                }
            },
        )
    }

    /// Send one picture of the shared window `tab` on its track, to everyone
    /// who shows it. A window starts with a keyframe
    /// ([`Self::take_window_keyframes`]), and each keyframe starts a group.
    ///
    /// Only the payload is sealed, as a call's picture's is.
    ///
    /// # Errors
    ///
    /// [`LiveError::Unwanted`] when nobody shows this window,
    /// [`LiveError::Unkeyed`] for a picture before its first keyframe or a
    /// group past [`video::MAX_GROUP_OBJECTS`] (send a keyframe), and
    /// otherwise as [`Self::publish`].
    pub fn send_window(&self, tab: u64, object: &VideoObject) -> Result<(), LiveError> {
        let object = VideoObject {
            layer: Layer::High,
            ..object.clone()
        };
        self.send_keyed(
            Track::Window(tab, Layer::High),
            object.keyframe,
            object.timestamp_us,
            &object.payload,
            move |body| {
                VideoObject {
                    payload: body,
                    ..object
                }
                .encode()
                .map_err(|_| LiveError::Invalid)
            },
        )
    }

    /// Send one object of window `tab`'s page on its track, to everyone who
    /// shows it. A page starts with a whole frame
    /// ([`Self::take_page_keyframes`]), and each whole frame starts a group.
    ///
    /// The page is sealed as its own stream: only the payload is sealed, so
    /// the relay still validates the object it forwards and never sees the
    /// page.
    ///
    /// # Errors
    ///
    /// [`LiveError::Unwanted`] when nobody shows this page,
    /// [`LiveError::Unkeyed`] for a patch before the page's first whole
    /// frame or a group past [`page::MAX_GROUP_OBJECTS`] (send a whole
    /// frame), and otherwise as [`Self::publish`].
    pub fn send_page(&self, tab: u64, object: &PageObject) -> Result<(), LiveError> {
        let track = Track::Page(tab);
        let object = object.clone();
        self.send_keyed(
            track,
            object.whole,
            object.timestamp_us,
            &object.payload,
            move |body| {
                PageObject {
                    payload: body,
                    ..object
                }
                .encode()
                .map_err(|_| LiveError::Invalid)
            },
        )
    }

    /// Seal one object of a demanded track's payload and write it in the
    /// track's group: `rebuild` puts the sealed payload back in the object the
    /// track carries.
    fn send_keyed(
        &self,
        track: Track,
        whole: bool,
        timestamp_us: u64,
        payload: &[u8],
        rebuild: impl FnOnce(Vec<u8>) -> Result<Vec<u8>, LiveError>,
    ) -> Result<(), LiveError> {
        if self.shared.ended() {
            return Err(LiveError::Ended);
        }
        let (sealer, name) = if live::is_call_media(track) {
            self.shared.call_sealer(track)?
        } else {
            let name = live::track(track);
            let keys = self.shared.keys.lock_recover().clone();
            let sealer = Sealer::new(
                &keys,
                self.shared.join.device,
                stream(track),
                TrackName::new(&self.shared.join.scope, name.as_str()),
            )?;
            (sealer, name)
        };
        let timestamp = moq_net::Timestamp::from_scale(timestamp_us, 1_000_000)
            .map_err(|_| LiveError::Invalid)?;
        let mut demands = self.shared.demands.lock_recover();
        let Some(out) = demands.out.get_mut(&name) else {
            return Err(LiveError::Unwanted);
        };
        let cipher_body =
            sealer.seal(u32::try_from(out.next_group).unwrap_or(u32::MAX), payload)?;
        let bytes = rebuild(cipher_body)?;
        let sent = out.write(whole, timestamp, bytes);
        if sent.is_err() {
            demands.keyframes.insert(track);
            drop(demands);
            self.shared.demand_moved();
        }
        sent
    }

    /// Who may send from now on, under the same keys: a roster that moved
    /// while the epoch stayed as it was.
    ///
    /// A device that joined the roster is read from now on: a live session reads the
    /// devices it knew when it started, so without this a member who joins
    /// after it is never heard.
    pub fn set_roster(&self, roster: Roster) {
        self.shared.opener.lock_recover().set_roster(roster.clone());
        *self.shared.roster.lock_recover() = roster;
        read_roster(&self.shared, &mut self.readers.lock_recover());
        self.shared.settle_call();
        // A reader that waits for a hold for a device the roster names no
        // longer has nothing left to wait for.
        self.shared.wake_waiting();
    }

    /// Hold the workspace's calls to `rules` (by channel) from now on, each device being the
    /// person `persons` names: this device sends media in its call only where the call lets it
    /// speak, and opens the media only of the devices the call lets speak.
    pub fn set_calls(
        &self,
        rules: BTreeMap<String, crate::calls::CallRule>,
        persons: BTreeMap<[u8; 32], String>,
    ) {
        *self.shared.call_rules.lock_recover() = (rules, persons);
        self.shared.settle_call();
    }

    /// Seal and open under `keys` from now on: a channel whose epoch moved,
    /// or one sender's media keys where it holds those.
    pub fn set_keys(&self, keys: impl Into<LiveKeys>) {
        let keys = keys.into();
        let roster = self.shared.roster.lock_recover().clone();
        *self.shared.keys.lock_recover() = keys.clone();
        *self.shared.opener.lock_recover() = Opener::new(keys, roster);
    }

    /// The call media keys this device needs now: the channel its presence says it is in, and
    /// the generation of that call's rule whose key group seals its media
    /// ([`collab_types::calls::media_document`]). `None` outside a call. Until those keys are
    /// handed over ([`Self::set_call_keys`]) this device sends and opens none of the call.
    #[must_use]
    pub fn call_keys_wanted(&self) -> Option<(String, u64)> {
        self.shared.call_keys_wanted()
    }

    /// Seal and open the media of `channel`'s call under `keys` from now on: the keys of the
    /// `generation` of its rule, which only the devices the call admits are given. Keys of a call
    /// or generation this device is not in seal and open nothing.
    pub fn set_call_keys(&self, channel: &str, generation: u64, keys: impl Into<LiveKeys>) {
        *self.shared.call_keys.lock_recover() = Some((channel.to_owned(), generation, keys.into()));
        self.shared.settle_call();
    }

    /// Ask `wake` to run whenever an object is heard or the session ends.
    pub fn set_wake(&self, wake: Arc<dyn Fn() + Send + Sync>) {
        *self.shared.wake.lock_recover() = Some(wake);
    }

    /// Ask `ended` to run once, when the live session ends: the carrier closed, the
    /// relay went away, or the roster no longer opens what it reads.
    pub fn set_ended(&self, ended: Arc<dyn Fn() + Send + Sync>) {
        *self.shared.ended_hook.lock_recover() = Some(ended);
    }

    /// Whether the carrier has ended.
    #[must_use]
    pub fn ended(&self) -> bool {
        self.shared.ended()
    }
}

impl Drop for LiveSession {
    fn drop(&mut self) {
        self.shared.end();
    }
}

/// The stream a live session track's frames are keyed and sealed as.
#[must_use]
pub const fn stream(track: Track) -> Stream {
    match track {
        Track::Presence => Stream::Presence,
        Track::Audio => Stream::Microphone,
        Track::Camera(_) => Stream::Camera,
        Track::Screen(_) => Stream::Screen,
        Track::Window(..) => Stream::Window,
        Track::Page(_) => Stream::Page,
        Track::Share => Stream::Share,
        Track::Activity => Stream::Activity,
    }
}

/// Retention of one published object at the relay.
fn retain(track: Track) -> Duration {
    match live::delivery(track) {
        Delivery::Datagram { retain, .. } | Delivery::Keyed { retain, .. } => retain,
        Delivery::Stream { max_age } => max_age,
    }
}

/// The track's info, as the relay republishes it: its retention and priority.
fn track_info(track: Track) -> moq_net::track::Info {
    moq_net::track::Info::default()
        .with_max_age(retain(track))
        .with_priority(live::priority(track))
}

/// Serve the peers' requests for this device's demanded tracks: a request for
/// a camera layer, a screen layer, a shared window or a window's page is
/// accepted and published
/// from then on, a track
/// nobody reads past its grace is ended
/// ([`watch_unused`]), and anything else is refused.
async fn accept_demands(shared: Arc<Shared>, mut requests: moq_net::broadcast::Dynamic) {
    while let Ok(request) = requests.requested_track().await {
        // Only a track someone can ask this device for: the voice and pictures of the call it is
        // in, the windows it shares and the pages of the windows it shows.
        let accepted = match live::parse_call_track(request.name()) {
            Some((track, call)) => shared.in_call(call).then_some(track),
            None => live::parse_track(request.name())
                .filter(|track| matches!(track, Track::Window(_, Layer::High) | Track::Page(_))),
        };
        let Some(accepted) = accepted else {
            request.reject(moq_net::Error::NotFound);
            continue;
        };
        if shared.ended() {
            request.reject(moq_net::Error::Cancel);
            return;
        }
        let name = request.name().to_owned();
        let out = request.accept(track_info(accepted));
        let unused = watch_unused(&shared, name.clone(), out.clone());
        let mut demands = shared.demands.lock_recover();
        let next_group = demands.next_group.get(&name).copied().unwrap_or(0);
        demands.out.insert(
            name,
            Demand {
                track: out,
                group: None,
                next_group,
                in_group: 0,
                _unused: unused,
            },
        );
        // The new reader can start at nothing but a group's first object: a
        // picture's keyframe, a page's whole frame. Voice has none.
        if accepted != Track::Audio {
            demands.keyframes.insert(accepted);
        }
        drop(demands);
        shared.demand_moved();
    }
}

/// A demanded track nobody reads any more is ended and no longer encoded. A
/// track is accepted before its reader attaches, so it is ended only once it
/// stays unread past the grace: a track ended in that gap would take the
/// keyframe asked for with it.
fn watch_unused(shared: &Arc<Shared>, name: String, track: moq_net::track::Producer) -> Task {
    let shared = Arc::downgrade(shared);
    Task::spawn(async move {
        loop {
            unused(&track).await;
            let Some(shared) = shared.upgrade() else {
                return;
            };
            let mut demands = shared.demands.lock_recover();
            if !demands
                .out
                .get(&name)
                .is_some_and(|out| out.track.is_clone(&track))
            {
                return;
            }
            if track.clone().abort_unused(moq_net::Error::Cancel).is_err() {
                continue;
            }
            if let Some(out) = demands.out.remove(&name) {
                demands.next_group.insert(name, out.next_group);
            }
            drop(demands);
            shared.demand_moved();
            return;
        }
    })
}

/// Open one picture object's sealed payload, keeping the header the relay
/// validated: `None` when the epoch or the roster no longer opens it.
fn open_picture(
    shared: &Shared,
    device: [u8; 32],
    (track, name): (Track, &str),
    bytes: &[u8],
) -> Option<(u32, Vec<u8>)> {
    let object = VideoObject::decode(bytes).ok()?;
    let opened = shared.open(device, track, name, &object.payload).ok()?;
    let sequence = u32::try_from(object.sequence).unwrap_or(u32::MAX);
    let body = VideoObject {
        payload: opened,
        ..object
    }
    .encode()
    .ok()?;
    Some((sequence, body))
}

/// Open one page object's sealed payload, keeping the header the relay
/// validated: `None` when the epoch or the roster no longer opens it.
fn open_page(
    shared: &Shared,
    device: [u8; 32],
    (track, name): (Track, &str),
    bytes: &[u8],
) -> Option<(u32, Vec<u8>)> {
    let object = PageObject::decode(bytes).ok()?;
    let opened = shared.open(device, track, name, &object.payload).ok()?;
    let sequence = u32::try_from(object.sequence).unwrap_or(u32::MAX);
    let body = PageObject {
        payload: opened,
        ..object
    }
    .encode()
    .ok()?;
    Some((sequence, body))
}

/// The wait a track nobody reads may stay accepted: a reader attaches just
/// after its request (a picture's keyframe, a relay session's SUBSCRIBE
/// exchanging consumer handles), and a track ended in that gap would take
/// what it was asked for with it.
pub(crate) const UNUSED_GRACE: Duration = Duration::from_secs(2);

/// Wait until `track` is unread for longer than [`UNUSED_GRACE`].
pub(crate) async fn unused(track: &moq_net::track::Producer) {
    loop {
        if track.unused().await.is_err() {
            return;
        }
        n0_future::time::sleep(UNUSED_GRACE).await;
        if !track.is_used() {
            return;
        }
    }
}

/// Read `track` of `device`'s broadcast for as long as the live session lives.
///
/// One subscription per broadcast, held while the device publishes: a frame
/// this live session cannot open yet is passed over, and the next opens once
/// [`LiveSession::set_keys`] caught up. A device's broadcast ends when its live session does — it left the workspace and
/// came back, or a page reloaded — and is announced again under the same
/// namespace by the session that took its place. This reader waits for that
/// announcement and reads the track again, so the device is heard here
/// without this live session joining again; what it said before is not played twice.
///
/// A track every device is read by ([`TRACKS`]) is read from a hold: its
/// device is one of the [`MAX_HEARD`] this live session reads at once, or the
/// reader waits for one of those to go and the device counts as not heard
/// until then ([`LiveSession::unheard`]). A track asked for by what the device
/// shows ([`LiveSession::show`]) takes none. Presence is read only while the live session
/// hears it ([`LiveSession::hear_presence`]): its subscription is dropped when that
/// stops, and taken again at the newest group when it starts again.
async fn receive(shared: Arc<Shared>, device: [u8; 32], track: Track) {
    let address = Namespace::new(&shared.join.scope, device).path();
    let takes_hold = TRACKS.contains(&track);
    let presence = track == Track::Presence;
    let call_media = live::is_call_media(track);
    loop {
        if presence {
            std::future::poll_fn(|cx| shared.poll_presence(device, true, cx)).await;
        }
        if shared.ended() || !shared.admits(device) {
            return;
        }
        let Ok(broadcast) = shared
            .incoming
            .consume()
            .routed_broadcast(address.as_str())
            .await
        else {
            return;
        };
        let _hold = if takes_hold {
            let Some(hold) = hold(&shared, device, &broadcast).await else {
                // The broadcast went away while this reader waited for a
                // hold, or there is nothing left to read it for.
                continue;
            };
            Some(hold)
        } else {
            None
        };
        // A call's media is read under the name of the call this device is in, from a device the
        // call lets speak; outside one, the reader waits for the call to move.
        let (name, revision) = if call_media {
            let revision = shared.call_revision.load(Ordering::Acquire);
            let Some(named) = shared.call_name(track, device) else {
                let moved = std::future::poll_fn(|cx| shared.poll_call_moved(revision, cx));
                n0_future::future::or(moved, async {
                    let _ = broadcast.closed().await;
                })
                .await;
                continue;
            };
            named
        } else {
            (live::track(track), 0)
        };
        // A reader that attaches late starts at the newest group the track
        // holds: the presence a peer said before it joined is what it draws,
        // instead of waiting out the heartbeat.
        let subscription = match broadcast.track(name.as_str()) {
            Ok(producer) => {
                let newest = producer.latest();
                producer
                    .subscribe(
                        moq_net::track::Subscription::default()
                            .with_max_age(retain(track))
                            .with_start(newest.map(moq_net::track::Position::group)),
                    )
                    .await
                    .ok()
            }
            Err(_) => None,
        };
        let Some(mut subscriber) = subscription else {
            if call_media {
                // The device is not in this call yet, or its relay does not carry it to this
                // one: asked again once the call moves, or a moment on.
                call_retry(&shared, revision, &broadcast).await;
            } else {
                let _ = broadcast.closed().await;
            }
            continue;
        };
        let reading = async { Some(read(&shared, device, (track, &name), &mut subscriber).await) };
        let said = if presence {
            let stopped = async {
                std::future::poll_fn(|cx| shared.poll_presence(device, false, cx)).await;
                None
            };
            n0_future::future::or(reading, stopped).await
        } else if call_media {
            // The call moved: what was read under its old name is let go.
            let moved = async {
                std::future::poll_fn(|cx| shared.poll_call_moved(revision, cx)).await;
                None
            };
            n0_future::future::or(reading, moved).await
        } else {
            reading.await
        };
        // Presence is no longer heard, or the call moved: the subscription goes, and the relay
        // stops sending it here.
        let Some(said) = said else {
            continue;
        };
        if said && !shared.ended() {
            // The device's broadcast went away with its session after it said
            // itself: whoever drew it takes it down now, rather than once it
            // has been silent long enough. A device that comes back says itself
            // again on the broadcast that takes its place.
            shared.deliver(Heard::left(device));
        }
        if call_media {
            call_retry(&shared, revision, &broadcast).await;
            continue;
        }
        // The subscription ended with the broadcast: the one that takes its
        // place is read once announced.
        let _ = broadcast.closed().await;
    }
}

/// How long a reader of a call's media waits before it asks a device again for a call track
/// the device did not carry: the device may have joined the call since.
const CALL_RETRY: Duration = Duration::from_millis(500);

/// Wait until the call moves past `revision`, `broadcast` ends, or [`CALL_RETRY`] passed.
async fn call_retry(shared: &Shared, revision: u64, broadcast: &moq_net::broadcast::Consumer) {
    let moved = std::future::poll_fn(|cx| shared.poll_call_moved(revision, cx));
    let gone = async {
        let _ = broadcast.closed().await;
    };
    let later = n0_future::time::sleep(CALL_RETRY);
    n0_future::future::or(moved, n0_future::future::or(gone, later)).await;
}

/// Read one subscription of `track` until it ends, the live session is over or the
/// device left the roster: `true` when a presence track that delivered a
/// frame ended with its broadcast. A frame this live session cannot open is counted
/// ([`LiveSession::refusals`]) and passed over: the subscription stays, and the
/// device's next frame is opened under whatever keys and roster the live session
/// holds by then.
async fn read(
    shared: &Shared,
    device: [u8; 32],
    (track, name): (Track, &str),
    subscriber: &mut moq_net::track::Subscriber,
) -> bool {
    // Whether a presence frame of this subscription was delivered.
    let mut said = false;
    // The newest audio object played: a bundle carries earlier objects again,
    // and one already played is not played twice.
    let mut played: Option<u64> = None;
    loop {
        match next(subscriber).await {
            Next::Datagram(datagram) => {
                // Only audio travels as datagrams: one bundle of voice
                // objects, newest first, each with its payload sealed.
                if track != Track::Audio {
                    continue;
                }
                let Ok(objects) = voice::bundle_objects(&datagram.payload) else {
                    continue;
                };
                for sealed in objects.into_iter().rev() {
                    let Ok(object) = VoiceObject::decode(sealed) else {
                        continue;
                    };
                    if played.is_some_and(|seen| object.sequence <= seen) {
                        continue;
                    }
                    if shared.ended() || !shared.admits(device) {
                        return false;
                    }
                    if hear_voice(shared, device, name, &object) {
                        played = Some(object.sequence);
                    }
                }
            }
            Next::Group(mut group) => {
                let sequence = u32::try_from(group.sequence).unwrap_or(u32::MAX);
                while let Ok(Some(frame)) = group.read_frame().await {
                    if shared.ended() || !shared.admits(device) {
                        return false;
                    }
                    if track == Track::Audio {
                        // Group streams carry one voice object per frame: the
                        // same object the datagrams carry.
                        let Ok(object) = VoiceObject::decode(&frame.payload) else {
                            continue;
                        };
                        if played.is_some_and(|seen| object.sequence <= seen) {
                            continue;
                        }
                        if hear_voice(shared, device, name, &object) {
                            played = Some(object.sequence);
                        }
                        continue;
                    }
                    said |= take_frame(shared, device, (track, name), sequence, &frame.payload);
                }
            }
            Next::End => return said,
        }
    }
}

/// Open one frame of a group of `track` (anything but audio) and deliver it: `true` when it is
/// a presence frame that was delivered.
fn take_frame(
    shared: &Shared,
    device: [u8; 32],
    (track, name): (Track, &str),
    sequence: u32,
    payload: &[u8],
) -> bool {
    if matches!(track, Track::Page(_)) {
        // A page keeps the header the relay validated: whether
        // it is a whole frame is what a viewer acts on.
        if let Some((sequence, body)) = open_page(shared, device, (track, name), payload) {
            shared.deliver(Heard {
                sender: device,
                track,
                sequence,
                body,
            });
        }
        return false;
    }
    if matches!(
        track,
        Track::Camera(_) | Track::Screen(_) | Track::Window(..)
    ) {
        // A picture keeps the header the relay validated.
        if let Some((sequence, body)) = open_picture(shared, device, (track, name), payload) {
            shared.deliver_picture(Heard {
                sender: device,
                track,
                sequence,
                body,
            });
        }
        return false;
    }
    if track == Track::Presence && !shared.hears_presence() {
        // Arrived as the subscription was let go.
        return false;
    }
    if track == Track::Share {
        // Addressed to one device, and sealed under its host's key: only what is
        // this device's is opened, and what is another's is passed by.
        match shared.open_share(device, payload) {
            Ok(Some(body)) => shared.deliver(Heard {
                sender: device,
                track,
                sequence,
                body,
            }),
            Ok(None) => {}
            Err(refusal) => shared.refused(&refusal),
        }
        return false;
    }
    match shared.open(device, track, name, payload) {
        Ok(body) => {
            shared.deliver(Heard {
                sender: device,
                track,
                sequence,
                body,
            });
            track == Track::Presence
        }
        Err(refusal) => {
            shared.refused(&refusal);
            false
        }
    }
}

/// Open one voice object's payload and deliver the object: `false` when the
/// epoch or the roster no longer opens it, which ends the read.
fn hear_voice(shared: &Shared, device: [u8; 32], name: &str, object: &VoiceObject) -> bool {
    let opened = match shared.open(device, Track::Audio, name, &object.payload) {
        Ok(opened) => opened,
        Err(refusal) => {
            shared.refused(&refusal);
            return false;
        }
    };
    let Ok(body) = (VoiceObject {
        payload: opened,
        ..object.clone()
    })
    .encode() else {
        return false;
    };
    shared.deliver(Heard {
        sender: device,
        track: Track::Audio,
        sequence: u32::try_from(object.sequence).unwrap_or(u32::MAX),
        body,
    });
    true
}

/// Seal `objects` and write them as one group of `producer`'s track, numbered
/// by the last of them: the group a burst is.
fn write_group(
    producer: &moq_net::track::Producer,
    sealer: &Sealer,
    objects: &[(u32, &[u8])],
) -> Result<(), LiveError> {
    let Some(&(last, _)) = objects.last() else {
        return Ok(());
    };
    let mut group = producer
        .create_group(moq_net::group::Info::from(u64::from(last)))
        .map_err(|_| LiveError::Unavailable)?;
    for &(sequence, body) in objects {
        let frame = sealer.seal(sequence, body)?;
        let timestamp = moq_net::Timestamp::from_scale(u64::from(sequence), 1)
            .unwrap_or(moq_net::Timestamp::ZERO);
        group
            .write_frame(timestamp, frame)
            .map_err(|_| LiveError::Unavailable)?;
    }
    group.finish().map_err(|_| LiveError::Unavailable)?;
    Ok(())
}

/// What a live session's reader, or the relay's copy, sees next on a subscribed
/// track.
pub(crate) enum Next {
    Datagram(moq_net::Datagram),
    Group(Box<moq_net::group::Consumer>),
    End,
}

/// The next object on `track`, from datagrams or a stream. Either one that ends or breaks
/// ends the read: the carrier reconnects.
pub(crate) async fn next(track: &mut moq_net::track::Subscriber) -> Next {
    moq_net::kio::wait(|waiter| {
        let datagram = track.poll_recv_datagram(waiter);
        match datagram {
            Poll::Ready(Ok(Some(datagram))) => return Poll::Ready(Next::Datagram(datagram)),
            Poll::Ready(Ok(None) | Err(_)) => return Poll::Ready(Next::End),
            Poll::Pending => {}
        }
        match track.poll_recv_group(waiter) {
            Poll::Ready(Ok(Some(group))) => Poll::Ready(Next::Group(Box::new(group))),
            Poll::Ready(Ok(None) | Err(_)) => Poll::Ready(Next::End),
            Poll::Pending => Poll::Pending,
        }
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_carrier_that_went_away_is_dialled_again_sooner_at_first_and_never_gives_up() {
        let waits: Vec<Duration> = (1..=12).map(redial_wait).collect();
        assert_eq!(waits[0], REDIAL_FIRST);
        for pair in waits.windows(2) {
            assert!(
                pair[1] >= pair[0],
                "a redial waits no less than the one before it: {waits:?}"
            );
        }
        assert!(
            waits.contains(&REDIAL_LONGEST) && waits.iter().all(|wait| *wait <= REDIAL_LONGEST),
            "a relay that stays away is dialled now and then rather than every turn: {waits:?}"
        );
        assert_eq!(
            redial_wait(0),
            REDIAL_FIRST,
            "the first attempt of a carrier that went away waits the shortest"
        );
    }

    /// A device takes up to a quarter off a wait, by its own key: never more, never longer than
    /// the wait, the same every time it is asked, and not what the next device takes, so the
    /// devices that missed a relay together do not ask it again together.
    #[test]
    fn devices_that_wait_for_the_same_thing_do_not_all_wait_the_same() {
        let wait = Duration::from_secs(8);
        let three_quarters = Duration::from_secs(6);
        let waits: Vec<Duration> = (0..64_u8)
            .map(|device| spread(wait, [device; 32], 3))
            .collect();
        for spread in &waits {
            assert!(
                *spread <= wait && *spread >= three_quarters,
                "a device waits between three quarters of the wait and all of it: {spread:?}"
            );
        }
        assert_eq!(
            spread(wait, [7; 32], 3),
            spread(wait, [7; 32], 3),
            "a device's own wait is the same whenever it is asked"
        );
        // Sixty-four devices that missed a relay in one moment ask it again over the quarter,
        // and no sixteen of them (what a relay admits at once) within the same tenth of it.
        let tenths: Vec<usize> = (0..10_u128)
            .map(|tenth| {
                waits
                    .iter()
                    .filter(|spread| {
                        // The quarter is two seconds: a tenth of it is two hundred milliseconds.
                        let off = wait.saturating_sub(**spread).as_millis();
                        (off / 200).min(9) == tenth
                    })
                    .count()
            })
            .collect();
        assert!(
            tenths.iter().all(|devices| *devices < 16),
            "the devices' waits are spread over the quarter: {tenths:?}"
        );
        let distinct: BTreeSet<Duration> = waits.iter().copied().collect();
        assert!(distinct.len() > 56, "{} distinct waits", distinct.len());
        // The schedule keeps its shape: each wait is longer than the one before it, whatever each
        // device takes off, until the longest.
        for device in 0..64_u8 {
            let waits: Vec<Duration> = (1..=6)
                .map(|attempt| spread(redial_wait(attempt), [device; 32], u64::from(attempt)))
                .collect();
            assert!(
                waits.windows(2).all(|pair| pair[1] > pair[0]),
                "a device waits longer each time: {waits:?}"
            );
        }
    }
}
