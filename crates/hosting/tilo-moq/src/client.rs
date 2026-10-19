//! Bounded native call transport: voice, and the pictures (camera, screen)
//! asked of this participant and shown of the others ([`crate::video`]).
//! Connection values are inert; the privileged native owner retains keys,
//! grants, descriptor custody and current policy.
use crate::{
    carrier,
    relay::{Direction, VOICE_TRACK},
    video::{self, Layer, Source, VideoObject},
    voice::{self, VoiceObject},
};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tilo_lock::MutexRecover as _;

const SEND_AGE: Duration = Duration::from_millis(100);
const RECEIVE_AGE: Duration = Duration::from_millis(200);
const TICK: Duration = Duration::from_millis(10);
/// Transport + WebTransport + admission preface + SETUP budget. High-RTT links
/// (satellite, ~600 ms) need several round trips plus a loss recovery.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// Bytes either local origin may cache.
const ORIGIN_BYTES: u64 = 64 * 1024;
const RECEIVE_FRAMES: usize = 10;
/// How long a sent picture stays worth delivering: a late subscriber starts
/// at the newest keyframe group within it.
const PICTURE_SEND_AGE: Duration = Duration::from_secs(2);
const PICTURE_RECEIVE_AGE: Duration = Duration::from_secs(2);
/// Pictures queued per shown track before the queue starts over at the next
/// keyframe: a decoder never sees a picture whose reference was dropped.
const PICTURES_QUEUED: usize = 8;
/// How soon a publication whose reader ended is read again while the relay
/// still offers it.
const REREAD: Duration = Duration::from_millis(200);

