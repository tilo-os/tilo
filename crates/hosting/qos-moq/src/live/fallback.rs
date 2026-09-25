//! LIVE_V1 fallback carrier: the same live relay reached over plain HTTPS
//! (HTTP/1.1, HTTP/2 or HTTP/3) for browsers or networks without a working
//! WebTransport (no UDP, no WebTransport API, or a certificate WebTransport
//! does not accept). No WebSockets.
//!
//! * `POST /peer/v3/live/fallback/open`, body = the exact `SignedLiveGrant`
//!   bytes (the MoQ Authorization Token): admitted by the same
//!   [`LiveAuthority::try_admit`] and lease/roster [`LiveSession::status`]
//!   observations as `/moq/v21/live`. The response body is a stream of
//!   server frames; the first is `HELLO` with a random 128-bit session id.
//! * `POST /peer/v3/live/fallback/send`, header `x-tilo-live-session`, body =
//!   client frames (presence, ops, ops subscriptions). Answered `204`.
//! * `POST /peer/v3/live/fallback/resume`, header `x-tilo-live-session`: a new
//!   response stream for the same session (stream rotation, reconnect). The
//!   session survives a detached stream for [`RESUME_GRACE`].
//!
//! Bridging. A fallback publisher is attached to its namespace slot exactly
//! like a WebTransport publisher: an in-process broadcast with a `presence`
//! datagram track and on-demand `ops/<record>` tracks, pumped by the same
//! validated, rate-limited relay pumps into the shared egress. A fallback
//! subscriber reads the same egress origin (room scope) as a MoQ subscriber.
//! So MoQ and fallback members see each other in both directions, with the
//! same frame bytes, limits and latest-value presence.
//!
//! Frame: `type u8 | length u32 (big endian) | payload`.
use super::*;
use bytes::Bytes;
use std::collections::VecDeque;
use std::sync::atomic::AtomicBool;
use std::task::{Context, Poll};
use tokio::sync::{Notify, mpsc};

pub const FALLBACK_OPEN_PATH: &str = "/peer/v3/live/fallback/open";
pub const FALLBACK_RESUME_PATH: &str = "/peer/v3/live/fallback/resume";
pub const FALLBACK_SEND_PATH: &str = "/peer/v3/live/fallback/send";
/// Lowercase hex of the 16-byte session id.
pub const FALLBACK_SESSION_HEADER: &str = "x-tilo-live-session";
/// Largest client frame batch (one `send` body).
pub const MAX_SEND_BYTES: usize = 256 * 1024;
/// Server frames: session hello (`id[16] | flags u8`, bit0 publish, bit1 subscribe).
pub const FRAME_HELLO: u8 = 0x01;
/// `publisher[32] | active u8`.
pub const FRAME_MEMBER: u8 = 0x02;
/// `publisher[32] | PresenceFrame`.
pub const FRAME_PRESENCE: u8 = 0x03;
/// `publisher[32] | record[32] | OpsFrame`.
pub const FRAME_OPS: u8 = 0x04;
/// `publisher[32] | record[32]`: that ops subscription ended (reset, withdrawn, overflow).
pub const FRAME_OPS_END: u8 = 0x05;
/// Empty keepalive.
pub const FRAME_PING: u8 = 0x06;
/// `reason u8`: this stream ends. [`END_ROTATE`] means resume; any other the session ended.
pub const FRAME_END: u8 = 0x07;
pub const END_ROTATE: u8 = 0;
pub const END_REVOKED: u8 = 1;
pub const END_CLOSED: u8 = 2;
/// Client frames: `PresenceFrame`.
pub const FRAME_SEND_PRESENCE: u8 = 0x11;
/// `record[32] | OpsFrame`. Consecutive frames of one record in one body form one group.
pub const FRAME_SEND_OPS: u8 = 0x12;
/// `record[32]`: this client finished writing the record.
pub const FRAME_SEND_OPS_CLOSE: u8 = 0x13;
/// `publisher[32] | record[32]`.
pub const FRAME_SUBSCRIBE_OPS: u8 = 0x14;
/// `publisher[32] | record[32]`.
pub const FRAME_UNSUBSCRIBE_OPS: u8 = 0x15;
/// Empty: end the session now.
pub const FRAME_SEND_CLOSE: u8 = 0x16;

