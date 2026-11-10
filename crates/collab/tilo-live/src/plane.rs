//! One participant's live session in one scope: what it publishes, sealed
//! under the scope's channel content keys, and what it hears from the
//! scope's other participants.
//!
//! A [`Plane`] is one carrier session ([`crate::carrier`]) with one broadcast
//! of its own ([`crate::live::Namespace`] of this device) and one reader per
//! roster device and track. Every object it sends is sealed with
//! `Sealer`; every object it receives is opened with `Opener`, so what the
//! relay copies is opaque to the relay and a frame from a device the roster
//! does not name is refused before it is decrypted.
//!
//! Presence, audio and the activity note are tracks every plane publishes.
//! Pictures are not: a camera or screen layer exists only while a peer asks
//! for it ([`Plane::show`]), and a device encodes a layer only while it is
//! asked for ([`Plane::wanted_pictures`]), so a picture nobody watches costs
//! no bitrate and no encoder.
//!
//! The carrier is whatever [`Dial`] this device has: iroh to the device that
//! serves the workspace ([`crate::native::Iroh`]). This module does not name
//! it.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::task::Poll;
use std::time::Duration;

use crate::frame::{Opener, Refusal, Sealer, Stream, TrackName};
use crate::roster::Roster;
use crate::video::{self, Layer, Source, VideoObject};
use channel_crypto::ChannelContentKeys;
use moq_net::origin::{Consumer, Producer};
use tilo_lock::MutexRecover as _;

use crate::Task;
use crate::admit::{AdmissionError, Connected, Dial, OriginTask, origin_config};
use crate::live::{self, Delivery, Namespace, Track, exact};
use crate::scope::Scope;
use crate::voice::{self, VoiceObject};

/// The tracks every plane publishes: the workspace's presence, a call's audio
/// and the activity note. A plane's pictures ([`Track::Camera`],
/// [`Track::Screen`]) are not among them: a layer exists only while someone
/// asks for it ([`Plane::show`]).
pub const TRACKS: [Track; 3] = [Track::Presence, Track::Audio, Track::Activity];

/// Objects one plane keeps before it drops the oldest: a jitter buffer, not a
/// backlog.
const QUEUE: usize = 256;
/// Pictures one received track keeps before it starts over at the next
/// keyframe: a decoder never sees a picture whose reference was dropped.
const PICTURES_QUEUED: usize = 8;
/// Join budget: the transport, the admission preface and the setup.
const JOINT: Duration = Duration::from_secs(10);
/// How long a reader waits before asking again for a peer's broadcast whose
/// read ended with nothing in it.
const REJOIN: Duration = Duration::from_millis(100);
/// Bytes one plane's own publication may cache at its ingress.
const INGRESS: u64 = 2 * 1024 * 1024;
/// Bytes a plane caches of what it hears.
const EGRESS: u64 = 4 * 1024 * 1024;
/// Longest anything a plane publishes is retained by its own origin.
const RETAIN: Duration = Duration::from_secs(30);

/// Why a plane could not start, or stopped carrying something.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum PlaneError {
    /// The relay refused the grant.
    #[error("the live plane was not admitted")]
    Denied,
    /// The carrier or its setup failed.
    #[error("the live plane is unavailable")]
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
    #[error("the live plane has ended")]
    Ended,
}

impl From<AdmissionError> for PlaneError {
    fn from(error: AdmissionError) -> Self {
        match error {
            AdmissionError::Denied | AdmissionError::Invalid => Self::Denied,
            AdmissionError::Unavailable => Self::Unavailable,
        }
    }
}

impl From<Refusal> for PlaneError {
    fn from(refusal: Refusal) -> Self {
        Self::Refused(refusal)
    }
}

/// What one plane joins: the scope, this device, the scope's keys and who may
/// send in it.
#[derive(Clone)]
pub struct Join {
    pub scope: Scope,
    pub device: [u8; 32],
    pub keys: ChannelContentKeys,
    pub roster: Roster,
    /// Whether this device publishes its own broadcast, or only listens.
    pub publish: bool,
}

/// One object a plane heard, already opened.
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