/// Exact immutable carrier scope checked against the actual native owner.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConnectionScope {
    pub endpoint: String,
    /// Empty selects system certificate verification. Nonempty pins exact
    /// certificates supplied through the native owner's trusted configuration.
    pub fingerprints: Vec<String>,
    pub owner_epoch: [u8; 32],
    pub carrier_generation: u64,
    pub publication: Option<String>,
    /// The call this carrier hears (`<call hex>`): every publication directly
    /// beneath it that the owner admits at the time, read as the relay
    /// offers it. Who is in the call is not part of the scope, so people
    /// joining and leaving never replace the carrier.
    pub subscription_prefix: Option<String>,
}
/// Inert one-use transport input, never a grant or local microphone consent.
pub struct Connection {
    pub scope: ConnectionScope,
    pub authorization: Vec<u8>,
}
/// Installed only by the privileged native device owner, never by app input.
/// Implementations bind every scope field to retained authenticated custody,
/// and revalidate current policy, identity, grants, presence and generations.
///
/// `current(scope, Subscribe, prefix)` with the scope's own
/// [`ConnectionScope::subscription_prefix`] asks whether the carrier may
/// hear the call at all; with a publication beneath it, whether that one
/// publication may be heard now.
pub trait ClientAuthority: Send + Sync {
    fn current(&self, scope: &ConnectionScope, direction: Direction, namespace: &str) -> bool;
    /// Advances whenever what `current` answers may have changed (a new
    /// grant): the carrier then asks again about each publication it reads.
    fn revision(&self) -> u64 {
        0
    }
}
/// One captured Opus packet. Transport assigns wire sequence and group numbers.
pub struct CapturedOpus {
    pub timestamp_48k: u64,
    pub duration_samples: u16,
    pub payload: Vec<u8>,
    pub captured_at: Instant,
}
/// Namespace already commits to the remote participation generation. The
/// local carrier generation additionally fences decoder state after reconnect.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct PublicationId {
    pub namespace: String,
    pub carrier_generation: u64,
    /// Which time this carrier read the namespace: a publisher that leaves
    /// and returns restarts its sequence, so its return is a new publication.
    pub incarnation: u64,
}
/// One picture received from another participant.
pub struct ReceivedPicture {
    pub publisher: PublicationId,
    pub source: Source,
    pub object: VideoObject,
    pub received_at: Instant,
}
/// One picture track this carrier publishes because the relay asked for it.
struct PictureOut {
    track: moq_net::track::Producer,
    group: Option<moq_net::group::Producer>,
    next_group: u64,
    in_group: u64,
}
impl PictureOut {
    /// Write one encoded picture: a keyframe starts a group.
    fn write(
        &mut self,
        keyframe: bool,
        timestamp: moq_net::Timestamp,
        bytes: Vec<u8>,
    ) -> Result<(), ClientError> {
        if keyframe {
            if let Some(group) = self.group.take() {
                let _ = group.finish();
            }
            self.group = Some(
                self.track
                    .create_group(moq_net::group::Info::from(self.next_group))
                    .map_err(|_| ClientError::Capacity)?,
            );
            self.next_group = self.next_group.saturating_add(1);
            self.in_group = 0;
        }
        if self.in_group >= video::MAX_GROUP_OBJECTS {
            if let Some(group) = self.group.take() {
                let _ = group.finish();
            }
            return Err(ClientError::Capacity);
        }
        let group = self.group.as_mut().ok_or(ClientError::Expired)?;
        group
            .write_frame(timestamp, bytes)
            .map_err(|_| ClientError::Capacity)?;
        self.in_group = self.in_group.saturating_add(1);
        Ok(())
    }
}
/// The pictures received of one shown track.
#[derive(Default)]
struct PictureQueue {
    pictures: VecDeque<ReceivedPicture>,
    /// The queue overflowed: nothing until the next keyframe.
    awaiting_keyframe: bool,
}
impl PictureQueue {
    /// Queue `picture`, or start over at the next keyframe when behind,
    /// rather than hand a decoder a picture whose reference is gone.
    fn admit(&mut self, picture: ReceivedPicture) {
        if self.awaiting_keyframe && !picture.object.keyframe {
            return;
        }
        self.awaiting_keyframe = false;
        if self.pictures.len() >= PICTURES_QUEUED {
            self.pictures.clear();
            if !picture.object.keyframe {
                self.awaiting_keyframe = true;
                return;
            }
        }
        self.pictures.push_back(picture);
    }
}
pub struct ReceivedOpus {
    pub publisher: PublicationId,
    pub group: u64,
    pub object: VoiceObject,
    pub received_at: Instant,
    pub expires_at: Instant,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Connecting,
    Live,
    Denied,
    Failed,
    Closed,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ClientError {
    #[error("invalid native media connection")]
    Invalid,
    #[error("native media authority denied")]
    Denied,
    #[error("native media transport unavailable")]
    Unavailable,
    #[error("native media publication is muted")]
    Muted,
    #[error("native media frame expired")]
    Expired,
    #[error("native media capacity exceeded")]
    Capacity,
}
/// Content-free transport diagnostics; these do not establish audio hardware readiness.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counters {
    pub sent: u64,
    pub received: u64,
    pub late_dropped: u64,
    pub duplicate_dropped: u64,
    pub capacity_dropped: u64,
    pub queued_received: usize,
}
struct SentGroup {
    /// When the group's newest frame goes stale. Aborting a group discards
    /// every frame the transport has not drained yet, so a group lives as long
    /// as its newest frame, not its oldest.
    expires: Instant,
    group: moq_net::group::Producer,
    count: u64,
}
struct State {
    status: Status,
    muted: bool,
    sequence: u64,
    session: Option<carrier::Connected>,
    broadcast: Option<moq_net::broadcast::Producer>,
    track: Option<moq_net::track::Producer>,
    groups: VecDeque<SentGroup>,
    received: BTreeMap<String, VecDeque<ReceivedOpus>>,
    /// The publications read now, by namespace, with their incarnation.
    reading: BTreeMap<String, u64>,
    /// Advances whenever `reading` changes.
    reading_revision: u64,
    /// The picture tracks asked of this carrier's publication, by track.
    pictures_out: BTreeMap<String, PictureOut>,
    /// Advances whenever `pictures_out` gains or loses a track.
    pictures_revision: u64,
    /// The picture tracks to show of each publication, by namespace.
    shown: BTreeMap<String, BTreeSet<String>>,
    /// Pictures received, by namespace and track.
    pictures_in: BTreeMap<(String, String), PictureQueue>,
    counters: Counters,
}
impl State {
    fn clear_publication(&mut self) {
        self.muted = true;
        for sent in self.groups.drain(..) {
            let _ = sent.group.abort(moq_net::Error::Cancel);
        }
        // Never reuse a partially transmitted group after mute or expiry.
        if !self.sequence.is_multiple_of(voice::FRAMES_PER_GROUP) {
            self.sequence = self.sequence.saturating_add(
                voice::FRAMES_PER_GROUP.saturating_sub(self.sequence % voice::FRAMES_PER_GROUP),
            );
        }
    }
    fn end(&mut self, status: Status) {
        if self.status == Status::Closed {
            return;
        }
        self.status = status;
        self.clear_publication();
        self.received.clear();
        if !self.reading.is_empty() {
            self.reading.clear();
            self.reading_revision = self.reading_revision.saturating_add(1);
        }
        if !self.pictures_out.is_empty() {
            self.pictures_out.clear();
            self.pictures_revision = self.pictures_revision.saturating_add(1);
        }
        self.pictures_in.clear();
        if let Some(connected) = self.session.take() {
            connected.session.abort(moq_net::Error::Cancel);
        }
        if let Some(track) = self.track.take() {
            let _ = track.abort(moq_net::Error::Cancel);
        }
        if let Some(broadcast) = self.broadcast.take() {
            let _ = broadcast.abort(moq_net::Error::Cancel);
        }
    }
    fn expire(&mut self, now: Instant) {
        while self.groups.front().is_some_and(|sent| sent.expires <= now) {
            let Some(sent) = self.groups.pop_front() else {
                break;
            };
            let _ = sent.group.abort(moq_net::Error::Cancel);
            if sent.count < voice::FRAMES_PER_GROUP {
                self.sequence = self.sequence.saturating_add(
                    voice::FRAMES_PER_GROUP.saturating_sub(self.sequence % voice::FRAMES_PER_GROUP),
                );
            }
        }
        for queue in self.received.values_mut() {
            while queue.front().is_some_and(|frame| frame.expires_at <= now) {
                queue.pop_front();
                self.counters.late_dropped = self.counters.late_dropped.saturating_add(1);
            }
        }
    }
}
struct Shared {
    scope: ConnectionScope,
    authority: Arc<dyn ClientAuthority>,
    state: Mutex<State>,
    outgoing: moq_net::origin::Producer,
    incoming: moq_net::origin::Producer,
    _origins: [carrier::OriginTask; 2],
}
impl Shared {
    fn current(&self) -> bool {
        self.scope
            .publication
            .iter()
            .map(|name| (Direction::Publish, name))
            .chain(
                self.scope
                    .subscription_prefix
                    .iter()
                    .map(|prefix| (Direction::Subscribe, prefix)),
            )
            .all(|(direction, name)| self.authority.current(&self.scope, direction, name))
    }
    /// Whether `name`, offered by the relay, is a publication this carrier
    /// reads now: another participant directly beneath the call that the
    /// owner admits.
    fn admits(&self, name: &str) -> bool {
        let Some(prefix) = &self.scope.subscription_prefix else {
            return false;
        };
        valid_namespace(name)
            && name.get(..64) == Some(prefix.as_str())
            && self.scope.publication.as_deref() != Some(name)
            && self
                .authority
                .current(&self.scope, Direction::Subscribe, name)
    }
    fn check(&self) -> Result<(), ClientError> {
        if !self.current() {
            self.state.lock_recover().end(Status::Denied);
            return Err(ClientError::Denied);
        }
        Ok(())
    }
}
/// One owned carrier attempt. No automatic reconnect or proof reuse; a new
/// native owner operation must authorize each replacement. Drop closes it.
pub struct Client {
    shared: Arc<Shared>,
    task: Mutex<Option<tokio::task::JoinHandle<()>>>,
}
impl Client {
    /// Validate `connection` and start the native client on the current runtime.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError::Invalid`] for a malformed connection scope and
    /// [`ClientError::Unavailable`] outside a Tokio runtime or when the native
    /// client cannot be configured.
    pub fn start(
        connection: Connection,
        authority: Arc<dyn ClientAuthority>,
    ) -> Result<Self, ClientError> {
        let url = validate(&connection)?;
        let runtime =
            tokio::runtime::Handle::try_current().map_err(|_| ClientError::Unavailable)?;
        let (outgoing, outgoing_task) =
            carrier::OriginTask::produce(carrier::origin_config(ORIGIN_BYTES, SEND_AGE));
        let (incoming, incoming_task) =
            carrier::OriginTask::produce(carrier::origin_config(ORIGIN_BYTES, RECEIVE_AGE));
        let shared = Arc::new(Shared {
            scope: connection.scope,
            authority,
            outgoing,
            incoming,
            _origins: [outgoing_task, incoming_task],
            state: Mutex::new(State {
                status: Status::Connecting,
                muted: true,
                sequence: 0,
                session: None,
                broadcast: None,
                track: None,
                groups: VecDeque::new(),
                received: BTreeMap::new(),
                reading: BTreeMap::new(),
                reading_revision: 0,
                pictures_out: BTreeMap::new(),
                pictures_revision: 0,
                shown: BTreeMap::new(),
                pictures_in: BTreeMap::new(),
                counters: Counters::default(),
            }),
        });
        shared.check()?;
        let trust = if shared.scope.fingerprints.is_empty() {
            carrier::Trust::System
        } else {
            carrier::Trust::Fingerprints(shared.scope.fingerprints.clone())
        };
        let mut picture_requests = None;
        let publish = if let Some(name) = &shared.scope.publication {
            let broadcast = shared
                .outgoing
                .create_broadcast(name.as_str())
                .map_err(|_| ClientError::Unavailable)?;
            // The relay asks for a picture layer while someone shows it.
            picture_requests = Some(broadcast.dynamic());
            let track = broadcast
                .create_track(
                    VOICE_TRACK,
                    moq_net::track::Info::default().with_max_age(SEND_AGE),
                )
                .map_err(|_| ClientError::Unavailable)?;
            broadcast
                .announce(moq_net::origin::Route::default())
                .map_err(|_| ClientError::Unavailable)?;
            let mut state = shared.state.lock_recover();
            state.track = Some(track);
            state.broadcast = Some(broadcast);
            drop(state);
            let names = crate::relay::exact(&[name.as_str()]).ok_or(ClientError::Invalid)?;
            Some(
                shared
                    .outgoing
                    .consume()
                    .scope("", &names)
                    .map_err(|_| ClientError::Invalid)?,
            )
        } else {
            None
        };
        let subscribe = match &shared.scope.subscription_prefix {
            None => None,
            Some(prefix) => {
                let subtree =
                    moq_net::Pattern::subtree(prefix.as_str()).map_err(|_| ClientError::Invalid)?;
                Some(
                    shared
                        .incoming
                        .scope("", &moq_net::Patterns::from(subtree))
                        .map_err(|_| ClientError::Invalid)?,
                )
            }
        };
        let run_shared = shared.clone();
        let grant = connection.authorization;
        let origins = Origins {
            publish,
            subscribe,
            picture_requests,
        };
        let task = runtime.spawn(async move {
            run(run_shared, url, trust, grant, origins).await;
        });
        Ok(Self {
            shared,
            task: Mutex::new(Some(task)),
        })
    }
    pub fn status(&self) -> Status {
        let _ = self.shared.check();
        self.shared.state.lock_recover().status
    }
    /// Called only after the shell's independent local capture consent/lease.
    /// This operation cannot widen the owner's granted publication direction.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError::Denied`] without a publication grant and
    /// [`ClientError::Unavailable`] unless the session is live.
    pub fn enable_publication(&self) -> Result<(), ClientError> {
        self.shared.check()?;
        if self.shared.scope.publication.is_none() {
            return Err(ClientError::Denied);
        }
        let mut state = self.shared.state.lock_recover();
        if state.status != Status::Live {
            return Err(ClientError::Unavailable);
        }
        state.muted = false;
        drop(state);
        Ok(())
    }
    /// No outage backlog: non-live or muted frames are rejected synchronously.
    ///
    /// # Errors
    ///
    /// Returns an error when the session is not live, publication is muted, the
    /// frame is stale, or the group cannot accept it.
    pub fn try_send(&self, frame: CapturedOpus) -> Result<(), ClientError> {
        self.shared.check()?;
        let now = Instant::now();
        let mut state = self.shared.state.lock_recover();
        if state.status != Status::Live {
            return Err(ClientError::Unavailable);
        }
        if state.muted {
            return Err(ClientError::Muted);
        }
        if frame.captured_at > now || now.duration_since(frame.captured_at) >= SEND_AGE {
            state.counters.late_dropped = state.counters.late_dropped.saturating_add(1);
            return Err(ClientError::Expired);
        }
        if frame.duration_samples != voice::FRAME_SAMPLES
            || frame.payload.is_empty()
            || frame.payload.len() > voice::MAX_OBJECT_BYTES - voice::HEADER_BYTES
        {
            return Err(ClientError::Invalid);
        }
        state.expire(now);
        let sequence = state.sequence;
        let next = sequence.checked_add(1).ok_or(ClientError::Capacity)?;
        let timestamp = moq_net::Timestamp::from_scale(frame.timestamp_48k, 48_000)
            .map_err(|_| ClientError::Invalid)?;
        let bytes = VoiceObject {
            sequence,
            timestamp: frame.timestamp_48k,
            duration: frame.duration_samples,
            payload: frame.payload,
        }
        .encode()
        .map_err(|_| ClientError::Invalid)?;
        if state
            .groups
            .back()
            .is_none_or(|sent| sent.count == voice::FRAMES_PER_GROUP)
        {
            let group = state
                .track
                .as_mut()
                .ok_or(ClientError::Unavailable)?
                .create_group(moq_net::group::Info::from(
                    sequence / voice::FRAMES_PER_GROUP,
                ))
                .map_err(|_| ClientError::Capacity)?;
            state.groups.push_back(SentGroup {
                expires: crate::later(frame.captured_at, SEND_AGE),
                group,
                count: 0,
            });
        }
        let sent = state.groups.back_mut().ok_or(ClientError::Unavailable)?;
        sent.expires = sent.expires.max(crate::later(frame.captured_at, SEND_AGE));
        sent.group
            .write_frame(timestamp, bytes)
            .map_err(|_| ClientError::Capacity)?;
        sent.count = sent.count.saturating_add(1);
        if sent.count == voice::FRAMES_PER_GROUP {
            sent.group.finish().map_err(|_| ClientError::Unavailable)?;
        }
        state.sequence = next;
        state.counters.sent = state.counters.sent.saturating_add(1);
        drop(state);
        Ok(())
    }
    pub fn try_recv(&self) -> Option<ReceivedOpus> {
        self.shared.check().ok()?;
        let mut state = self.shared.state.lock_recover();
        state.expire(Instant::now());
        if state.status != Status::Live {
            return None;
        }
        let name = state
            .received
            .iter()
            .filter_map(|(name, queue)| queue.front().map(|frame| (name, frame.received_at)))
            .min_by_key(|(_, at)| *at)
            .map(|(name, _)| name.clone())?;
        state.received.get_mut(&name)?.pop_front()
    }
    /// The picture layers someone asks of this participant now: what to
    /// encode. Changes with [`Self::pictures_revision`].
    pub fn wanted_pictures(&self) -> Vec<(Source, Layer)> {
        let state = self.shared.state.lock_recover();
        state
            .pictures_out
            .keys()
            .filter_map(|name| video::parse_track(name))
            .collect()
    }
    /// Advances whenever a picture layer is asked for or no longer is.
    pub fn pictures_revision(&self) -> u64 {
        self.shared.state.lock_recover().pictures_revision
    }
    /// Send one picture of `source` on its layer's track. A layer starts
    /// with a keyframe, and each keyframe starts a group.
    ///
    /// # Errors
    ///
    /// [`ClientError::Muted`] when nobody asks for this layer,
    /// [`ClientError::Expired`] for a picture before the layer's first
    /// keyframe, [`ClientError::Capacity`] for a group past
    /// [`video::MAX_GROUP_OBJECTS`] (send a keyframe), and otherwise as
    /// [`Self::try_send`].
    pub fn send_picture(&self, source: Source, object: &VideoObject) -> Result<(), ClientError> {
        self.shared.check()?;
        let bytes = object.encode().map_err(|_| ClientError::Invalid)?;
        let timestamp = moq_net::Timestamp::from_scale(object.timestamp_us, 1_000_000)
            .map_err(|_| ClientError::Invalid)?;
        let mut state = self.shared.state.lock_recover();
        if state.status != Status::Live {
            return Err(ClientError::Unavailable);
        }
        let sent = state
            .pictures_out
            .get_mut(&video::track(source, object.layer))
            .ok_or(ClientError::Muted)
            .and_then(|out| out.write(object.keyframe, timestamp, bytes));
        drop(state);
        sent
    }
    /// Show `tracks` of the publication `namespace` (none: stop showing it):
    /// the carrier asks the relay for exactly these while it reads that
    /// publication.
    pub fn show(&self, namespace: &str, tracks: &[(Source, Layer)]) {
        let mut state = self.shared.state.lock_recover();
        if tracks.is_empty() {
            state.shown.remove(namespace);
        } else {
            state.shown.insert(
                namespace.to_owned(),
                tracks
                    .iter()
                    .map(|(source, layer)| video::track(*source, *layer))
                    .collect(),
            );
        }
    }
    /// The oldest picture received of the shown tracks.
    pub fn try_recv_picture(&self) -> Option<ReceivedPicture> {
        self.shared.check().ok()?;
        let mut state = self.shared.state.lock_recover();
        if state.status != Status::Live {
            return None;
        }
        let key = state
            .pictures_in
            .iter()
            .filter_map(|(key, queue)| {
                queue
                    .pictures
                    .front()
                    .map(|picture| (key, picture.received_at))
            })
            .min_by_key(|(_, at)| *at)
            .map(|(key, _)| key.clone())?;
        state.pictures_in.get_mut(&key)?.pictures.pop_front()
    }
    /// The publications this carrier reads now. Changes with
    /// [`Self::publications_revision`].
    pub fn publications(&self) -> Vec<PublicationId> {
        let state = self.shared.state.lock_recover();
        state
            .reading
            .iter()
            .map(|(namespace, incarnation)| PublicationId {
                namespace: namespace.clone(),
                carrier_generation: self.shared.scope.carrier_generation,
                incarnation: *incarnation,
            })
            .collect()
    }
    /// Advances whenever a publication starts or stops being read.
    pub fn publications_revision(&self) -> u64 {
        self.shared.state.lock_recover().reading_revision
    }
    pub fn counters(&self) -> Counters {
        let mut state = self.shared.state.lock_recover();
        state.expire(Instant::now());
        Counters {
            queued_received: state.received.values().map(VecDeque::len).sum(),
            ..state.counters
        }
    }
    pub fn stop_publication(&self) {
        self.shared.state.lock_recover().clear_publication();
    }
    pub fn close(&self) {
        self.shared.state.lock_recover().end(Status::Closed);
        let task = self.task.lock_recover().take();
        if let Some(task) = task {
            task.abort();
        }
    }
}
impl Drop for Client {
    fn drop(&mut self) {
        self.close();
    }
}