/// A stream is rotated (ended with [`END_ROTATE`]) after this long so long-lived
/// responses never pin one HTTP connection past its lifetime.
pub const STREAM_LIFETIME: Duration = Duration::from_secs(120);
/// A session without an attached stream ends after this long.
pub const RESUME_GRACE: Duration = Duration::from_secs(10);
const KEEPALIVE: Duration = Duration::from_secs(15);
/// Undelivered ops bytes per session; beyond it a subscription is reset.
const PENDING_OPS_BYTES: usize = 512 * 1024;
/// Server chunk target; one frame may exceed it.
const CHUNK_BYTES: usize = 64 * 1024;
const STREAM_QUEUE: usize = 4;
const MAX_REMOTE_PUBLISHERS: usize = 64;
const MAX_OPS_SUBSCRIPTIONS: usize = 256;
const MAX_SESSIONS: usize = MAX_LIVE_SESSIONS;
const PRESENCE_LATENCY: Duration = Duration::from_millis(50);
const OPS_LATENCY: Duration = Duration::from_millis(2500);

/// Why a fallback request was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FallbackError {
    /// Token or session refused, revoked, or direction not granted (403).
    Denied,
    /// Unknown or ended session (404): open a new one with a fresh grant.
    Gone,
    /// Session capacity or bounded contention (503).
    Busy,
    /// Malformed frames (400).
    Invalid,
}

/// Registry of fallback sessions of one relay.
#[derive(Default)]
pub(super) struct Registry {
    sessions: Mutex<HashMap<[u8; 16], Arc<Fallback>>>,
}

/// One HTTPS response stream of server frames.
pub struct FallbackStream {
    receiver: mpsc::Receiver<Bytes>,
}
impl FallbackStream {
    pub fn poll_next(&mut self, cx: &mut Context<'_>) -> Poll<Option<Bytes>> {
        self.receiver.poll_recv(cx)
    }
    pub async fn next(&mut self) -> Option<Bytes> {
        self.receiver.recv().await
    }
}

fn put(out: &mut Vec<u8>, kind: u8, parts: &[&[u8]]) {
    let len: usize = parts.iter().map(|part| part.len()).sum();
    out.push(kind);
    out.extend_from_slice(&(len as u32).to_be_bytes());
    for part in parts {
        out.extend_from_slice(part);
    }
}
/// Encode one frame (server or client).
pub fn encode_frame(kind: u8, parts: &[&[u8]]) -> Vec<u8> {
    let mut out = Vec::new();
    put(&mut out, kind, parts);
    out
}
/// Split a frame sequence; `None` if malformed.
pub fn decode_frames(mut bytes: &[u8]) -> Option<Vec<(u8, &[u8])>> {
    let mut frames = Vec::new();
    while !bytes.is_empty() {
        if bytes.len() < 5 {
            return None;
        }
        let kind = bytes[0];
        let len = u32::from_be_bytes(bytes[1..5].try_into().ok()?) as usize;
        let rest = &bytes[5..];
        if rest.len() < len {
            return None;
        }
        frames.push((kind, &rest[..len]));
        bytes = &rest[len..];
    }
    Some(frames)
}
fn key32(bytes: &[u8]) -> Option<[u8; 32]> {
    bytes.get(..32)?.try_into().ok()
}
fn unhex32(value: &str) -> Option<[u8; 32]> {
    let mut out = [0; 32];
    (value.len() == 64 && hex64(value)).then_some(())?;
    for (index, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&value[index * 2..index * 2 + 2], 16).ok()?;
    }
    Some(out)
}
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}
fn ops_name(record: &[u8; 32]) -> String {
    format!("{OPS_PREFIX}{}", hex(record))
}
/// Parse a session header value.
pub fn session_id(value: &str) -> Option<[u8; 16]> {
    if value.len() != 32
        || !value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return None;
    }
    let mut out = [0; 16];
    for (index, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&value[index * 2..index * 2 + 2], 16).ok()?;
    }
    Some(out)
}

