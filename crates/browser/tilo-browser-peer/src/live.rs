//! The page's device in the live plane.
//!
//! A joined page is a participant like a native device: it opens one
//! `moq-lite` session over the relay's WebTransport listener, sealed as its
//! own device (not as any device that bridges anything), publishes its
//! presence and microphone into its own namespace of the workspace scope, and
//! opens what the other participants publish. The relay copies sealed bytes
//! and holds no key.
//!
//! The session is the page's carrier ([`tilo_live::admit`]): the same preface,
//! the same `moq-lite` setup and the same track names as a native device's,
//! sealed under the scope's own keys as the call lane seals a call's frames.
//! The lane names are the live plane's: [`Track::Presence`] as
//! `Stream::Presence` on `presence`, [`Track::Audio`] as `Stream::Microphone`
//! on `audio`.
//!
//! A browser without WebTransport, or without WebTransport subprotocol
//! negotiation, has no way to join — there is no polling fallback — so
//! [`LiveSession::open`] answers [`LiveError::Unsupported`], whose message is
//! what the page shows.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bytes::Bytes;
use moq_net::transport::poll::Session as Transport;
use tilo_call_lane::{Opener, Roster, Sealer, Stream, TrackName};
use tilo_live::admit::{self, AdmissionError, Connected, OriginTask, origin_config};
use tilo_live::live::{self, Namespace, Track};
use tilo_live::scope::Scope;
use tilo_live::voice::{self, VoiceObject};

use crate::ChannelContentKeys;

/// Bytes an origin may cache of this page's own publication.
const PUBLISH_BYTES: u64 = 4 * 1024 * 1024;
/// Bytes an origin may cache of what the other participants published.
const RECEIVE_BYTES: u64 = 16 * 1024 * 1024;
/// How long this page's objects stay readable after they are written.
const PUBLISH_RETAIN: Duration = Duration::from_secs(30);
/// How long received objects stay readable.
const RECEIVE_RETAIN: Duration = Duration::from_secs(2);
/// How long a received audio object is worth playing.
const AUDIO_RETAIN: Duration = Duration::from_millis(200);
/// How long a participant's presence stays readable.
const PRESENCE_RETAIN: Duration = Duration::from_secs(30);
/// Opened frames held for [`LiveSession::next`]; the oldest go first.
const QUEUE: usize = 512;

/// The message a page shows when its browser cannot join live.
pub const UNSUPPORTED: &str = "this browser cannot join live";

/// Why the page could not join, or stopped being part of, the live plane.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum LiveError {
    /// The browser has no WebTransport, or no WebTransport subprotocol
    /// negotiation. There is no fallback: the page joins nothing.
    #[error("{UNSUPPORTED}")]
    Unsupported,
    /// The host refused the page's grant, or revoked its session.
    #[error("the host refused the page's live grant")]
    Denied,
    /// The carrier could not be reached, or did not finish its setup.
    #[error("the live carrier is unavailable")]
    Unavailable,
    /// The transport does not speak the live carrier's protocol.
    #[error("the transport does not speak the live carrier's protocol")]
    WrongProtocol,
    /// The scope's keys cannot seal or open this device's streams.
    #[error("the page holds no media key for this scope")]
    NoKey,
    /// The plane refused the page's namespace or track.
    #[error("the live plane refused the page's namespace")]
    Refused,
}

/// One frame from another participant: its payload after its sender's key
/// opened it, and which participant and track it came from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Opened {
    /// The device that sealed it, which the roster had to admit.
    pub sender: [u8; 32],
    /// The live track it arrived on.
    pub track: Track,
    /// The publisher's sequence number within that track.
    pub sequence: u32,
    /// Presence bytes, or one Opus frame.
    pub payload: Vec<u8>,
}

/// The page's device in one scope's live plane.
pub struct LiveSession {
    shared: Arc<Shared>,
    tasks: Mutex<Vec<n0_future::task::JoinHandle<()>>>,
    connected: Mutex<Option<Connected>>,
    _origins: [OriginTask; 2],
}