fn hex_part(part: &str) -> bool {
    part.len() == 64
        && part
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        && part.bytes().any(|b| b != b'0')
}
/// A call: 64 lowercase hex digits, not all zero.
fn valid_call(prefix: &str) -> bool {
    hex_part(prefix)
}
fn valid_namespace(name: &str) -> bool {
    // `owner/track`: 64 lowercase hex digits on each side of the one separator.
    name.len() == 129
        && name
            .split_once('/')
            .is_some_and(|(owner, track)| hex_part(owner) && hex_part(track))
}
fn validate(connection: &Connection) -> Result<url::Url, ClientError> {
    let scope = &connection.scope;
    if connection.authorization.is_empty()
        || connection.authorization.len() > crate::relay::MAX_AUTHORIZATION_BYTES
        || scope.owner_epoch == [0; 32]
        || scope.carrier_generation == 0
        || scope.endpoint.len() > 2048
        || scope.fingerprints.len() > 2
        || (scope.publication.is_none() && scope.subscription_prefix.is_none())
    {
        return Err(ClientError::Invalid);
    }
    let url = url::Url::parse(&scope.endpoint).map_err(|_| ClientError::Invalid)?;
    if url.scheme() != "https"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.host_str().is_none()
        || url.path() != crate::relay::VOICE_PATH
        || url.query().is_some()
        || url.fragment().is_some()
        || url.as_str() != scope.endpoint
        || scope.endpoint
            != format!(
                "{}{path}",
                url.origin().ascii_serialization(),
                path = crate::relay::VOICE_PATH
            )
    {
        return Err(ClientError::Invalid);
    }
    if scope.fingerprints.iter().any(|value| {
        value.len() != 64
            || !value
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    }) {
        return Err(ClientError::Invalid);
    }
    if scope
        .publication
        .as_deref()
        .is_some_and(|name| !valid_namespace(name))
        || scope
            .subscription_prefix
            .as_deref()
            .is_some_and(|prefix| !valid_call(prefix))
    {
        return Err(ClientError::Invalid);
    }
    // A publication lies directly beneath the call this carrier hears.
    if let (Some(name), Some(prefix)) = (&scope.publication, &scope.subscription_prefix)
        && name.get(..64) != Some(prefix.as_str())
    {
        return Err(ClientError::Invalid);
    }
    Ok(url)
}
/// What a carrier serves and reads.
struct Origins {
    publish: Option<moq_net::origin::Consumer>,
    subscribe: Option<moq_net::origin::Producer>,
    /// The relay's requests for picture tracks of this publication.
    picture_requests: Option<moq_net::broadcast::Dynamic>,
}