#[derive(Default)]
struct Outbox {
    members: VecDeque<([u8; 32], bool)>,
    presence: HashMap<[u8; 32], Bytes>,
    ops: VecDeque<Vec<u8>>,
    ops_bytes: usize,
    end: Option<u8>,
}

struct OpsTrack {
    track: moq_net::track::Producer,
    used: Instant,
}

#[derive(Default)]
struct Publish {
    broadcast: Option<moq_net::broadcast::Producer>,
    presence: Option<moq_net::track::Producer>,
    /// Records this client writes (<= [`MAX_OPEN_RECORDS`], LRU).
    writing: HashMap<String, OpsTrack>,
    /// Tracks the relay requested on demand (<= [`MAX_EGRESS_OPS_TRACKS`]).
    served: HashMap<String, moq_net::track::Producer>,
}

#[derive(Default)]
struct Subscribe {
    members: HashMap<[u8; 32], (moq_net::broadcast::Consumer, Task)>,
    ops: HashMap<([u8; 32], [u8; 32]), Task>,
}

struct Fallback {
    id: [u8; 16],
    relay: u64,
    path: String,
    room: String,
    publisher: [u8; 32],
    publish_allowed: bool,
    subscribe_allowed: bool,
    outbox: Mutex<Outbox>,
    publish: Mutex<Publish>,
    subscribe: Mutex<Subscribe>,
    notify: Notify,
    /// Current stream generation and when the last stream detached.
    stream: Mutex<(u64, Option<Instant>)>,
    ended: AtomicBool,
    end: Notify,
}

impl Fallback {
    fn push_member(&self, publisher: [u8; 32], active: bool) {
        let mut outbox = self.outbox.lock().unwrap();
        if !active {
            outbox.presence.remove(&publisher);
        }
        outbox.members.push_back((publisher, active));
        drop(outbox);
        self.notify.notify_one();
    }
    fn push_presence(&self, publisher: [u8; 32], payload: Bytes) {
        self.outbox
            .lock()
            .unwrap()
            .presence
            .insert(publisher, payload);
        self.notify.notify_one();
    }
    /// Queue an ops frame; `false` when over budget (the caller resets).
    fn push_ops(&self, publisher: &[u8; 32], record: &[u8; 32], payload: &[u8]) -> bool {
        let mut outbox = self.outbox.lock().unwrap();
        if outbox.ops_bytes + payload.len() > PENDING_OPS_BYTES {
            return false;
        }
        let frame = encode_frame(FRAME_OPS, &[publisher, record, payload]);
        outbox.ops_bytes += frame.len();
        outbox.ops.push_back(frame);
        drop(outbox);
        self.notify.notify_one();
        true
    }
    fn push_ops_end(&self, publisher: &[u8; 32], record: &[u8; 32]) {
        let frame = encode_frame(FRAME_OPS_END, &[publisher, record]);
        let mut outbox = self.outbox.lock().unwrap();
        outbox.ops_bytes += frame.len();
        outbox.ops.push_back(frame);
        drop(outbox);
        self.notify.notify_one();
    }
    /// Drain queued frames into one chunk (members, then presence, then ops).
    fn take_chunk(&self, out: &mut Vec<u8>) -> Option<u8> {
        let mut outbox = self.outbox.lock().unwrap();
        while let Some((publisher, active)) = outbox.members.pop_front() {
            put(out, FRAME_MEMBER, &[&publisher, &[u8::from(active)]]);
        }
        for (publisher, payload) in outbox.presence.drain() {
            put(out, FRAME_PRESENCE, &[&publisher, &payload]);
        }
        while out.len() < CHUNK_BYTES {
            let Some(frame) = outbox.ops.pop_front() else {
                break;
            };
            outbox.ops_bytes -= frame.len();
            out.extend_from_slice(&frame);
        }
        outbox.end.filter(|_| outbox.ops.is_empty())
    }
    fn finish(&self, reason: u8) {
        if self.ended.swap(true, Ordering::SeqCst) {
            return;
        }
        self.outbox.lock().unwrap().end.get_or_insert(reason);
        self.notify.notify_one();
        self.end.notify_waiters();
        self.end.notify_one();
    }
}