/// One picture track this plane publishes because a peer asked for it.
struct PictureOut {
    track: moq_net::track::Producer,
    group: Option<moq_net::group::Producer>,
    next_group: u64,
    in_group: u64,
    /// Ends the track once nobody reads it ([`watch_unused`]).
    _unused: Task,
}

impl PictureOut {
    /// Write one encoded picture: a keyframe starts a group.
    fn write(
        &mut self,
        keyframe: bool,
        timestamp: moq_net::Timestamp,
        bytes: Vec<u8>,
    ) -> Result<(), PlaneError> {
        if keyframe {
            if let Some(group) = self.group.take() {
                let _ = group.finish();
            }
            self.group = Some(
                self.track
                    .create_group(moq_net::group::Info::from(self.next_group))
                    .map_err(|_| PlaneError::Unkeyed)?,
            );
            self.next_group = self.next_group.saturating_add(1);
            self.in_group = 0;
        }
        if self.in_group >= video::MAX_GROUP_OBJECTS {
            self.group = None;
            return Err(PlaneError::Unkeyed);
        }
        let group = self.group.as_mut().ok_or(PlaneError::Unkeyed)?;
        group
            .write_frame(timestamp, bytes)
            .map_err(|_| PlaneError::Unkeyed)?;
        self.in_group = self.in_group.saturating_add(1);
        Ok(())
    }
}

/// The picture tracks of one plane, as its readers asked for them.
#[derive(Default)]
struct Pictures {
    /// The tracks someone asked this device for, by track name: exactly what
    /// it encodes ([`Plane::wanted_pictures`]).
    out: BTreeMap<String, PictureOut>,
    /// Where a track asked for again numbers its groups: one past the last
    /// group of its earlier acceptance, so a subscriber that asks again is
    /// never sent a group it already read.
    next_group: BTreeMap<String, u64>,
    /// Tracks whose reader needs a keyframe: they were asked for afresh, or
    /// a picture could not join its group.
    keyframes: BTreeSet<video::Track>,
    /// Received tracks that dropped a picture: nothing of them is delivered
    /// until a keyframe.
    behind: BTreeSet<([u8; 32], Track)>,
}

/// What a plane shares with its readers.
pub(crate) struct Shared {
    pub(crate) join: Join,
    incoming: Producer,
    opener: Mutex<Opener>,
    /// The keys `publish` seals under: the join's, until the channel's epoch
    /// moves ([`Plane::set_keys`]).
    keys: Mutex<ChannelContentKeys>,
    roster: Mutex<Roster>,
    /// How many readers each device the roster named has running now: a
    /// device is read by exactly one set of them.
    reading: Mutex<BTreeSet<[u8; 32]>>,
    published: Mutex<std::collections::BTreeMap<String, moq_net::track::Producer>>,
    /// This device's broadcast, kept for as long as the plane lives: dropping
    /// it withdraws the announcement its subscribers are reading.
    _broadcast: Option<moq_net::broadcast::Producer>,
    /// The peers' requests for picture tracks of this device's broadcast,
    /// taken once by the plane's acceptor.
    picture_requests: Mutex<Option<moq_net::broadcast::Dynamic>>,
    /// The picture layers someone shows of this device, and the groups they
    /// are written in.
    pictures: Mutex<Pictures>,
    /// Advances whenever a picture layer is asked for or ends, or a picture
    /// could not join its group.
    pictures_revision: AtomicU64,
    heard: Mutex<VecDeque<Heard>>,
    /// How many objects this plane opened from its peers, and why it could not
    /// open what a reader got: the answer a member needs when it hears nobody
    /// who is in the roster.
    heard_total: AtomicU64,
    refusals: Mutex<BTreeMap<String, usize>>,
    ended: Mutex<bool>,
    wake: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
    /// Told once, when the plane ends: what rejoins the scope elsewhere.
    ended_hook: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
}