async fn run(
    shared: Arc<Shared>,
    url: url::Url,
    trust: carrier::Trust,
    grant: Vec<u8>,
    origins: Origins,
) {
    let Origins {
        publish,
        subscribe,
        mut picture_requests,
    } = origins;
    // The relay answers the admission preface only after host admission;
    // until then the transport is merely open, not live.
    let connected =
        carrier::connect(&url, &trust, &grant, publish, subscribe, CONNECT_TIMEOUT).await;
    drop(grant);
    let Ok(connected) = connected else {
        shared.state.lock_recover().end(Status::Failed);
        return;
    };
    let session = connected.session.clone();
    if shared.check().is_err() {
        session.abort(moq_net::Error::Cancel);
        return;
    }
    {
        let mut state = shared.state.lock_recover();
        if state.status != Status::Connecting {
            session.abort(moq_net::Error::Cancel);
            return;
        }
        state.session = Some(connected);
        state.status = Status::Live;
    }
    let mut readers = Readers::default();
    let mut announced = shared.incoming.consume().announced();
    let mut revision = shared.authority.revision();
    let mut tick = tokio::time::interval(TICK);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            _ = session.closed() => { shared.state.lock_recover().end(Status::Failed); break; }
            update = announced.next(), if shared.scope.subscription_prefix.is_some() => {
                let Some(update) = update else {
                    shared.state.lock_recover().end(Status::Failed);
                    break;
                };
                let name = update.prefix.as_str().to_owned();
                if update.kind.is_active() {
                    readers.offered.insert(name.clone());
                    readers.retry.remove(&name);
                    if !readers.reading(&name) && shared.admits(&name) {
                        readers.start(&shared, name);
                    }
                } else {
                    readers.offered.remove(&name);
                    readers.stop(&shared, &name);
                }
            }
            request = async {
                match picture_requests.as_mut() {
                    Some(requests) => requests.requested_track().await,
                    None => std::future::pending().await,
                }
            } => match request {
                Ok(request) => accept_picture(&shared, request),
                Err(_) => picture_requests = None,
            },
            _ = tick.tick() => {
                if shared.check().is_err() { break; }
                {
                    let mut state = shared.state.lock_recover();
                    if state.status != Status::Live { break; }
                    state.expire(Instant::now());
                    // A layer nobody shows any more is no longer encoded.
                    let asked = state.pictures_out.len();
                    state.pictures_out.retain(|_, out| out.track.is_used());
                    if state.pictures_out.len() != asked {
                        state.pictures_revision = state.pictures_revision.saturating_add(1);
                    }
                }
                // A new grant may admit someone the relay already offers, or
                // no longer admit someone being read.
                let now_revision = shared.authority.revision();
                let regranted = now_revision != revision;
                revision = now_revision;
                readers.reconcile(&shared, regranted, Instant::now());
                readers.reconcile_pictures(&shared, Instant::now());
            }
            ended = readers.picture_tasks.join_next(), if !readers.picture_tasks.is_empty() => {
                if let Some(Ok(key)) = ended {
                    readers.picture_ended(&shared, key);
                }
            }
            ended = readers.tasks.join_next(), if !readers.tasks.is_empty() => {
                // A publication ended (its publisher left, or its stream
                // broke). Everyone else plays on; it is read again, as a new
                // incarnation, while the relay still offers it.
                if let Some(Ok((name, incarnation))) = ended {
                    readers.ended(&shared, &name, incarnation);
                }
            }
        }
    }
    readers.tasks.abort_all();
    readers.picture_tasks.abort_all();
    session.abort(moq_net::Error::Cancel);
}