impl LiveRelay {
    fn fallback_session(&self, id: &[u8; 16]) -> Option<Arc<Fallback>> {
        self.0.fallback.sessions.lock().unwrap().get(id).cloned()
    }

    /// Admit `token` (the exact grant bytes) and open a fallback session with
    /// its first response stream. `id` must come from a CSPRNG (the host's OS
    /// randomness): it is the bearer handle of `send` and `resume`. Must run
    /// inside Tokio.
    pub async fn fallback_open(
        &self,
        token: &[u8],
        id: [u8; 16],
    ) -> Result<FallbackStream, FallbackError> {
        let inner = &self.0;
        if inner.fallback.sessions.lock().unwrap().len() >= MAX_SESSIONS {
            return Err(FallbackError::Busy);
        }
        let Ok(permit) = inner.sessions.clone().try_acquire_owned() else {
            inner.counters.rejected.fetch_add(1, Ordering::Relaxed);
            return Err(FallbackError::Busy);
        };
        let Some(scopes) = self
            .scopes(
                "moq-transport-21".parse().expect("fixed version"),
                Some(token),
            )
            .await
        else {
            inner.counters.rejected.fetch_add(1, Ordering::Relaxed);
            return Err(FallbackError::Denied);
        };
        let publisher = unhex32(scopes.session.publisher()).ok_or(FallbackError::Denied)?;
        if id == [0; 16] || inner.fallback.sessions.lock().unwrap().contains_key(&id) {
            return Err(FallbackError::Busy);
        }
        let relay_session = inner.next_session.fetch_add(1, Ordering::Relaxed);
        let session = Arc::new(Fallback {
            id,
            relay: relay_session,
            path: scopes.path.clone(),
            room: scopes.session.room().to_owned(),
            publisher,
            publish_allowed: scopes.session.publish(),
            subscribe_allowed: scopes.session.subscribe(),
            outbox: Mutex::default(),
            publish: Mutex::default(),
            subscribe: Mutex::default(),
            notify: Notify::new(),
            stream: Mutex::new((0, None)),
            ended: AtomicBool::new(false),
            end: Notify::new(),
        });
        inner
            .fallback
            .sessions
            .lock()
            .unwrap()
            .insert(id, session.clone());
        inner.counters.accepted.fetch_add(1, Ordering::Relaxed);
        inner.counters.active.fetch_add(1, Ordering::Relaxed);
        if session.publish_allowed {
            self.fallback_attach(&session);
        }
        let watch = session.subscribe_allowed.then(|| {
            Task::spawn(watch_members(
                Arc::downgrade(inner),
                Arc::downgrade(&session),
            ))
        });
        let relay = self.clone();
        let life = session.clone();
        tokio::spawn(async move {
            let _permit = permit;
            let _watch = watch;
            relay.fallback_life(life, scopes.session).await;
        });
        Ok(self.fallback_stream(&session))
    }

    /// A new response stream for an existing session.
    pub fn fallback_resume(&self, id: &[u8; 16]) -> Result<FallbackStream, FallbackError> {
        let session = self.fallback_session(id).ok_or(FallbackError::Gone)?;
        if session.ended.load(Ordering::SeqCst) {
            return Err(FallbackError::Gone);
        }
        Ok(self.fallback_stream(&session))
    }

    /// Apply one client frame batch.
    pub fn fallback_send(&self, id: &[u8; 16], body: &[u8]) -> Result<(), FallbackError> {
        if body.len() > MAX_SEND_BYTES {
            return Err(FallbackError::Invalid);
        }
        let session = self.fallback_session(id).ok_or(FallbackError::Gone)?;
        if session.ended.load(Ordering::SeqCst) {
            return Err(FallbackError::Gone);
        }
        let frames = decode_frames(body).ok_or(FallbackError::Invalid)?;
        let mut groups: HashMap<String, moq_net::group::Producer> = HashMap::new();
        let result = frames.into_iter().try_for_each(|(kind, payload)| {
            self.fallback_frame(&session, kind, payload, &mut groups)
        });
        for (_, mut group) in groups {
            let _ = group.finish();
        }
        result
    }