/// What the session's tasks share.
struct Shared {
    scope: Scope,
    device: [u8; 32],
    namespace: Namespace,
    presence: Mutex<Sealer>,
    audio: Mutex<Sealer>,
    opener: Mutex<Opener>,
    /// This page's broadcast, kept alive for as long as the page publishes:
    /// the namespace ends when its last broadcast producer goes away, and a
    /// track producer does not hold it open.
    broadcast: moq_net::broadcast::Producer,
    presence_track: moq_net::track::Producer,
    audio_track: Mutex<moq_net::track::Producer>,
    /// This page's audio objects, newest last: its redundancy channel.
    sent: Mutex<VecDeque<Vec<u8>>>,
    presence_sequence: Mutex<u32>,
    opened: Mutex<VecDeque<Opened>>,
    closed: Mutex<bool>,
    /// End this page's transport at once, in every target.
    hang_up: Box<dyn Fn() + Send + Sync>,
}

impl Shared {
    fn presence_name(&self) -> TrackName {
        TrackName::new(&self.scope, live::track(Track::Presence).as_str())
    }

    fn audio_name(&self) -> TrackName {
        TrackName::new(&self.scope, live::track(Track::Audio).as_str())
    }

    fn seal_presence(&self, presence: &[u8]) -> Result<Vec<u8>, LiveError> {
        let sequence = {
            let mut next = lock(&self.presence_sequence);
            let sequence = *next;
            *next = sequence.wrapping_add(1);
            sequence
        };
        lock(&self.presence)
            .seal(sequence, presence)
            .map_err(|_| LiveError::NoKey)
    }

    fn open_presence(&self, sender: [u8; 32], frame: &[u8]) -> Option<Opened> {
        let payload = lock(&self.opener)
            .open(sender, Stream::Presence, &self.presence_name(), frame)
            .ok()?;
        Some(Opened {
            sender,
            track: Track::Presence,
            sequence: tilo_call_lane::frame::sequence(frame).unwrap_or(0),
            payload,
        })
    }

    fn open_audio(&self, sender: [u8; 32], sequence: u64, frame: &[u8]) -> Option<Opened> {
        let payload = lock(&self.opener)
            .open(sender, Stream::Microphone, &self.audio_name(), frame)
            .ok()?;
        Some(Opened {
            sender,
            track: Track::Audio,
            sequence: u32::try_from(sequence).unwrap_or(u32::MAX),
            payload,
        })
    }

    fn offer(&self, opened: Opened) {
        let mut queue = lock(&self.opened);
        if queue.len() >= QUEUE {
            queue.pop_front();
        }
        queue.push_back(opened);
    }

    fn is_closed(&self) -> bool {
        *lock(&self.closed)
    }
}