/// Serve the relay's request for one of this publication's picture tracks,
/// while the owner lets it publish.
fn accept_picture(shared: &Shared, request: moq_net::track::Request) {
    let publishes = shared.scope.publication.as_deref().is_some_and(|name| {
        shared
            .authority
            .current(&shared.scope, Direction::Publish, name)
    });
    if video::parse_track(request.name()).is_none() || !publishes {
        request.reject(moq_net::Error::NotFound);
        return;
    }
    let name = request.name().to_owned();
    let track = request.accept(moq_net::track::Info::default().with_max_age(PICTURE_SEND_AGE));
    let mut state = shared.state.lock_recover();
    state.pictures_out.insert(
        name,
        PictureOut {
            track,
            group: None,
            next_group: 0,
            in_group: 0,
        },
    );
    state.pictures_revision = state.pictures_revision.saturating_add(1);
}

/// The publications one carrier reads, as the relay offers them.
#[derive(Default)]
struct Readers {
    /// What the relay offers beneath the call now.
    offered: BTreeSet<String>,
    /// What is read, by namespace: its incarnation and its task.
    running: BTreeMap<String, (u64, tokio::task::AbortHandle)>,
    /// The last incarnation of each namespace this carrier read.
    incarnations: BTreeMap<String, u64>,
    /// When an ended publication may be read again.
    retry: BTreeMap<String, Instant>,
    tasks: tokio::task::JoinSet<(String, u64)>,
    /// The shown picture tracks read, by namespace and track: the
    /// publication's incarnation they belong to, and their task.
    pictures: BTreeMap<(String, String), (u64, tokio::task::AbortHandle)>,
    /// When an ended picture track may be read again.
    picture_retry: BTreeMap<(String, String), Instant>,
    picture_tasks: tokio::task::JoinSet<(String, String)>,
}
impl Readers {
    fn reading(&self, name: &str) -> bool {
        self.running.contains_key(name)
    }
    fn start(&mut self, shared: &Arc<Shared>, name: String) {
        let incarnation = self
            .incarnations
            .get(&name)
            .map_or(1, |last| last.saturating_add(1));
        self.incarnations.insert(name.clone(), incarnation);
        {
            let mut state = shared.state.lock_recover();
            // Nothing of an earlier incarnation follows the new one.
            state.received.remove(&name);
            state.reading.insert(name.clone(), incarnation);
            state.reading_revision = state.reading_revision.saturating_add(1);
        }
        let (owner, reading) = (shared.clone(), name.clone());
        let task = self.tasks.spawn(async move {
            receive(owner, reading.clone(), incarnation).await;
            (reading, incarnation)
        });
        self.running.insert(name, (incarnation, task));
    }
    fn stop(&mut self, shared: &Shared, name: &str) {
        self.retry.remove(name);
        if let Some((_, task)) = self.running.remove(name) {
            task.abort();
            let mut state = shared.state.lock_recover();
            state.received.remove(name);
            state.reading.remove(name);
            state.reading_revision = state.reading_revision.saturating_add(1);
        }
    }
    fn ended(&mut self, shared: &Shared, name: &str, incarnation: u64) {
        if self
            .running
            .get(name)
            .is_some_and(|(current, _)| *current == incarnation)
        {
            self.running.remove(name);
            let mut state = shared.state.lock_recover();
            state.reading.remove(name);
            state.reading_revision = state.reading_revision.saturating_add(1);
            drop(state);
            self.retry
                .insert(name.to_owned(), crate::later(Instant::now(), REREAD));
        }
    }
    /// Read the picture tracks shown of each publication being read; stop
    /// those no longer shown or whose publication is no longer read.
    fn reconcile_pictures(&mut self, shared: &Arc<Shared>, now: Instant) {
        let wanted: Vec<((String, String), u64)> = {
            let state = shared.state.lock_recover();
            state
                .shown
                .iter()
                .filter_map(|(name, tracks)| {
                    state
                        .reading
                        .get(name)
                        .map(|incarnation| (name, tracks, *incarnation))
                })
                .flat_map(|(name, tracks, incarnation)| {
                    tracks
                        .iter()
                        .map(move |track| ((name.clone(), track.clone()), incarnation))
                })
                .collect()
        };
        let stale: Vec<(String, String)> = self
            .pictures
            .iter()
            .filter(|(key, (incarnation, _))| {
                !wanted
                    .iter()
                    .any(|(wanted, current)| wanted == *key && current == incarnation)
            })
            .map(|(key, _)| key.clone())
            .collect();
        for key in stale {
            if let Some((_, task)) = self.pictures.remove(&key) {
                task.abort();
            }
            shared.state.lock_recover().pictures_in.remove(&key);
        }
        for (key, incarnation) in wanted {
            if self.pictures.contains_key(&key)
                || self.picture_retry.get(&key).is_some_and(|at| now < *at)
            {
                continue;
            }
            self.picture_retry.remove(&key);
            let Some((source, _)) = video::parse_track(&key.1) else {
                continue;
            };
            shared
                .state
                .lock_recover()
                .pictures_in
                .insert(key.clone(), PictureQueue::default());
            let (owner, reading) = (shared.clone(), key.clone());
            let task = self.picture_tasks.spawn(async move {
                receive_pictures(owner, &reading, incarnation, source).await;
                reading
            });
            self.pictures.insert(key, (incarnation, task));
        }
    }
    fn picture_ended(&mut self, shared: &Shared, key: (String, String)) {
        if self.pictures.remove(&key).is_some() {
            shared.state.lock_recover().pictures_in.remove(&key);
            self.picture_retry
                .insert(key, crate::later(Instant::now(), REREAD));
        }
    }
    /// Start what is offered and admitted but not read (after a new grant,
    /// or once an ended one may be read again); stop what a new grant no
    /// longer admits.
    fn reconcile(&mut self, shared: &Arc<Shared>, regranted: bool, now: Instant) {
        let due: Vec<String> = self
            .offered
            .iter()
            .filter(|name| !self.reading(name))
            .filter(|name| self.retry.get(*name).map_or(regranted, |at| now >= *at))
            .cloned()
            .collect();
        for name in due {
            self.retry.remove(&name);
            if shared.admits(&name) {
                self.start(shared, name);
            }
        }
        if regranted {
            let lapsed: Vec<String> = self
                .running
                .keys()
                .filter(|name| !shared.admits(name))
                .cloned()
                .collect();
            for name in lapsed {
                self.stop(shared, &name);
            }
        }
    }
}
/// Read one shown picture track of one publication into its queue.
async fn receive_pictures(
    shared: Arc<Shared>,
    key: &(String, String),
    incarnation: u64,
    source: Source,
) {
    let (name, track) = key;
    let Ok(broadcast) = shared
        .incoming
        .consume()
        .routed_broadcast(name.as_str())
        .await
    else {
        return;
    };
    let Ok(track) = broadcast.track(track) else {
        return;
    };
    let Ok(mut track) = track
        .subscribe(moq_net::track::Subscription::default().with_max_age(PICTURE_RECEIVE_AGE))
        .await
    else {
        return;
    };
    while let Ok(Some(mut group)) = track.recv_group().await {
        while let Ok(Some(frame)) = group.read_frame().await {
            if shared.check().is_err() {
                return;
            }
            let Ok(object) = VideoObject::decode(&frame.payload) else {
                return;
            };
            if !deliver_picture(&shared, key, incarnation, source, object) {
                return;
            }
        }
    }
}