    fn fallback_frame(
        &self,
        session: &Arc<Fallback>,
        kind: u8,
        payload: &[u8],
        groups: &mut HashMap<String, moq_net::group::Producer>,
    ) -> Result<(), FallbackError> {
        match kind {
            FRAME_SEND_PRESENCE => {
                if !session.publish_allowed {
                    return Err(FallbackError::Denied);
                }
                if payload.is_empty() || payload.len() > MAX_DATAGRAM_FRAME_BYTES {
                    return Err(FallbackError::Invalid);
                }
                let mut publish = session.publish.lock().unwrap();
                if let Some(track) = publish.presence.as_mut() {
                    let _ = track
                        .append_datagram(moq_net::Timestamp::ZERO, Bytes::copy_from_slice(payload));
                }
                Ok(())
            }
            FRAME_SEND_OPS => {
                if !session.publish_allowed {
                    return Err(FallbackError::Denied);
                }
                let record = key32(payload).ok_or(FallbackError::Invalid)?;
                let frame = &payload[32..];
                if frame.is_empty() || frame.len() > MAX_STREAM_FRAME_BYTES {
                    return Err(FallbackError::Invalid);
                }
                let name = ops_name(&record);
                if !groups.contains_key(&name) {
                    let mut publish = session.publish.lock().unwrap();
                    let Some(track) = ops_writer(&mut publish, &name) else {
                        return Ok(());
                    };
                    let Ok(group) = track.append_group() else {
                        return Ok(());
                    };
                    groups.insert(name.clone(), group);
                }
                if let Some(group) = groups.get_mut(&name) {
                    let _ =
                        group.write_frame(moq_net::Timestamp::ZERO, Bytes::copy_from_slice(frame));
                }
                Ok(())
            }
            FRAME_SEND_OPS_CLOSE => {
                let record = key32(payload).ok_or(FallbackError::Invalid)?;
                let name = ops_name(&record);
                if let Some(mut group) = groups.remove(&name) {
                    let _ = group.finish();
                }
                let mut publish = session.publish.lock().unwrap();
                if let Some(entry) = publish.writing.remove(&name) {
                    let mut track = entry.track;
                    let _ = track.finish();
                    if let Some(broadcast) = publish.broadcast.as_mut() {
                        let _ = broadcast.remove_track(&name);
                    }
                }
                Ok(())
            }
            FRAME_SUBSCRIBE_OPS | FRAME_UNSUBSCRIBE_OPS => {
                if !session.subscribe_allowed {
                    return Err(FallbackError::Denied);
                }
                let (publisher, record) = key32(payload)
                    .zip(payload.get(32..).and_then(key32))
                    .filter(|_| payload.len() == 64)
                    .ok_or(FallbackError::Invalid)?;
                if kind == FRAME_UNSUBSCRIBE_OPS {
                    session
                        .subscribe
                        .lock()
                        .unwrap()
                        .ops
                        .remove(&(publisher, record));
                    return Ok(());
                }
                self.fallback_subscribe_ops(session, publisher, record);
                Ok(())
            }
            FRAME_SEND_CLOSE => {
                session.finish(END_CLOSED);
                Ok(())
            }
            _ => Err(FallbackError::Invalid),
        }
    }

    fn fallback_subscribe_ops(
        &self,
        session: &Arc<Fallback>,
        publisher: [u8; 32],
        record: [u8; 32],
    ) {
        let mut subscribe = session.subscribe.lock().unwrap();
        if subscribe.ops.contains_key(&(publisher, record)) {
            return;
        }
        let Some((broadcast, _)) = subscribe.members.get(&publisher) else {
            drop(subscribe);
            session.push_ops_end(&publisher, &record);
            return;
        };
        if subscribe.ops.len() >= MAX_OPS_SUBSCRIPTIONS {
            drop(subscribe);
            session.push_ops_end(&publisher, &record);
            return;
        }
        let task = Task::spawn(pump_ops(
            Arc::downgrade(session),
            broadcast.clone(),
            publisher,
            record,
        ));
        subscribe.ops.insert((publisher, record), task);
    }