/// Lock `slot`, ignoring a poisoned lock: every guarded value stays valid
/// under a panic, and one panic must not leave the page unable to leave.
fn lock<T>(slot: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    slot.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl LiveSession {
    /// Dial the relay's WebTransport listener at `origin` (the
    /// [`LiveInfo::webtransport_origin`](tilo_relay_protocol::web::LiveInfo)
    /// that `/live` answered), accepting exactly the certificate whose SHA-256
    /// is `certificate_sha256` (base64, as that route reports it), and join
    /// `scope` with `grant`.
    ///
    /// # Errors
    ///
    /// [`LiveError::Unsupported`] in a browser without WebTransport, and the
    /// failures [`connect`](Self::connect) reports.
    pub async fn open(
        origin: &str,
        certificate_sha256: &str,
        grant: &[u8],
        scope: Scope,
        device: [u8; 32],
        keys: ChannelContentKeys,
        roster: Roster,
    ) -> Result<Self, LiveError> {
        let certificate = decode_sha256(certificate_sha256).ok_or(LiveError::Unavailable)?;
        let options = tilo_net::webtransport::Options::pinned(vec![certificate])
            .with_protocols(vec![tilo_live::preface::PROTOCOL.to_owned()]);
        let transport = tilo_net::webtransport::connect(origin, options)
            .await
            .map_err(|error| match error {
                tilo_net::NetError::Unsupported(_) => LiveError::Unsupported,
                _ => LiveError::Unavailable,
            })?;
        Self::connect(transport, grant, scope, device, keys, roster).await
    }

    /// Join `scope` on an open carrier `transport`, admitted by `grant`.
    ///
    /// The page seals as `device` (its own) and opens only frames from the
    /// devices `roster` admits.
    ///
    /// # Errors
    ///
    /// [`LiveError::Unsupported`] when `transport` negotiated no subprotocol,
    /// [`LiveError::WrongProtocol`] when it speaks another, [`LiveError::Denied`]
    /// when the host refuses the grant, [`LiveError::Unavailable`] when the
    /// carrier fails first, and [`LiveError::NoKey`] when `keys` cannot seal
    /// this device's streams.
    pub async fn connect<S>(
        transport: S,
        grant: &[u8],
        scope: Scope,
        device: [u8; 32],
        keys: ChannelContentKeys,
        roster: Roster,
    ) -> Result<Self, LiveError>
    where
        S: Transport + Send + Sync + 'static,
        S::SendStream: Send,
        S::RecvStream: Send,
    {
        protocol_gate(transport.protocol())?;
        let namespace = Namespace::new(&scope, device);
        let (presence, audio) = sealers(&scope, device, &keys)?;
        let (published, publish) = publishing(&namespace)?;
        let (subscribing, subscribe_task, subscribe) = reading(&namespace)?;
        let hang_up = disconnect(&transport);
        let connected = admit::start(transport, grant, Some(publish), Some(subscribing.clone()))
            .await
            .map_err(|error| match error {
                AdmissionError::Denied => LiveError::Denied,
                AdmissionError::Invalid | AdmissionError::Unavailable => LiveError::Unavailable,
            })?;

        let shared = Arc::new(Shared {
            scope,
            device,
            namespace,
            broadcast: published.broadcast,
            presence: Mutex::new(presence),
            audio: Mutex::new(audio),
            opener: Mutex::new(Opener::new(keys, roster)),
            presence_track: published.presence,
            audio_track: Mutex::new(published.audio),
            sent: Mutex::new(VecDeque::new()),
            presence_sequence: Mutex::new(0),
            opened: Mutex::new(VecDeque::new()),
            closed: Mutex::new(false),
            hang_up,
        });

        let mut tasks = Vec::new();
        tasks.push({
            let ended = connected.session.clone();
            let shared = shared.clone();
            n0_future::task::spawn(async move {
                let _ = ended.closed().await;
                *lock(&shared.closed) = true;
            })
        });
        tasks.push({
            let shared = shared.clone();
            n0_future::task::spawn(async move { read_participants(shared, subscribe).await })
        });
        Ok(Self {
            shared,
            tasks: Mutex::new(tasks),
            connected: Mutex::new(Some(connected)),
            _origins: [published.task, subscribe_task],
        })
    }

    /// Publish `presence` as this page's device: one sealed group on the
    /// scope's `presence` track.
    ///
    /// # Errors
    ///
    /// [`LiveError::Denied`] once the session ended, [`LiveError::NoKey`] when
    /// the presence cannot be sealed, and [`LiveError::Refused`] when the
    /// track refuses the group.
    pub fn publish_presence(&self, presence: &[u8]) -> Result<(), LiveError> {
        self.check()?;
        let sealed = self.shared.seal_presence(presence)?;
        let mut group = self
            .shared
            .presence_track
            .append_group()
            .map_err(|_| LiveError::Refused)?;
        group
            .write_frame(moq_net::Timestamp::ZERO, Bytes::from(sealed))
            .map_err(|_| LiveError::Refused)?;
        group.finish().map_err(|_| LiveError::Refused)
    }

    /// Publish one 20 ms Opus frame (`opus`, sequence `seq`) as this page's
    /// device: a sealed tilo-voice/1 object, sent as a datagram bundle with the
    /// objects before it, as a browser publisher sends them.
    ///
    /// # Errors
    ///
    /// [`LiveError::Denied`] once the session ended, [`LiveError::NoKey`] when
    /// the frame cannot be sealed, and [`LiveError::Refused`] when the track
    /// refuses the datagram.
    pub fn publish_audio(&self, opus: &[u8], seq: u32) -> Result<(), LiveError> {
        self.check()?;
        let payload = lock(&self.shared.audio)
            .seal(seq, opus)
            .map_err(|_| LiveError::NoKey)?;
        let timestamp = u64::from(seq).saturating_mul(u64::from(voice::FRAME_SAMPLES));
        let object = VoiceObject {
            sequence: u64::from(seq),
            timestamp,
            duration: voice::FRAME_SAMPLES,
            payload,
        }
        .encode()
        .map_err(|_| LiveError::NoKey)?;
        let mut sent = lock(&self.shared.sent);
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
        let timestamp = moq_net::Timestamp::from_scale(timestamp, u64::from(voice::SAMPLE_RATE))
            .map_err(|_| LiveError::Refused)?;
        lock(&self.shared.audio_track)
            .insert_datagram(u64::from(seq), timestamp, Bytes::from(bundle))
            .map_err(|_| LiveError::Refused)
    }

    /// The next frame another participant published that the roster admits
    /// and this page holds a key for, in arrival order.
    pub fn next(&self) -> Option<Opened> {
        lock(&self.shared.opened).pop_front()
    }

    /// Open only the frames from the devices `roster` now names.
    pub fn set_roster(&self, roster: Roster) {
        lock(&self.shared.opener).set_roster(roster);
    }

    /// Whether the host ended this page's session: a removal, or the host
    /// going away. A closed session publishes nothing more.
    pub fn is_closed(&self) -> bool {
        self.shared.is_closed()
    }

    /// Leave the plane: end the transport, stop reading and publishing.
    /// Idempotent.
    pub fn close(&self) {
        *lock(&self.shared.closed) = true;
        self.shared.broadcast.finish();
        (self.shared.hang_up)();
        drop(lock(&self.connected).take());
        for task in lock(&self.tasks).drain(..) {
            task.abort();
        }
    }

    fn check(&self) -> Result<(), LiveError> {
        if self.shared.is_closed() {
            return Err(LiveError::Denied);
        }
        Ok(())
    }
}

impl Drop for LiveSession {
    fn drop(&mut self) {
        self.close();
    }
}

/// The two sealers this page writes its tracks with, under `keys`.
fn sealers(
    scope: &Scope,
    device: [u8; 32],
    keys: &ChannelContentKeys,
) -> Result<(Sealer, Sealer), LiveError> {
    let presence = Sealer::new(
        keys,
        device,
        Stream::Presence,
        TrackName::new(scope, live::track(Track::Presence).as_str()),
    )
    .map_err(|_| LiveError::NoKey)?;
    let audio = Sealer::new(
        keys,
        device,
        Stream::Microphone,
        TrackName::new(scope, live::track(Track::Audio).as_str()),
    )
    .map_err(|_| LiveError::NoKey)?;
    Ok((presence, audio))
}

/// This page's publishing side: the broadcast it announces, the tracks it
/// writes, and the origin task that keeps them.
struct Published {
    broadcast: moq_net::broadcast::Producer,
    presence: moq_net::track::Producer,
    audio: moq_net::track::Producer,
    task: OriginTask,
}

/// Create this page's broadcast in `namespace`, with the consumer the carrier
/// announces it from.
fn publishing(namespace: &Namespace) -> Result<(Published, moq_net::origin::Consumer), LiveError> {
    let (origin, task) = OriginTask::produce(origin_config(PUBLISH_BYTES, PUBLISH_RETAIN));
    let broadcast = origin
        .create_broadcast(namespace.path().as_str())
        .map_err(|_| LiveError::Unavailable)?;
    let presence = broadcast
        .create_track(
            live::track(Track::Presence).as_str(),
            moq_net::track::Info::default().with_max_age(PUBLISH_RETAIN),
        )
        .map_err(|_| LiveError::Unavailable)?;
    let audio = broadcast
        .create_track(
            live::track(Track::Audio).as_str(),
            moq_net::track::Info::default().with_max_age(AUDIO_RETAIN),
        )
        .map_err(|_| LiveError::Unavailable)?;
    broadcast
        .announce(moq_net::origin::Route::default())
        .map_err(|_| LiveError::Refused)?;
    let pattern =
        moq_net::Pattern::literal(namespace.path().as_str()).map_err(|_| LiveError::Unavailable)?;
    let consume = origin
        .consume()
        .scope("", &moq_net::Patterns::from(pattern))
        .map_err(|_| LiveError::Unavailable)?;
    Ok((
        Published {
            broadcast,
            presence,
            audio,
            task,
        },
        consume,
    ))
}

/// The page's reading side: the origin the carrier subscribes from, the task
/// that keeps it, and the consumer that reads the scope's announcements.
fn reading(
    namespace: &Namespace,
) -> Result<
    (
        moq_net::origin::Producer,
        OriginTask,
        moq_net::origin::Consumer,
    ),
    LiveError,
> {
    let (origin, task) = OriginTask::produce(origin_config(RECEIVE_BYTES, RECEIVE_RETAIN));
    let subtree = moq_net::Pattern::subtree(namespace.scope_path().as_str())
        .map_err(|_| LiveError::Unavailable)?;
    let consume = origin
        .consume()
        .scope("", &moq_net::Patterns::from(subtree))
        .map_err(|_| LiveError::Unavailable)?;
    Ok((origin, task, consume))
}

/// End `transport` from anywhere, as [`LiveSession::close`] does.
fn disconnect<S: Transport + Send + 'static>(transport: &S) -> Box<dyn Fn() + Send + Sync> {
    let transport = Mutex::new(Some(transport.clone()));
    Box::new(move || {
        let mut session = lock(&transport).take();
        if let Some(session) = session.as_mut() {
            moq_net::web_transport_trait::poll::Session::close(session, 0, "");
        }
    })
}