/// Queue one received picture. Returns false when its reader must stop.
fn deliver_picture(
    shared: &Shared,
    key: &(String, String),
    incarnation: u64,
    source: Source,
    object: VideoObject,
) -> bool {
    let picture = ReceivedPicture {
        publisher: PublicationId {
            namespace: key.0.clone(),
            carrier_generation: shared.scope.carrier_generation,
            incarnation,
        },
        source,
        object,
        received_at: Instant::now(),
    };
    let mut state = shared.state.lock_recover();
    if state.status != Status::Live || state.reading.get(&key.0) != Some(&incarnation) {
        return false;
    }
    let queued = state
        .pictures_in
        .get_mut(key)
        .map(|queue| queue.admit(picture))
        .is_some();
    drop(state);
    queued
}

async fn receive(shared: Arc<Shared>, name: String, incarnation: u64) {
    let Ok(broadcast) = shared
        .incoming
        .consume()
        .routed_broadcast(name.as_str())
        .await
    else {
        return;
    };
    let Ok(track) = broadcast.track(VOICE_TRACK) else {
        return;
    };
    // Browser publishers send each object as a datagram bundle (newest first,
    // then redundant earlier objects); a separate local subscription drains
    // that best-effort channel alongside the groups.
    let datagrams = match broadcast.track(VOICE_TRACK) {
        Ok(track) => track
            .subscribe(moq_net::track::Subscription::default().with_max_age(RECEIVE_AGE))
            .await
            .ok(),
        Err(_) => None,
    };
    let Ok(track) = track
        .subscribe(moq_net::track::Subscription::default().with_max_age(RECEIVE_AGE))
        .await
    else {
        return;
    };
    let last = Mutex::new(None);
    let reading = Reading {
        name: &name,
        incarnation,
    };
    let groups = receive_groups(&shared, reading, track, &last);
    match datagrams {
        Some(datagrams) => {
            tokio::select! {
                () = groups => {}
                () = receive_datagrams(&shared, reading, datagrams, &last) => {}
            }
        }
        None => groups.await,
    }
}