    /// Attach the session's in-process broadcast to its namespace slot, exactly
    /// where a WebTransport publisher's announced broadcast would attach.
    fn fallback_attach(&self, session: &Arc<Fallback>) {
        let mut info = moq_net::broadcast::Info::new();
        info.origin = moq_net::origin::Info::new(moq_net::Origin::random())
            .with_pool(moq_net::cache::Pool::new(INGRESS_POOL_BYTES))
            .with_cache_duration(OPS_WINDOW)
            .with_latency_default(OPS_WINDOW);
        let mut broadcast = info.produce();
        let presence = broadcast
            .create_track(
                PRESENCE_TRACK,
                moq_net::track::Info::default()
                    .with_ordered(false)
                    .with_latency_max(PRESENCE_LATENCY),
            )
            .ok();
        let mut dynamic = broadcast.dynamic();
        let consumer = broadcast.consume();
        {
            let mut publish = session.publish.lock().unwrap();
            publish.broadcast = Some(broadcast);
            publish.presence = presence;
        }
        // Serve the relay's on-demand ops track requests (subscriber demand).
        let weak = Arc::downgrade(session);
        let serve = Task::spawn(async move {
            while let Ok(request) = dynamic.requested_track().await {
                let Some(session) = weak.upgrade() else { break };
                let name = request.name().to_owned();
                let mut publish = session.publish.lock().unwrap();
                publish.served.retain(|_, track| !track.is_closed());
                if ops_record(&name).is_none() || publish.served.len() >= MAX_EGRESS_OPS_TRACKS {
                    request.reject(moq_net::Error::NotFound);
                    continue;
                }
                let track = request.accept(ops_info());
                if let Some(entry) = publish.writing.get_mut(&name) {
                    entry.track = track;
                } else {
                    publish.served.insert(name, track);
                }
            }
        });
        // The serving task ends with the broadcast (dropped at session end).
        serve.detach();
        self.0.attach_client(&session.path, session.relay, consumer);
    }

    fn fallback_stream(&self, session: &Arc<Fallback>) -> FallbackStream {
        let (sender, receiver) = mpsc::channel(STREAM_QUEUE);
        let generation = {
            let mut stream = session.stream.lock().unwrap();
            stream.0 += 1;
            stream.1 = None;
            stream.0
        };
        tokio::spawn(write_stream(session.clone(), generation, sender));
        FallbackStream { receiver }
    }

    /// Lifecycle: lease/roster observations exactly as a WebTransport session,
    /// resume grace, and cleanup (revocation withdraws only this namespace).
    async fn fallback_life(&self, session: Arc<Fallback>, permit: Arc<dyn LiveSession>) {
        let inner = &self.0;
        let mut validity = tokio::time::interval(AUTHORITY_INTERVAL);
        validity.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut unavailable: Option<Instant> = None;
        let revoked = loop {
            tokio::select! {
                _ = session.end.notified() => break false,
                _ = validity.tick() => {
                    if session.ended.load(Ordering::SeqCst) {
                        break false;
                    }
                    let now = Instant::now();
                    if unavailable.is_some_and(|deadline| now >= deadline) {
                        break true;
                    }
                    match permit.status() {
                        AuthorizationStatus::Current => unavailable = None,
                        AuthorizationStatus::Unavailable { expires_at } => unavailable = Some(expires_at),
                        AuthorizationStatus::Denied => break true,
                    }
                    let detached = session.stream.lock().unwrap().1;
                    if detached.is_some_and(|at| now.duration_since(at) >= RESUME_GRACE) {
                        break false;
                    }
                }
            }
        };
        session.finish(if revoked { END_REVOKED } else { END_CLOSED });
        inner.fallback.sessions.lock().unwrap().remove(&session.id);
        {
            let mut subscribe = session.subscribe.lock().unwrap();
            subscribe.ops.clear();
            subscribe.members.clear();
        }
        let broadcast = {
            let mut publish = session.publish.lock().unwrap();
            publish.writing.clear();
            publish.served.clear();
            publish.presence = None;
            publish.broadcast.take()
        };
        if revoked {
            inner.counters.revoked.fetch_add(1, Ordering::Relaxed);
            let host_fed =
                session.publish_allowed && inner.remove_slot(&session.path, Some(session.relay));
            if host_fed {
                let relay = Arc::downgrade(inner);
                let path = session.path.clone();
                tokio::spawn(async move {
                    tokio::time::sleep(REANNOUNCE_AFTER).await;
                    if let Some(inner) = relay.upgrade()
                        && let Some(slot) = inner.slot(&path, true)
                    {
                        slot.state
                            .lock()
                            .unwrap()
                            .host_active
                            .get_or_insert_with(Instant::now);
                    }
                });
            }
        } else if session.publish_allowed {
            inner.detach_client(&session.path, session.relay);
        }
        if let Some(mut broadcast) = broadcast {
            broadcast.finish();
        }
        inner.counters.active.fetch_sub(1, Ordering::Relaxed);
    }
}