impl Shared {
    /// The one plane of one device in one scope, with its own broadcast
    /// announced in `outgoing`.
    fn new(join: &Join, outgoing: &Producer, incoming: &Producer) -> Result<Arc<Self>, PlaneError> {
        // A plane whose keys cannot seal this device's presence is not one
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
                .map_err(|_| PlaneError::Unavailable)?;
            // Every track exists before the broadcast is announced: a relay
            // copies the set it finds announced, and a track that appeared
            // later would never be copied to the subscribers already there.
            let mut published = std::collections::BTreeMap::new();
            for track in TRACKS {
                let name = live::track(track);
                let producer = broadcast
                    .create_track(name.as_str(), track_info(track))
                    .map_err(|_| PlaneError::Unavailable)?;
                published.insert(name, producer);
            }
            // Taken before the announcement, like the relay's own forwarding:
            // a request that arrives with the announcement is not one to miss.
            let requests = broadcast.dynamic();
            broadcast
                .announce(moq_net::origin::Route::default())
                .map_err(|_| PlaneError::Unavailable)?;
            (Some(broadcast), published, Some(requests))
        } else {
            (None, std::collections::BTreeMap::new(), None)
        };
        Ok(Arc::new(Self {
            opener: Mutex::new(Opener::new(join.keys.clone(), join.roster.clone())),
            keys: Mutex::new(join.keys.clone()),
            roster: Mutex::new(join.roster.clone()),
            reading: Mutex::new(BTreeSet::new()),
            join: join.clone(),
            incoming: incoming.clone(),
            published: Mutex::new(published),
            _broadcast: broadcast,
            picture_requests: Mutex::new(requests),
            pictures: Mutex::new(Pictures::default()),
            pictures_revision: AtomicU64::new(0),
            heard: Mutex::new(VecDeque::new()),
            heard_total: AtomicU64::new(0),
            refusals: Mutex::new(BTreeMap::new()),
            ended: Mutex::new(false),
            wake: Mutex::new(None),
            ended_hook: Mutex::new(None),
        }))
    }

    /// What the carrier publishes: exactly this device's own broadcast.
    fn publication(&self, outgoing: &Producer) -> Option<Consumer> {
        if !self.join.publish {
            return None;
        }
        let path = Namespace::new(&self.join.scope, self.join.device).path();
        exact(&[path.as_str()]).and_then(|names| outgoing.consume().scope("", &names).ok())
    }

    /// What the carrier subscribes to: every participant of this scope.
    fn subscription(&self, incoming: &Producer) -> Result<Option<Producer>, PlaneError> {
        let prefix = live::participants(&self.join.scope);
        let pattern =
            moq_net::Pattern::subtree(prefix.as_str()).map_err(|_| PlaneError::Unavailable)?;
        let scoped = incoming
            .scope("", &moq_net::Patterns::from(pattern))
            .map_err(|_| PlaneError::Unavailable)?;
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
            let hook = self.ended_hook.lock_recover().clone();
            if let Some(hook) = hook {
                hook();
            }
            self.wake();
        }
    }
    pub(crate) fn wake(&self) {
        let wake = self.wake.lock_recover().clone();
        if let Some(wake) = wake {
            wake();
        }
    }
    fn deliver(&self, heard: Heard) {
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
    /// One frame a reader got and this plane could not open, by why.
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
        let mut pictures = self.pictures.lock_recover();
        if pictures.behind.contains(&key) {
            if !keyframe {
                return;
            }
            pictures.behind.remove(&key);
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
                pictures.behind.insert(key);
                return;
            }
        }
        drop(pictures);
        self.deliver(heard);
    }
    /// A picture track someone asked for, or one this plane no longer reads,
    /// or one that needs a keyframe: the encoder reads it.
    fn pictures_moved(&self) {
        self.pictures_revision.fetch_add(1, Ordering::Relaxed);
        self.wake();
    }
    /// The peers' requests for this device's picture tracks, taken once.
    fn take_picture_requests(&self) -> Option<moq_net::broadcast::Dynamic> {
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
    fn open(&self, sender: [u8; 32], track: Track, frame: &[u8]) -> Result<Vec<u8>, Refusal> {
        let name = live::track(track);
        let name = TrackName::new(&self.join.scope, name.as_str());
        let opener = self.opener.lock_recover();
        opener.open(sender, stream(track), &name, frame)
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

/// One device's sealed session in one scope.
pub struct Plane {
    shared: Arc<Shared>,
    /// This device's recent audio objects, newest last: the redundancy its
    /// datagrams carry, so one lost packet costs one frame and not two.
    sent: Mutex<VecDeque<Vec<u8>>>,
    /// The peer picture tracks this plane reads now: exactly what its app
    /// shows ([`Self::show`]), one reader each.
    shown: Mutex<BTreeMap<([u8; 32], Track), Task>>,
    /// One reader per device of the roster, by track: started when the device
    /// joins the roster, ended by itself once the roster no longer admits it.
    readers: Mutex<Vec<Task>>,
    _pictures: Vec<Task>,
    _origins: Vec<OriginTask>,
    _connected: Option<Connected>,
}

impl Plane {
    /// Join `join`'s scope through `dial`, admitted with `grant`.
    ///
    /// # Errors
    ///
    /// [`PlaneError::Denied`] when the relay refuses the grant,
    /// [`PlaneError::Unavailable`] when the carrier or its setup fails, and
    /// [`PlaneError::Refused`] when the scope's keys do not cover this
    /// device's own presence.
    pub async fn join(dial: &impl Dial, grant: &[u8], join: Join) -> Result<Self, PlaneError> {
        Self::join_within(dial, grant, join, JOINT).await
    }

    /// Join as [`Self::join`], waiting at most `within` for the carrier and
    /// the admission: a device trying the relays of a scope in turn spends no
    /// more than this on one that is not there.
    ///
    /// # Errors
    ///
    /// As [`Self::join`].
    pub async fn join_within(
        dial: &impl Dial,
        grant: &[u8],
        join: Join,
        within: Duration,
    ) -> Result<Self, PlaneError> {
        let (outgoing, outgoing_task) = OriginTask::produce(origin_config(INGRESS, RETAIN));
        let (incoming, incoming_task) = OriginTask::produce(origin_config(EGRESS, RETAIN));
        let shared = Shared::new(&join, &outgoing, &incoming)?;
        let publish = shared.publication(&outgoing);
        let subscribe = shared.subscription(&incoming)?;
        let connected = dial.dial(grant, publish, subscribe, within).await?;
        let plane = Self::serve(shared, vec![outgoing_task, incoming_task], Some(connected));
        Ok(plane)
    }

    /// Join `join`'s scope on the relay this device itself serves: no
    /// connection to itself, and the same sealed objects, published into and
    /// heard from the relay's shared origin ([`crate::relay::Relay::shared`]).
    ///
    /// # Errors
    ///
    /// As [`Self::join`].
    #[cfg(feature = "relay")]
    pub fn local(relay: &crate::relay::Relay, join: &Join) -> Result<Self, PlaneError> {
        let shared_origin = relay.shared();
        let shared = Shared::new(join, &shared_origin, &shared_origin)?;
        Ok(Self::serve(shared, Vec::new(), None))
    }

    /// Start the readers of a prepared plane.
    fn serve(shared: Arc<Shared>, origins: Vec<OriginTask>, connected: Option<Connected>) -> Self {
        let mut readers = Vec::new();
        let mut pictures = Vec::new();
        if let Some(connected) = &connected {
            let watched = connected.session.clone();
            let ended = shared.clone();
            readers.push(Task::spawn(async move {
                let _ = watched.closed().await;
                ended.end();
            }));
        }
        if let Some(requests) = shared.take_picture_requests() {
            pictures.push(Task::spawn(accept_pictures(shared.clone(), requests)));
        }
        read_roster(&shared, &mut readers);
        Self {
            shared,
            sent: Mutex::new(VecDeque::new()),
            shown: Mutex::new(BTreeMap::new()),
            readers: Mutex::new(readers),
            _pictures: pictures,
            _origins: origins,
            _connected: connected,
        }
    }

    /// Who this plane publishes as.
    #[must_use]
    pub fn device(&self) -> [u8; 32] {
        self.shared.join.device
    }

    /// Seal `body` and publish it as object `sequence` of `track`.
    ///
    /// Audio is carried on the datagram lane: `body` is one `tilo-voice/1`
    /// object and only its payload is sealed, so the relay still forwards a
    /// well-formed voice object and never sees the audio itself. Everything
    /// else is one sealed object per group.
    ///
    /// # Errors
    ///
    /// [`PlaneError::Refused`] when the scope's keys do not cover this
    /// device's `track`, [`PlaneError::Ended`] when the session has ended,
    /// [`PlaneError::Invalid`] when `body` is not a voice object of the size
    /// the audio lane carries, and [`PlaneError::Unavailable`] when the
    /// broadcast is gone.
    pub fn publish(&self, track: Track, sequence: u32, body: &[u8]) -> Result<(), PlaneError> {
        if self.shared.ended() {
            return Err(PlaneError::Ended);
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
        let mut producer = self
            .shared
            .published
            .lock_recover()
            .get(name.as_str())
            .cloned()
            .ok_or(PlaneError::Unavailable)?;
        if track == Track::Audio {
            let object = VoiceObject::decode(body).map_err(|_| PlaneError::Invalid)?;
            let at = object.timestamp;
            let payload = sealer.seal(sequence, &object.payload)?;
            let object = VoiceObject { payload, ..object }
                .encode()
                .map_err(|_| PlaneError::Invalid)?;
            // The objects before this one again, as far as one datagram
            // carries them: a subscriber that lost the previous packet plays
            // it from this one.
            let mut sent = self.sent.lock_recover();
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
            drop(sent);
            let timestamp = moq_net::Timestamp::from_scale(at, u64::from(voice::SAMPLE_RATE))
                .unwrap_or(moq_net::Timestamp::ZERO);
            return producer
                .insert_datagram(u64::from(sequence), timestamp, bundle)
                .map_err(|_| PlaneError::Unavailable);
        }
        let frame = sealer.seal(sequence, body)?;
        let mut group = producer
            .create_group(moq_net::group::Info::from(u64::from(sequence)))
            .map_err(|_| PlaneError::Unavailable)?;
        let timestamp = moq_net::Timestamp::from_scale(u64::from(sequence), 1)
            .unwrap_or(moq_net::Timestamp::ZERO);
        group
            .write_frame(timestamp, frame)
            .map_err(|_| PlaneError::Unavailable)?;
        group.finish().map_err(|_| PlaneError::Unavailable)?;
        Ok(())
    }

    /// The next object heard from a peer, or `None` when none is queued.
    #[must_use]
    pub fn try_recv(&self) -> Option<Heard> {
        self.shared.heard.lock_recover().pop_front()
    }

    /// How many objects this plane opened from the peers it reads.
    #[must_use]
    pub fn heard_total(&self) -> u64 {
        self.shared.heard_total.load(Ordering::Relaxed)
    }

    /// Why this plane could not open what it read, by reason and count: what a
    /// member shows when it is in a scope and hears nobody who is in it.
    #[must_use]
    pub fn refusals(&self) -> BTreeMap<String, usize> {
        self.shared.refusals.lock_recover().clone()
    }

    /// Show `tracks` of `sender` here, and stop showing those left out: this
    /// plane asks the peer for exactly these picture layers, and its session
    /// encodes a layer only while a peer asks for it. A track is read from
    /// the peer's own broadcast, so what arrives is [`Shared::deliver`]ed as
    /// that peer's objects.
    pub fn show(&self, sender: [u8; 32], tracks: &[Track]) {
        let wanted: BTreeSet<Track> = tracks
            .iter()
            .copied()
            .filter(|track| matches!(track, Track::Camera(_) | Track::Screen(_)))
            .collect();
        let mut shown = self.shown.lock_recover();
        let stale: Vec<([u8; 32], Track)> = shown
            .keys()
            .filter(|(device, track)| *device == sender && !wanted.contains(track))
            .copied()
            .collect();
        for key in stale {
            // Dropping the reader's task ends it: the peer is no longer
            // asked for that layer.
            shown.remove(&key);
        }
        if sender == self.shared.join.device {
            return;
        }
        for track in wanted {
            if shown.contains_key(&(sender, track)) {
                continue;
            }
            let reader = Task::spawn(receive_pictures(self.shared.clone(), sender, track));
            shown.insert((sender, track), reader);
        }
    }

    /// The picture layers someone shows of this device now: exactly what it
    /// encodes. Changes with [`Self::pictures_revision`].
    #[must_use]
    pub fn wanted_pictures(&self) -> Vec<(Source, Layer)> {
        self.shared
            .pictures
            .lock_recover()
            .out
            .keys()
            .filter_map(|name| video::parse_track(name))
            .collect()
    }

    /// Whether a layer of `source` needs a keyframe now, as
    /// [`Self::take_picture_keyframes`] would take.
    #[must_use]
    pub fn wants_picture_keyframe(&self, source: Source) -> bool {
        self.shared
            .pictures
            .lock_recover()
            .keyframes
            .iter()
            .any(|track| matches!(track, video::Track::Picture(of, _) if *of == source))
    }

    /// The layers of `source` that need a keyframe now: their tracks were
    /// asked for afresh, or a picture could not join its group. A still
    /// picture sends its new reader nothing until asked.
    #[must_use]
    pub fn take_picture_keyframes(&self, source: Source) -> Vec<Layer> {
        let mut pictures = self.shared.pictures.lock_recover();
        let layers = pictures
            .keyframes
            .iter()
            .filter_map(|track| match track {
                video::Track::Picture(of, layer) if *of == source => Some(*layer),
                _ => None,
            })
            .collect();
        pictures
            .keyframes
            .retain(|track| !matches!(track, video::Track::Picture(of, _) if *of == source));
        layers
    }

    /// Advances whenever a picture layer is asked for or ends, or a picture
    /// could not join its group.
    #[must_use]
    pub fn pictures_revision(&self) -> u64 {
        self.shared.pictures_revision.load(Ordering::Relaxed)
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
    /// [`PlaneError::Unwanted`] when nobody shows this layer,
    /// [`PlaneError::Unkeyed`] for a picture before the layer's first
    /// keyframe or a group past [`video::MAX_GROUP_OBJECTS`] (send a
    /// keyframe), and otherwise as [`Self::publish`].
    pub fn send_picture(&self, source: Source, object: &VideoObject) -> Result<(), PlaneError> {
        if self.shared.ended() {
            return Err(PlaneError::Ended);
        }
        let track = match source {
            Source::Camera => Track::Camera(object.layer),
            Source::Screen => Track::Screen(object.layer),
        };
        let name = live::track(track);
        let keys = self.shared.keys.lock_recover().clone();
        let sealer = Sealer::new(
            &keys,
            self.shared.join.device,
            stream(track),
            TrackName::new(&self.shared.join.scope, name.as_str()),
        )?;
        let timestamp = moq_net::Timestamp::from_scale(object.timestamp_us, 1_000_000)
            .map_err(|_| PlaneError::Invalid)?;
        let mut pictures = self.shared.pictures.lock_recover();
        let Some(out) = pictures.out.get_mut(&name) else {
            return Err(PlaneError::Unwanted);
        };
        let cipher_body = sealer.seal(
            u32::try_from(out.next_group).unwrap_or(u32::MAX),
            &object.payload,
        )?;
        let object = VideoObject {
            payload: cipher_body,
            ..object.clone()
        };
        let bytes = object.encode().map_err(|_| PlaneError::Invalid)?;
        let sent = out.write(object.keyframe, timestamp, bytes);
        if sent.is_err() {
            pictures
                .keyframes
                .insert(video::Track::Picture(source, object.layer));
            drop(pictures);
            self.shared.pictures_moved();
        }
        sent
    }

    /// Who may send from now on, under the same keys: a roster that moved
    /// while the epoch stayed as it was.
    ///
    /// A device that joined the roster is read from now on: a plane reads the
    /// devices it knew when it started, so without this a member who joins
    /// after it is never heard.
    pub fn set_roster(&self, roster: Roster) {
        self.shared.opener.lock_recover().set_roster(roster.clone());
        *self.shared.roster.lock_recover() = roster;
        read_roster(&self.shared, &mut self.readers.lock_recover());
    }

    /// Seal and open under `keys` from now on: a channel whose epoch moved.
    pub fn set_keys(&self, keys: ChannelContentKeys) {
        let roster = self.shared.roster.lock_recover().clone();
        *self.shared.keys.lock_recover() = keys.clone();
        *self.shared.opener.lock_recover() = Opener::new(keys, roster);
    }

    /// Ask `wake` to run whenever an object is heard or the session ends.
    pub fn set_wake(&self, wake: Arc<dyn Fn() + Send + Sync>) {
        *self.shared.wake.lock_recover() = Some(wake);
    }

    /// Ask `ended` to run once, when the plane ends: the carrier closed, the
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

impl Drop for Plane {
    fn drop(&mut self) {
        self.shared.end();
    }
}

/// The stream a plane track's frames are keyed and sealed as.
#[must_use]
pub const fn stream(track: Track) -> Stream {
    match track {
        Track::Presence => Stream::Presence,
        Track::Audio => Stream::Microphone,
        Track::Camera(_) => Stream::Camera,
        Track::Screen(_) => Stream::Screen,
        Track::Window(..) => Stream::Window,
        Track::Input(_) | Track::Activity => Stream::Activity,
    }
}

/// Retention of one published object at the relay.
fn retain(track: Track) -> Duration {
    match live::delivery(track) {
        Delivery::Datagram { retain, .. } | Delivery::Keyed { retain, .. } => retain,
        Delivery::Stream { max_age } | Delivery::Latest { max_age } => max_age,
    }
}

/// The track's info, as the relay republishes it: its retention and priority.
fn track_info(track: Track) -> moq_net::track::Info {
    moq_net::track::Info::default()
        .with_max_age(retain(track))
        .with_priority(live::priority(track))
}

/// Serve the peers' requests for this device's picture tracks: a request for
/// a camera or screen layer is accepted and encoded from then on, a track
/// nobody reads past its grace is ended
/// ([`watch_unused`]), and anything else is refused.
async fn accept_pictures(shared: Arc<Shared>, mut requests: moq_net::broadcast::Dynamic) {
    while let Ok(request) = requests.requested_track().await {
        let Some(track) = video::Track::parse(request.name()) else {
            request.reject(moq_net::Error::NotFound);
            continue;
        };
        let video::Track::Picture(source, layer) = track else {
            request.reject(moq_net::Error::NotFound);
            continue;
        };
        if shared.ended() {
            request.reject(moq_net::Error::Cancel);
            return;
        }
        let name = request.name().to_owned();
        let plane_track = match source {
            Source::Camera => Track::Camera(layer),
            Source::Screen => Track::Screen(layer),
        };
        let out = request.accept(track_info(plane_track));
        let unused = watch_unused(&shared, name.clone(), out.clone());
        let mut pictures = shared.pictures.lock_recover();
        let next_group = pictures.next_group.get(&name).copied().unwrap_or(0);
        pictures.out.insert(
            name,
            PictureOut {
                track: out,
                group: None,
                next_group,
                in_group: 0,
                _unused: unused,
            },
        );
        // The new reader can start at nothing but a keyframe.
        pictures.keyframes.insert(track);
        drop(pictures);
        shared.pictures_moved();
    }
}

/// A picture track nobody reads any more is ended and no longer encoded. A
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
            let mut pictures = shared.pictures.lock_recover();
            if !pictures
                .out
                .get(&name)
                .is_some_and(|out| out.track.is_clone(&track))
            {
                return;
            }
            if track.clone().abort_unused(moq_net::Error::Cancel).is_err() {
                continue;
            }
            if let Some(out) = pictures.out.remove(&name) {
                pictures.next_group.insert(name, out.next_group);
            }
            drop(pictures);
            shared.pictures_moved();
            return;
        }
    })
}

/// Read one shown picture track of `device`'s broadcast for as long as it is
/// shown. Asking the broadcast for the track is the demand: the peer encodes
/// that layer while this reader exists, and stops when it ends.
async fn receive_pictures(shared: Arc<Shared>, device: [u8; 32], track: Track) {
    let address = Namespace::new(&shared.join.scope, device).path();
    let name = live::track(track);
    loop {
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
        let Ok(peer_track) = broadcast.track(name.as_str()) else {
            return;
        };
        let Ok(mut subscriber) = peer_track
            .subscribe(moq_net::track::Subscription::default().with_max_age(retain(track)))
            .await
        else {
            return;
        };
        // Whether the track delivered anything before it ended: an ended
        // layer is asked for again only after the grace, not in a spin.
        let mut heard_any = false;
        while let Ok(Some(mut group)) = subscriber.recv_group().await {
            while let Ok(Some(frame)) = group.read_frame().await {
                if shared.ended() || !shared.admits(device) {
                    return;
                }
                let Some((sequence, body)) = open_picture(&shared, device, track, &frame.payload)
                else {
                    // A frame this device may not open: the epoch or the
                    // roster moved.
                    return;
                };
                heard_any = true;
                shared.deliver_picture(Heard {
                    sender: device,
                    track,
                    sequence,
                    body,
                });
            }
        }
        if !heard_any {
            n0_future::time::sleep(REJOIN).await;
        }
    }
}

/// Open one picture object's sealed payload, keeping the header the relay
/// validated: `None` when the epoch or the roster no longer opens it.
fn open_picture(
    shared: &Shared,
    device: [u8; 32],
    track: Track,
    bytes: &[u8],
) -> Option<(u32, Vec<u8>)> {
    let object = VideoObject::decode(bytes).ok()?;
    let opened = shared.open(device, track, &object.payload).ok()?;
    let sequence = u32::try_from(object.sequence).unwrap_or(u32::MAX);
    let body = VideoObject {
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

/// Read `track` of `device`'s broadcast for as long as the plane lives.
///
/// One subscription per broadcast, held while the device publishes: a frame
/// this plane cannot open yet is passed over, and the next opens once
/// [`Plane::set_keys`] caught up. A device's broadcast ends when its plane does — it left the workspace and
/// came back, or a page reloaded — and is announced again under the same
/// namespace by the session that took its place. This reader waits for that
/// announcement and reads the track again, so the device is heard here
/// without this plane joining again; what it said before is not played twice.
async fn receive(shared: Arc<Shared>, device: [u8; 32], track: Track) {
    let address = Namespace::new(&shared.join.scope, device).path();
    let name = live::track(track);
    loop {
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
        // A reader that attaches late starts at the newest group the track
        // holds: the presence a peer said before it joined is what it draws,
        // instead of waiting out the heartbeat.
        let subscribed = match broadcast.track(name.as_str()) {
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
        if let Some(mut subscriber) = subscribed
            && read(&shared, device, track, &mut subscriber).await
            && !shared.ended()
        {
            // The device's broadcast went away with its session after it said
            // itself: whoever drew it takes it down now, rather than once it
            // has been silent long enough. A device that comes back says itself
            // again on the broadcast that takes its place.
            shared.deliver(Heard::left(device));
        }
        // The subscription ended with the broadcast: the one that takes its
        // place is read once announced.
        let _ = broadcast.closed().await;
    }
}

/// Read one subscription of `track` until it ends, the plane is over or the
/// device left the roster: `true` when a presence track that delivered a
/// frame ended with its broadcast. A frame this plane cannot open is counted
/// ([`Plane::refusals`]) and passed over: the subscription stays, and the
/// device's next frame is opened under whatever keys and roster the plane
/// holds by then.
async fn read(
    shared: &Shared,
    device: [u8; 32],
    track: Track,
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
                    if hear_voice(shared, device, &object) {
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
                        // Group lanes carry one voice object per frame: the
                        // same object the datagram lane carries.
                        let Ok(object) = VoiceObject::decode(&frame.payload) else {
                            continue;
                        };
                        if played.is_some_and(|seen| object.sequence <= seen) {
                            continue;
                        }
                        if hear_voice(shared, device, &object) {
                            played = Some(object.sequence);
                        }
                        continue;
                    }
                    match shared.open(device, track, &frame.payload) {
                        Ok(body) => {
                            said |= track == Track::Presence;
                            shared.deliver(Heard {
                                sender: device,
                                track,
                                sequence,
                                body,
                            });
                        }
                        Err(refusal) => shared.refused(&refusal),
                    }
                }
            }
            Next::End => return said,
        }
    }
}

/// Open one voice object's payload and deliver the object: `false` when the
/// epoch or the roster no longer opens it, which ends the read.
fn hear_voice(shared: &Shared, device: [u8; 32], object: &VoiceObject) -> bool {
    let opened = match shared.open(device, Track::Audio, &object.payload) {
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

/// What a plane's reader, or the relay's copy, sees next on a subscribed
/// track.
pub(crate) enum Next {
    Datagram(moq_net::Datagram),
    Group(Box<moq_net::group::Consumer>),
    End,
}

/// The next object on `track`, from either lane. A lane that ends or breaks
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