/// One publication being read: its namespace and incarnation.
#[derive(Clone, Copy)]
struct Reading<'a> {
    name: &'a str,
    incarnation: u64,
}

/// The newest object delivered from one publication.
#[derive(Clone, Copy)]
struct Delivered {
    sequence: u64,
    timestamp: u64,
}
impl Delivered {
    /// Frames published between this object and `next` that never arrived:
    /// the publisher, relay or receiver dropped them past a latency budget.
    /// Only frames missing from both the sequence and the media time count:
    /// a publisher in DTX (silence) skips media time without skipping
    /// sequence numbers, and a `grouped` publisher that abandons a partial
    /// group, as it does at the end of every talk spurt, resumes at the next
    /// group without ever sending the rest of the one it left. It abandons
    /// a group only after sending nothing for its frame budget, so the media
    /// time then runs past the sequence numbers it skipped; a group tail lost
    /// on the way (the receiver's read budget ran out, or the publisher
    /// expired it undrained) leaves the two equal and is counted.
    fn skipped(self, next: &VoiceObject, grouped: bool) -> u64 {
        let first_missing = self.sequence.saturating_add(1);
        let frames = next
            .timestamp
            .saturating_sub(self.timestamp)
            .checked_div(u64::from(voice::FRAME_SAMPLES))
            .unwrap_or(0)
            .saturating_sub(1);
        let mut sequences = next.sequence.saturating_sub(first_missing);
        if grouped && next.sequence.is_multiple_of(voice::FRAMES_PER_GROUP) && frames > sequences {
            let next_group = (self.sequence / voice::FRAMES_PER_GROUP)
                .saturating_add(1)
                .saturating_mul(voice::FRAMES_PER_GROUP);
            sequences = next.sequence.saturating_sub(first_missing.max(next_group));
        }
        sequences.min(frames)
    }
}