fn ops_info() -> moq_net::track::Info {
    moq_net::track::Info::default()
        .with_ordered(true)
        .with_latency_max(Duration::from_millis(2000))
}

/// The track this client writes `name` on: a relay-requested track if one is
/// served, else a new one (evicting the least recently written record).
fn ops_writer<'a>(
    publish: &'a mut Publish,
    name: &str,
) -> Option<&'a mut moq_net::track::Producer> {
    let usable = publish
        .writing
        .get(name)
        .is_some_and(|entry| !entry.track.is_closed());
    if !usable {
        publish.writing.remove(name);
        if publish.writing.len() >= MAX_OPEN_RECORDS
            && let Some(oldest) = publish
                .writing
                .iter()
                .min_by_key(|(_, entry)| entry.used)
                .map(|(name, _)| name.clone())
        {
            if let Some(entry) = publish.writing.remove(&oldest) {
                let mut track = entry.track;
                let _ = track.finish();
            }
            if let Some(broadcast) = publish.broadcast.as_mut() {
                let _ = broadcast.remove_track(&oldest);
            }
        }
        let track = match publish
            .served
            .remove(name)
            .filter(|track| !track.is_closed())
        {
            Some(track) => track,
            None => {
                let broadcast = publish.broadcast.as_mut()?;
                let _ = broadcast.remove_track(name);
                broadcast.create_track(name.to_owned(), ops_info()).ok()?
            }
        };
        publish.writing.insert(
            name.to_owned(),
            OpsTrack {
                track,
                used: Instant::now(),
            },
        );
    }
    let entry = publish.writing.get_mut(name)?;
    entry.used = Instant::now();
    Some(&mut entry.track)
}

async fn write_stream(session: Arc<Fallback>, generation: u64, sender: mpsc::Sender<Bytes>) {
    let started = Instant::now();
    let mut first = true;
    let current = || session.stream.lock().unwrap().0 == generation;
    loop {
        let mut chunk = Vec::new();
        if first {
            let flags =
                u8::from(session.publish_allowed) | (u8::from(session.subscribe_allowed) << 1);
            put(&mut chunk, FRAME_HELLO, &[&session.id, &[flags]]);
            first = false;
        }
        let end = session.take_chunk(&mut chunk);
        let rotate = end.is_none() && started.elapsed() >= STREAM_LIFETIME;
        if let Some(reason) = end.or(rotate.then_some(END_ROTATE)) {
            put(&mut chunk, FRAME_END, &[&[reason]]);
            let _ = sender.send(Bytes::from(chunk)).await;
            break;
        }
        if !chunk.is_empty() {
            if !current() || sender.send(Bytes::from(chunk)).await.is_err() {
                break;
            }
            continue;
        }
        let wait = KEEPALIVE.min(STREAM_LIFETIME.saturating_sub(started.elapsed()));
        tokio::select! {
            _ = session.notify.notified() => {}
            _ = sender.closed() => break,
            _ = tokio::time::sleep(wait) => {
                if started.elapsed() < STREAM_LIFETIME {
                    let mut ping = Vec::new();
                    put(&mut ping, FRAME_PING, &[]);
                    if !current() || sender.send(Bytes::from(ping)).await.is_err() {
                        break;
                    }
                }
            }
        }
        if !current() {
            // A newer stream replaced this one: hand over a wake-up this
            // writer may have consumed.
            session.notify.notify_one();
            break;
        }
    }
    let mut stream = session.stream.lock().unwrap();
    if stream.0 == generation {
        stream.1 = Some(Instant::now());
    }
}

