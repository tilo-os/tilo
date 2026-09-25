//! Bounded native voice transport. Connection values are inert; the privileged
//! native owner retains keys, grants, descriptor custody and current policy.
use crate::{
    relay::Direction,
    voice::{self, VoiceObject},
};
use std::{
    collections::{BTreeMap, VecDeque},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

const SEND_AGE: Duration = Duration::from_millis(100);
const RECEIVE_AGE: Duration = Duration::from_millis(200);
const TICK: Duration = Duration::from_millis(10);
/// Transport + WebTransport + admission (relay SETUP) budget. High-RTT links
/// (satellite, ~600 ms) need several round trips plus a loss recovery.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
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
    expires: Instant,
    group: moq_net::group::Producer,
    count: u64,
}
struct State {
    status: Status,
    muted: bool,
    sequence: u64,
    session: Option<moq_net::Session>,
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
        if self.sequence % voice::FRAMES_PER_GROUP != 0 {
            self.sequence = self
                .sequence
                .checked_add(voice::FRAMES_PER_GROUP - self.sequence % voice::FRAMES_PER_GROUP)
                .unwrap_or(u64::MAX);
        }
    }
    fn end(&mut self, status: Status) {
        if self.status == Status::Closed {
            return;
        }
        self.status = status;
        self.clear_publication();
        self.received.clear();
        if let Some(session) = self.session.take() {
            session.abort(moq_net::Error::Cancel);
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
            let sent = self.groups.pop_front().unwrap();
            let _ = sent.group.abort(moq_net::Error::Cancel);
            if sent.count < voice::FRAMES_PER_GROUP {
                self.sequence = self
                    .sequence
                    .checked_add(voice::FRAMES_PER_GROUP - self.sequence % voice::FRAMES_PER_GROUP)
                    .unwrap_or(u64::MAX);
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
            self.state.lock().unwrap().end(Status::Denied);
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
    pub fn start(
        connection: Connection,
        authority: Arc<dyn ClientAuthority>,
    ) -> Result<Self, ClientError> {
        let url = validate(&connection)?;
        let runtime =
            tokio::runtime::Handle::try_current().map_err(|_| ClientError::Unavailable)?;
        let outgoing = origin(SEND_AGE);
        let incoming = origin(RECEIVE_AGE);
        let shared = Arc::new(Shared {
            scope: connection.scope,
            authority,
            outgoing,
            incoming,
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
        let mut config = moq_native::ClientConfig::default();
        config.version = vec![
            "moq-transport-21"
                .parse()
                .map_err(|_| ClientError::Invalid)?,
        ];
        config.timeout = Some(CONNECT_TIMEOUT);
        config.tls.fingerprint = shared.scope.fingerprints.clone();
        let mut native = config
            .init()
            .map_err(|_| ClientError::Unavailable)?
            .with_authorization(connection.authorization)
            .map_err(|_| ClientError::Invalid)?;
        if let Some(name) = &shared.scope.publication {
            let mut broadcast = shared
                .outgoing
                .create_broadcast(
                    name.as_str(),
                    moq_net::broadcast::Route::new().with_announce(true),
                )
                .map_err(|_| ClientError::Unavailable)?;
            let track = broadcast
                .create_track(
                    "audio",
                    moq_net::track::Info::default().with_latency_max(SEND_AGE),
                )
                .map_err(|_| ClientError::Unavailable)?;
            let mut state = shared.state.lock().unwrap();
            state.track = Some(track);
            state.broadcast = Some(broadcast);
            let scoped = shared
                .outgoing
                .consume()
                .scope_exact(&[name.as_str().into()])
                .ok_or(ClientError::Invalid)?;
            native = native.with_publisher(scoped);
        }
        if !shared.scope.subscriptions.is_empty() {
            let paths: Vec<_> = shared
                .scope
                .subscriptions
                .iter()
                .map(|name| name.as_str().into())
                .collect();
            native = native.with_subscriber(
                shared
                    .incoming
                    .scope_exact(&paths)
                    .ok_or(ClientError::Invalid)?,
            );
        }
        let run_shared = shared.clone();
        let task = runtime.spawn(async move {
            run(run_shared, native, url).await;
        });
        Ok(Self {
            shared,
            task: Mutex::new(Some(task)),
        })
    }
    pub fn status(&self) -> Status {
        let _ = self.shared.check();
        self.shared.state.lock().unwrap().status
    }
    /// Called only after the shell's independent local capture consent/lease.
    /// This operation cannot widen the owner's granted publication direction.
    pub fn enable_publication(&self) -> Result<(), ClientError> {
        self.shared.check()?;
        if self.shared.scope.publication.is_none() {
            return Err(ClientError::Denied);
        }
        let mut state = self.shared.state.lock().unwrap();
        if state.status != Status::Live {
            return Err(ClientError::Unavailable);
        }
        state.muted = false;
        Ok(())
    }
    /// No outage backlog: non-live or muted frames are rejected synchronously.
    pub fn try_send(&self, frame: CapturedOpus) -> Result<(), ClientError> {
        self.shared.check()?;
        let now = Instant::now();
        let mut state = self.shared.state.lock().unwrap();
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
                .create_group(moq_net::group::Info {
                    sequence: sequence / voice::FRAMES_PER_GROUP,
                })
                .map_err(|_| ClientError::Capacity)?;
            state.groups.push_back(SentGroup {
                expires: frame.captured_at + SEND_AGE,
                group,
                count: 0,
            });
        }
        let sent = state.groups.back_mut().unwrap();
        sent.expires = sent.expires.min(frame.captured_at + SEND_AGE);
        sent.group
            .write_frame(timestamp, bytes)
            .map_err(|_| ClientError::Capacity)?;
        sent.count += 1;
        if sent.count == voice::FRAMES_PER_GROUP {
            sent.group.finish().map_err(|_| ClientError::Unavailable)?;
        }
        state.sequence = next;
        state.counters.sent = state.counters.sent.saturating_add(1);
        Ok(())
    }
    pub fn try_recv(&self) -> Option<ReceivedOpus> {
        self.shared.check().ok()?;
        let mut state = self.shared.state.lock().unwrap();
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
        let mut state = self.shared.state.lock().unwrap();
        state.expire(Instant::now());
        Counters {
            queued_received: state.received.values().map(VecDeque::len).sum(),
            ..state.counters
        }
    }
    pub fn stop_publication(&self) {
        self.shared.state.lock().unwrap().clear_publication();
    }
    pub fn close(&self) {
        self.shared.state.lock().unwrap().end(Status::Closed);
        if let Some(task) = self.task.lock().unwrap().take() {
            task.abort();
        }
    }
}
impl Drop for Client {
    fn drop(&mut self) {
        self.close();
    }
}

fn origin(age: Duration) -> moq_net::origin::Producer {
    moq_net::origin::Info::new(moq_net::Origin::random())
        .with_limits(
            moq_net::origin::Limits::voice()
                .with_frame_validator(|bytes| voice::validate_bundle(bytes).is_ok()),
        )
        .with_cache_duration(age)
        .with_latency_default(age)
        .produce()
}
fn valid_namespace(name: &str) -> bool {
    name.len() == 129
        && name.as_bytes()[64] == b'/'
        && name
            .bytes()
            .enumerate()
            .all(|(i, b)| i == 64 || b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        && name[..64].bytes().any(|b| b != b'0')
        && name[65..].bytes().any(|b| b != b'0')
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
        || scope.subscriptions.len() > 7
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
        || names.any(|name| !valid_namespace(name) || name[..64] != first[..64])
        || scope.subscriptions.iter().enumerate().any(|(index, name)| {
            scope.publication.as_ref() == Some(name) || scope.subscriptions[..index].contains(name)
        })
    {
        return Err(ClientError::Invalid);
    }
    Ok(url)
}
async fn run(shared: Arc<Shared>, native: moq_native::Client, url: url::Url) {
    let session = match native.connect(url).await {
        Ok(session) => session,
        Err(_) => {
            shared.state.lock().unwrap().end(Status::Failed);
            return;
        }
    };
    if session.version().as_str() != "moq-transport-21" || shared.check().is_err() {
        session.abort(moq_net::Error::Cancel);
        return;
    }
    // The relay sends its SETUP only after host admission; until then the
    // transport is merely open, not live.
    let admitted = tokio::select! {
        _ = session.setup_received() => true,
        _ = session.closed() => false,
        _ = tokio::time::sleep(CONNECT_TIMEOUT) => false,
    };
    if !admitted {
        shared.state.lock().unwrap().end(Status::Failed);
        session.abort(moq_net::Error::Cancel);
        return;
    }
    if shared.check().is_err() {
        session.abort(moq_net::Error::Cancel);
        return;
    }
    {
        let mut state = shared.state.lock().unwrap();
        if state.status != Status::Connecting {
            session.abort(moq_net::Error::Cancel);
            return;
        }
        state.session = Some(session.clone());
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
            _ = session.closed() => { shared.state.lock().unwrap().end(Status::Failed); break; }
            _ = tick.tick() => {
                if shared.check().is_err() { break; }
                let mut state = shared.state.lock().unwrap();
                if state.status != Status::Live { break; }
                state.expire(Instant::now());
                drop(state);
                shared.outgoing.expire(); shared.incoming.expire();
            }
            _ = readers.join_next(), if !readers.is_empty() => {
                // A closed remote publication can return under the same exact
                // namespace with a restarted sequence. Retire this carrier so
                // its owner obtains a fresh proof/generation and decoder state.
                shared.state.lock().unwrap().end(Status::Failed);
                break;
            }
        }
    }
    readers.abort_all();
    session.abort(moq_net::Error::Cancel);
}
async fn receive(shared: Arc<Shared>, name: String) {
    let Some(broadcast) = shared
        .incoming
        .consume()
        .announced_broadcast(name.as_str())
        .await
    else {
        return;
    };
    let Ok(track) = broadcast.track("audio") else {
        return;
    };
    // Browser publishers send each object as a Draft 21 OBJECT_DATAGRAM bundle
    // (newest first, then redundant earlier objects); a separate local
    // subscription drains that best-effort channel alongside the groups.
    let datagrams = match broadcast.track("audio") {
        Ok(track) => track
            .subscribe(moq_net::track::Subscription::default().with_latency_max(RECEIVE_AGE))
            .await
            .ok(),
        Err(_) => None,
    };
    let Ok(track) = track
        .subscribe(moq_net::track::Subscription::default().with_latency_max(RECEIVE_AGE))
        .await
    else {
        return;
    };
    let last = Mutex::new(None);
    let groups = receive_groups(&shared, &name, track, &last);
    match datagrams {
        Some(datagrams) => {
            tokio::select! {
                _ = groups => {}
                _ = receive_datagrams(&shared, &name, datagrams, &last) => {}
            }
        }
        None => groups.await,
    }
}

/// Queue one received object. Returns false when the carrier must stop.
fn deliver(
    shared: &Shared,
    name: &str,
    group: u64,
    object: VoiceObject,
    last: &Mutex<Option<u64>>,
) -> bool {
    let mut state = shared.state.lock().unwrap();
    if state.status != Status::Live {
        return false;
    }
    let mut last = last.lock().unwrap();
    if last.is_some_and(|prior| object.sequence <= prior) {
        state.counters.duplicate_dropped = state.counters.duplicate_dropped.saturating_add(1);
        return true;
    }
    *last = Some(object.sequence);
    state.expire(Instant::now());
    if state
        .received
        .get(name)
        .is_some_and(|queue| queue.len() >= RECEIVE_FRAMES)
    {
        state.received.get_mut(name).unwrap().pop_front();
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
            expires_at: arrived + RECEIVE_AGE,
        });
    state.counters.received = state.counters.received.saturating_add(1);
    true
}

async fn receive_datagrams(
    shared: &Shared,
    name: &str,
    mut track: moq_net::track::Subscriber,
    last: &Mutex<Option<u64>>,
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
            if !deliver(shared, name, datagram.sequence, object, last) {
                return;
            }
        }
    }
}