/// Queue one received object. Returns false when the carrier must stop.
fn deliver(
    shared: &Shared,
    reading: Reading<'_>,
    group: u64,
    grouped: bool,
    object: VoiceObject,
    last: &Mutex<Option<Delivered>>,
) -> bool {
    let mut state = shared.state.lock_recover();
    if state.status != Status::Live {
        return false;
    }
    // An incarnation no longer read delivers nothing more.
    if state.reading.get(reading.name) != Some(&reading.incarnation) {
        return false;
    }
    let name = reading.name;
    if !account(&mut state, grouped, &object, last) {
        return true;
    }
    state.expire(Instant::now());
    if state
        .received
        .get(name)
        .is_some_and(|queue| queue.len() >= RECEIVE_FRAMES)
    {
        if let Some(queue) = state.received.get_mut(name) {
            queue.pop_front();
        }
        state.counters.capacity_dropped = state.counters.capacity_dropped.saturating_add(1);
    }
    // Per-frame arrival: each frame's 200 ms receive budget (and its latency
    // accounting) starts when that frame arrived.
    let arrived = Instant::now();
    state
        .received
        .entry(name.to_owned())
        .or_default()
        .push_back(ReceivedOpus {
            publisher: PublicationId {
                namespace: name.to_owned(),
                carrier_generation: shared.scope.carrier_generation,
                incarnation: reading.incarnation,
            },
            group,
            object,
            received_at: arrived,
            expires_at: crate::later(arrived, RECEIVE_AGE),
        });
    state.counters.received = state.counters.received.saturating_add(1);
    true
}

/// Count what went missing between the last object of this publication and
/// `object`, and make `object` the last. Returns false for a duplicate.
fn account(
    state: &mut State,
    grouped: bool,
    object: &VoiceObject,
    last: &Mutex<Option<Delivered>>,
) -> bool {
    let mut last = last.lock_recover();
    if let Some(prior) = *last {
        if object.sequence <= prior.sequence {
            state.counters.duplicate_dropped = state.counters.duplicate_dropped.saturating_add(1);
            return false;
        }
        state.counters.late_dropped = state
            .counters
            .late_dropped
            .saturating_add(prior.skipped(object, grouped));
    }
    *last = Some(Delivered {
        sequence: object.sequence,
        timestamp: object.timestamp,
    });
    true
}

/// Drop one grouped object that arrived past its group's read budget,
/// counting it and what it skipped. Returns false when the carrier must stop.
fn drop_late(shared: &Shared, object: &VoiceObject, last: &Mutex<Option<Delivered>>) -> bool {
    let mut state = shared.state.lock_recover();
    if state.status != Status::Live {
        return false;
    }
    if account(&mut state, true, object, last) {
        state.counters.late_dropped = state.counters.late_dropped.saturating_add(1);
    }
    true
}

async fn receive_datagrams(
    shared: &Shared,
    reading: Reading<'_>,
    mut track: moq_net::track::Subscriber,
    last: &Mutex<Option<Delivered>>,
) {
    while let Ok(Some(datagram)) = track.recv_datagram().await {
        if shared.check().is_err() {
            return;
        }
        let Ok(objects) = voice::bundle_objects(&datagram.payload) else {
            // The relay validated the bundle; a malformed one here is not ours to trust.
            return;
        };
        // Oldest first: a recovered object reaches the decoder before the newer
        // object that carried it.
        for bytes in objects.into_iter().rev() {
            let Ok(object) = VoiceObject::decode(bytes) else {
                return;
            };
            if !deliver(shared, reading, datagram.sequence, false, object, last) {
                return;
            }
        }
    }
}

async fn receive_group(
    track: &mut moq_net::track::Subscriber,
) -> Result<Option<(moq_net::group::Consumer, Instant)>, moq_net::Error> {
    Ok(track
        .recv_group()
        .await?
        .map(|group| (group, crate::later(Instant::now(), RECEIVE_AGE))))
}

async fn receive_groups(
    shared: &Shared,
    reading: Reading<'_>,
    mut track: moq_net::track::Subscriber,
    last: &Mutex<Option<Delivered>>,
) {
    let mut next = receive_group(&mut track).await;
    while let Ok(Some((mut group, expires_at))) = next {
        let mut pending = None;
        let mut count: u64 = 0;
        loop {
            // A newer QUIC stream can arrive before this group's tail. Retain
            // one successor until this group finishes or reaches its deadline;
            // its deadline starts on arrival, not when we start reading it.
            tokio::select! {
                biased;
                value = group.read_frame() => {
                    let Ok(Some(frame)) = value else {
                        next = match pending.take() {
                            Some(queued) => queued,
                            None => receive_group(&mut track).await,
                        };
                        break;
                    };
                    if shared.check().is_err() { return; }
                    count = count.saturating_add(1);
                    let Ok(object) = VoiceObject::decode(&frame.payload) else { return; };
                    if count > voice::FRAMES_PER_GROUP || object.sequence / voice::FRAMES_PER_GROUP != group.sequence { return; }
                    // The group deadline bounds the read.
                    if Instant::now() >= expires_at {
                        if !drop_late(shared, &object, last) { return; }
                        continue;
                    }
                    if !deliver(shared, reading, group.sequence, true, object, last) { return; }
                }
                fresh = receive_group(&mut track), if pending.is_none() => { pending = Some(fresh); }
                () = tokio::time::sleep_until(expires_at.into()) => {
                    next = match pending.take() {
                        Some(queued) => queued,
                        None => receive_group(&mut track).await,
                    };
                    break;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests;