async fn watch_members(inner: Weak<Inner>, session: Weak<Fallback>) {
    let (egress, room, own) = {
        let (Some(inner), Some(session)) = (inner.upgrade(), session.upgrade()) else {
            return;
        };
        let room = format!("{NAMESPACE_ROOT}/{}", session.room);
        let Some(egress) = inner.egress.consume().scope(&[room.as_str().into()]) else {
            return;
        };
        (egress, room, session.publisher)
    };
    let mut announced = egress.announced();
    while let Some(announce) = announced.next().await {
        let Some(session) = session.upgrade() else {
            return;
        };
        let path = announce.path.as_str();
        let rest = path
            .strip_prefix(room.as_str())
            .map(|rest| rest.trim_start_matches('/'))
            .unwrap_or(path);
        let Some(publisher) = unhex32(rest) else {
            continue;
        };
        if publisher == own {
            continue;
        }
        match announce.broadcast {
            Some(broadcast) => {
                let mut subscribe = session.subscribe.lock().unwrap();
                let known = subscribe.members.remove(&publisher).is_some();
                if !known && subscribe.members.len() >= MAX_REMOTE_PUBLISHERS {
                    continue;
                }
                subscribe.ops.retain(|(member, _), _| *member != publisher);
                let task = Task::spawn(pump_presence(
                    Arc::downgrade(&session),
                    broadcast.clone(),
                    publisher,
                ));
                subscribe.members.insert(publisher, (broadcast, task));
                drop(subscribe);
                if known {
                    session.push_member(publisher, false);
                }
                session.push_member(publisher, true);
            }
            None => {
                let removed = {
                    let mut subscribe = session.subscribe.lock().unwrap();
                    subscribe.ops.retain(|(member, _), _| *member != publisher);
                    subscribe.members.remove(&publisher).is_some()
                };
                if removed {
                    session.push_member(publisher, false);
                }
            }
        }
    }
}

async fn pump_presence(
    session: Weak<Fallback>,
    broadcast: moq_net::broadcast::Consumer,
    publisher: [u8; 32],
) {
    loop {
        let mut subscription = moq_net::track::Subscription::default();
        subscription.latency_max = PRESENCE_LATENCY;
        let subscriber = match broadcast.track(PRESENCE_TRACK) {
            Ok(track) => track.subscribe(subscription).await.ok(),
            Err(_) => None,
        };
        if let Some(mut subscriber) = subscriber {
            while let Ok(Some(datagram)) = subscriber.recv_datagram().await {
                let Some(session) = session.upgrade() else {
                    return;
                };
                session.push_presence(publisher, datagram.payload);
            }
        }
        if broadcast.is_closed() {
            return;
        }
        tokio::time::sleep(PRESENCE_RETRY).await;
    }
}

async fn pump_ops(
    session: Weak<Fallback>,
    broadcast: moq_net::broadcast::Consumer,
    publisher: [u8; 32],
    record: [u8; 32],
) {
    let name = ops_name(&record);
    let mut subscription = moq_net::track::Subscription::default();
    subscription.ordered = true;
    subscription.priority = 1;
    subscription.latency_max = OPS_LATENCY;
    let subscriber = match broadcast.track(&name) {
        Ok(track) => track.subscribe(subscription).await.ok(),
        Err(_) => None,
    };
    if let Some(mut subscriber) = subscriber {
        'groups: while let Ok(Some(mut group)) = subscriber.recv_group().await {
            loop {
                match group.read_frame().await {
                    Ok(Some(frame)) => {
                        let Some(session) = session.upgrade() else {
                            return;
                        };
                        if !session.push_ops(&publisher, &record, &frame.payload) {
                            break 'groups;
                        }
                    }
                    Ok(None) => break,
                    Err(_) => break,
                }
            }
        }
    }
    if let Some(session) = session.upgrade() {
        if let Some(task) = session
            .subscribe
            .lock()
            .unwrap()
            .ops
            .remove(&(publisher, record))
        {
            task.detach();
        }
        session.push_ops_end(&publisher, &record);
    }
}

#[cfg(test)]
mod tests;