async fn receive_groups(
    shared: &Shared,
    name: &str,
    mut track: moq_net::track::Subscriber,
    last: &Mutex<Option<u64>>,
) {
    let mut next = track.recv_group().await;
    while let Ok(Some(mut group)) = next {
        let expires_at = Instant::now() + RECEIVE_AGE;
        let mut count = 0;
        loop {
            tokio::select! {
                biased;
                fresh = track.recv_group() => { next = fresh; break; }
                _ = tokio::time::sleep_until(expires_at.into()) => { next = track.recv_group().await; break; }
                value = group.read_frame() => {
                    let Ok(Some(frame)) = value else { next = track.recv_group().await; break; };
                    if shared.check().is_err() { return; }
                    count += 1;
                    let Ok(object) = VoiceObject::decode(&frame.payload) else { return; };
                    if count > voice::FRAMES_PER_GROUP || object.sequence / voice::FRAMES_PER_GROUP != group.sequence { return; }
                    // The group deadline bounds the read.
                    if Instant::now() >= expires_at {
                        let mut state = shared.state.lock().unwrap();
                        if state.status != Status::Live { return; }
                        state.counters.late_dropped = state.counters.late_dropped.saturating_add(1); continue;
                    }
                    if !deliver(shared, name, group.sequence, object, last) { return; }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests;