/// Follow the scope's announcements and read every other participant's
/// tracks for as long as the session lasts.
async fn read_participants(shared: Arc<Shared>, incoming: moq_net::origin::Consumer) {
    let mut announced = incoming.announced();
    while let Some(update) = announced.next().await {
        let Some(namespace) = Namespace::parse(update.prefix.as_str()) else {
            continue;
        };
        if namespace.scope_path() != shared.namespace.scope_path()
            || namespace.device() == shared.device
            || !update.kind.is_active()
        {
            continue;
        }
        let owner = shared.clone();
        let incoming = incoming.clone();
        n0_future::task::spawn(async move { read_participant(owner, incoming, namespace).await });
    }
}

async fn read_participant(
    shared: Arc<Shared>,
    incoming: moq_net::origin::Consumer,
    namespace: Namespace,
) {
    let Ok(broadcast) = incoming.routed_broadcast(namespace.path().as_str()).await else {
        return;
    };
    let sender = namespace.device();
    if let Ok(track) = broadcast.track(live::track(Track::Presence).as_str()) {
        let owner = shared.clone();
        n0_future::task::spawn(async move { read_presence(owner, sender, track).await });
    }
    // A native publisher sends audio as groups, a browser publisher as
    // datagram bundles; a publisher uses one form, so both readers run.
    if let Ok(track) = broadcast.track(live::track(Track::Audio).as_str()) {
        let owner = shared.clone();
        n0_future::task::spawn(async move { read_audio(owner, sender, track).await });
    }
    if let Ok(track) = broadcast.track(live::track(Track::Audio).as_str()) {
        let owner = shared.clone();
        n0_future::task::spawn(async move { read_audio_bundles(owner, sender, track).await });
    }
}

