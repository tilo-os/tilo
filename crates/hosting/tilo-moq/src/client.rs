//! Bounded native voice transport. Connection values are inert; the privileged
//! native owner retains keys, grants, descriptor custody and current policy.
use crate::{
    carrier,
    relay::{Direction, VOICE_TRACK},
    voice::{self, VoiceObject},
};
use std::{
    collections::{BTreeMap, VecDeque},
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
    pub subscriptions: Vec<String>,
}
/// Inert one-use transport input, never a grant or local microphone consent.
pub struct Connection {
    pub scope: ConnectionScope,
    pub authorization: Vec<u8>,
}
/// Installed only by the privileged native device owner, never by app input.
/// Implementations bind every scope field to retained authenticated custody,
/// and revalidate current policy, identity, grants, presence and generations.
pub trait ClientAuthority: Send + Sync {
    fn current(&self, scope: &ConnectionScope, direction: Direction, namespace: &str) -> bool;
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
                    .subscriptions
                    .iter()
                    .map(|name| (Direction::Subscribe, name)),
            )
            .all(|(direction, name)| self.authority.current(&self.scope, direction, name))
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
                counters: Counters::default(),
            }),
        });
        shared.check()?;
        let trust = if shared.scope.fingerprints.is_empty() {
            carrier::Trust::System
        } else {
            carrier::Trust::Fingerprints(shared.scope.fingerprints.clone())
        };
        let publish = if let Some(name) = &shared.scope.publication {
            let broadcast = shared
                .outgoing
                .create_broadcast(name.as_str())
                .map_err(|_| ClientError::Unavailable)?;
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
        let subscribe = if shared.scope.subscriptions.is_empty() {
            None
        } else {
            let names: Vec<&str> = shared
                .scope
                .subscriptions
                .iter()
                .map(String::as_str)
                .collect();
            let names = crate::relay::exact(&names).ok_or(ClientError::Invalid)?;
            Some(
                shared
                    .incoming
                    .scope("", &names)
                    .map_err(|_| ClientError::Invalid)?,
            )
        };
        let run_shared = shared.clone();
        let grant = connection.authorization;
        let task = runtime.spawn(async move {
            run(run_shared, url, trust, grant, publish, subscribe).await;
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

fn valid_namespace(name: &str) -> bool {
    let hex = |part: &str| {
        part.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    };
    let nonzero = |part: &str| part.bytes().any(|b| b != b'0');
    // `owner/track`: 64 lowercase hex digits on each side of the one separator.
    name.len() == 129
        && name.split_once('/').is_some_and(|(owner, track)| {
            owner.len() == 64 && hex(owner) && hex(track) && nonzero(owner) && nonzero(track)
        })
}
fn validate(connection: &Connection) -> Result<url::Url, ClientError> {
    let scope = &connection.scope;
    if connection.authorization.is_empty()
        || connection.authorization.len() > crate::relay::MAX_AUTHORIZATION_BYTES
        || scope.owner_epoch == [0; 32]
        || scope.carrier_generation == 0
        || scope.endpoint.len() > 2048
        || scope.fingerprints.len() > 2
        || (scope.publication.is_none() && scope.subscriptions.is_empty())
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
    let mut names = scope.publication.iter().chain(scope.subscriptions.iter());
    let first = names.next().ok_or(ClientError::Invalid)?;
    if !valid_namespace(first)
        || names.any(|name| !valid_namespace(name) || name.get(..64) != first.get(..64))
        || scope.subscriptions.iter().enumerate().any(|(index, name)| {
            scope.publication.as_ref() == Some(name)
                || scope
                    .subscriptions
                    .get(..index)
                    .is_some_and(|prior| prior.contains(name))
        })
    {
        return Err(ClientError::Invalid);
    }
    Ok(url)
}
async fn run(
    shared: Arc<Shared>,
    url: url::Url,
    trust: carrier::Trust,
    grant: Vec<u8>,
    publish: Option<moq_net::origin::Consumer>,
    subscribe: Option<moq_net::origin::Producer>,
) {
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
    let mut readers = tokio::task::JoinSet::new();
    for name in &shared.scope.subscriptions {
        let (owner, name) = (shared.clone(), name.clone());
        readers.spawn(async move {
            receive(owner, name).await;
        });
    }
    let mut tick = tokio::time::interval(TICK);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            _ = session.closed() => { shared.state.lock_recover().end(Status::Failed); break; }
            _ = tick.tick() => {
                if shared.check().is_err() { break; }
                let mut state = shared.state.lock_recover();
                if state.status != Status::Live { break; }
                state.expire(Instant::now());
            }
            _ = readers.join_next(), if !readers.is_empty() => {
                // A closed remote publication can return under the same exact
                // namespace with a restarted sequence. Retire this carrier so
                // its owner obtains a fresh proof/generation and decoder state.
                shared.state.lock_recover().end(Status::Failed);
                break;
            }
        }
    }
    readers.abort_all();
    session.abort(moq_net::Error::Cancel);
}
async fn receive(shared: Arc<Shared>, name: String) {
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
    let groups = receive_groups(&shared, &name, track, &last);
    match datagrams {
        Some(datagrams) => {
            tokio::select! {
                () = groups => {}
                () = receive_datagrams(&shared, &name, datagrams, &last) => {}
            }
        }
        None => groups.await,
    }
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
    name: &str,
    group: u64,
    grouped: bool,
    object: VoiceObject,
    last: &Mutex<Option<Delivered>>,
) -> bool {
    let mut state = shared.state.lock_recover();
    if state.status != Status::Live {
        return false;
    }
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
    name: &str,
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
            if !deliver(shared, name, datagram.sequence, false, object, last) {
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
    name: &str,
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
                    if !deliver(shared, name, group.sequence, true, object, last) { return; }
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
