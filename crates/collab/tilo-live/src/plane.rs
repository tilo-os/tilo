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
//! The carrier is whatever [`Dial`] this device has: iroh to the workspace's
//! relay ([`crate::native::Iroh`]), or a page's WebTransport session. This
//! module names neither.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::task::Poll;
use std::time::Duration;

use channel_crypto::ChannelContentKeys;
use moq_net::origin::{Consumer, Producer};
use tilo_call_lane::frame::{Opener, Refusal, Sealer, Stream, TrackName};
use tilo_call_lane::roster::Roster;
use tilo_lock::MutexRecover as _;

use crate::Task;
use crate::admit::{AdmissionError, Connected, Dial, OriginTask, origin_config};
use crate::live::{self, Delivery, Namespace, Track, exact};
use crate::scope::Scope;
use crate::voice::{self, VoiceObject};

/// The tracks one plane carries: the workspace's presence, a call's audio and
/// the activity note. Pictures keep their own lane ([`crate::client`]) until
/// they are carried here too.
pub const TRACKS: [Track; 3] = [Track::Presence, Track::Audio, Track::Activity];

/// Objects one plane keeps before it drops the oldest: a jitter buffer, not a
/// backlog.
const QUEUE: usize = 256;
/// Join budget: the transport, the admission preface and the setup.
const JOINT: Duration = Duration::from_secs(10);
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
    /// The opened body.
    pub body: Vec<u8>,
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
    published: Mutex<std::collections::BTreeMap<String, moq_net::track::Producer>>,
    /// This device's broadcast, kept for as long as the plane lives: dropping
    /// it withdraws the announcement its subscribers are reading.
    _broadcast: Option<moq_net::broadcast::Producer>,
    heard: Mutex<VecDeque<Heard>>,
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
        let (broadcast, published) = if join.publish {
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
            broadcast
                .announce(moq_net::origin::Route::default())
                .map_err(|_| PlaneError::Unavailable)?;
            (Some(broadcast), published)
        } else {
            (None, std::collections::BTreeMap::new())
        };
        Ok(Arc::new(Self {
            opener: Mutex::new(Opener::new(join.keys.clone(), join.roster.clone())),
            keys: Mutex::new(join.keys.clone()),
            roster: Mutex::new(join.roster.clone()),
            join: join.clone(),
            incoming: incoming.clone(),
            published: Mutex::new(published),
            _broadcast: broadcast,
            heard: Mutex::new(VecDeque::new()),
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
        self.wake();
    }
    /// Would `device`'s frame be opened now? Decided by the roster and the
    /// keys, before any byte of it is read.
    fn admits(&self, device: [u8; 32]) -> bool {
        let roster = self.roster.lock_recover();
        roster.admits(device)
    }
    fn open(&self, sender: [u8; 32], track: Track, frame: &[u8]) -> Result<Vec<u8>, Refusal> {
        let name = live::track(track);
        let name = TrackName::new(&self.join.scope, name.as_str());
        let opener = self.opener.lock_recover();
        opener.open(sender, stream(track), &name, frame)
    }
}

/// One device's sealed session in one scope.
pub struct Plane {
    shared: Arc<Shared>,
    /// This device's recent audio objects, newest last: the redundancy its
    /// datagrams carry, so one lost packet costs one frame and not two.
    sent: Mutex<VecDeque<Vec<u8>>>,
    _readers: Vec<Task>,
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
        if let Some(connected) = &connected {
            let watched = connected.session.clone();
            let ended = shared.clone();
            readers.push(Task::spawn(async move {
                let _ = watched.closed().await;
                ended.end();
            }));
        }
        for device in shared.join.roster.devices() {
            if device == shared.join.device {
                continue;
            }
            for track in TRACKS {
                readers.push(Task::spawn(receive(shared.clone(), device, track)));
            }
        }
        Self {
            shared,
            sent: Mutex::new(VecDeque::new()),
            _readers: readers,
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

    /// Who may send from now on, under the same keys: a roster that moved
    /// while the epoch stayed as it was.
    pub fn set_roster(&self, roster: Roster) {
        self.shared.opener.lock_recover().set_roster(roster.clone());
        *self.shared.roster.lock_recover() = roster;
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
        _ => Stream::Activity,
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

/// Read `track` of `device`'s broadcast for as long as the plane lives.
async fn receive(shared: Arc<Shared>, device: [u8; 32], track: Track) {
    let address = Namespace::new(&shared.join.scope, device).path();
    let Ok(broadcast) = shared
        .incoming
        .consume()
        .routed_broadcast(address.as_str())
        .await
    else {
        return;
    };
    let name = live::track(track);
    let Ok(producer) = broadcast.track(name.as_str()) else {
        return;
    };
    // A reader that attaches late starts at the newest group the track holds:
    // the presence a peer said before it joined is what it draws, instead of
    // waiting out the heartbeat.
    let newest = producer.latest();
    let Ok(mut subscriber) = producer
        .subscribe(
            moq_net::track::Subscription::default()
                .with_max_age(retain(track))
                .with_start(newest.map(moq_net::track::Position::group)),
        )
        .await
    else {
        return;
    };
    // The newest audio object played: a bundle carries earlier objects again,
    // and one already played is not played twice.
    let mut played: Option<u64> = None;
    loop {
        match next(&mut subscriber).await {
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
                        return;
                    }
                    if !hear_voice(&shared, device, &object) {
                        return;
                    }
                    played = Some(object.sequence);
                }
            }
            Next::Group(mut group) => {
                let sequence = u32::try_from(group.sequence).unwrap_or(u32::MAX);
                while let Ok(Some(frame)) = group.read_frame().await {
                    if shared.ended() || !shared.admits(device) {
                        return;
                    }
                    if track == Track::Audio {
                        // Group lanes carry one voice object per frame: the
                        // same object the datagram lane carries.
                        let Ok(object) = VoiceObject::decode(&frame.payload) else {
                            return;
                        };
                        if played.is_some_and(|seen| object.sequence <= seen) {
                            continue;
                        }
                        if !hear_voice(&shared, device, &object) {
                            return;
                        }
                        played = Some(object.sequence);
                        continue;
                    }
                    match shared.open(device, track, &frame.payload) {
                        Ok(body) => shared.deliver(Heard {
                            sender: device,
                            track,
                            sequence,
                            body,
                        }),
                        // A frame this device may not open is not one to keep
                        // reading the track for: the epoch or the roster moved.
                        Err(_) => return,
                    }
                }
            }
            Next::End => return,
        }
    }
}

/// Open one voice object's payload and deliver the object: `false` when the
/// epoch or the roster no longer opens it, which ends the read.
fn hear_voice(shared: &Shared, device: [u8; 32], object: &VoiceObject) -> bool {
    let Ok(opened) = shared.open(device, Track::Audio, &object.payload) else {
        return false;
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

/// What a plane's reader sees next on one of a peer's tracks.
enum Next {
    Datagram(moq_net::Datagram),
    Group(Box<moq_net::group::Consumer>),
    End,
}

/// The next object on `track`, from either lane. A lane that ends or breaks
/// ends the read: the carrier reconnects.
async fn next(track: &mut moq_net::track::Subscriber) -> Next {
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