async fn read_presence(shared: Arc<Shared>, sender: [u8; 32], track: moq_net::track::Consumer) {
    let Ok(mut subscriber) = track
        .subscribe(moq_net::track::Subscription::default().with_max_age(PRESENCE_RETAIN))
        .await
    else {
        return;
    };
    while let Ok(Some(mut group)) = subscriber.recv_group().await {
        while let Ok(Some(frame)) = group.read_frame().await {
            if shared.is_closed() {
                return;
            }
            if let Some(opened) = shared.open_presence(sender, &frame.payload) {
                shared.offer(opened);
            }
        }
    }
}

/// Read a publisher that sends its audio as groups, one object per frame.
async fn read_audio(shared: Arc<Shared>, sender: [u8; 32], track: moq_net::track::Consumer) {
    let Ok(mut subscriber) = track
        .subscribe(moq_net::track::Subscription::default().with_max_age(AUDIO_RETAIN))
        .await
    else {
        return;
    };
    let mut last = None;
    while let Ok(Some(mut group)) = subscriber.recv_group().await {
        while let Ok(Some(frame)) = group.read_frame().await {
            if shared.is_closed() {
                return;
            }
            let Ok(object) = VoiceObject::decode(&frame.payload) else {
                continue;
            };
            if last.is_some_and(|last| object.sequence <= last) {
                continue;
            }
            last = Some(object.sequence);
            if let Some(opened) = shared.open_audio(sender, object.sequence, &object.payload) {
                shared.offer(opened);
            }
        }
    }
}

/// Read a publisher that sends its audio as datagram bundles, newest object
/// first, then redundant earlier ones.
async fn read_audio_bundles(
    shared: Arc<Shared>,
    sender: [u8; 32],
    track: moq_net::track::Consumer,
) {
    let Ok(mut subscriber) = track
        .subscribe(moq_net::track::Subscription::default().with_max_age(AUDIO_RETAIN))
        .await
    else {
        return;
    };
    let mut last = None;
    while let Ok(Some(datagram)) = subscriber.recv_datagram().await {
        if shared.is_closed() {
            return;
        }
        let Ok(objects) = voice::bundle_objects(&datagram.payload) else {
            continue;
        };
        // Oldest first: an object recovered from another object's bundle
        // reaches the caller before the newer object that carried it.
        for bytes in objects.into_iter().rev() {
            let Ok(object) = VoiceObject::decode(bytes) else {
                continue;
            };
            if last.is_some_and(|last| object.sequence <= last) {
                continue;
            }
            last = Some(object.sequence);
            if let Some(opened) = shared.open_audio(sender, object.sequence, &object.payload) {
                shared.offer(opened);
            }
        }
    }
}

/// The gate on the session's negotiated subprotocol: the page joins only a
/// carrier that speaks the live plane, and a browser that negotiated none has
/// no fallback.
fn protocol_gate(protocol: Option<&str>) -> Result<(), LiveError> {
    match protocol {
        Some(tilo_live::preface::PROTOCOL) => Ok(()),
        None => Err(LiveError::Unsupported),
        Some(_) => Err(LiveError::WrongProtocol),
    }
}

/// The 32 bytes a base64 SHA-256 digest names.
fn decode_sha256(digest: &str) -> Option<[u8; 32]> {
    use base64::Engine as _;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(digest.trim())
        .ok()?;
    bytes.try_into().ok()
}

#[cfg(test)]
mod tests {
    use super::{LiveError, UNSUPPORTED, protocol_gate};

    #[test]
    fn a_browser_that_negotiates_no_subprotocol_cannot_join() {
        assert_eq!(protocol_gate(None), Err(LiveError::Unsupported));
        assert_eq!(LiveError::Unsupported.to_string(), UNSUPPORTED);
        assert_eq!(
            protocol_gate(Some("moq-lite-05")),
            Err(LiveError::WrongProtocol)
        );
        assert_eq!(protocol_gate(Some(tilo_live::preface::PROTOCOL)), Ok(()));
    }
}
