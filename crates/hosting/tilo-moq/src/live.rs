//! `LIVE_V1` relay: ephemeral multiplayer presence and ordered ops carried as
//! `moq-lite` tracks over WebTransport at [`LIVE_PATH`], beside voice.
//!
//! The relay never stores, orders durably or authorizes content. The native
//! host supplies a [`LiveAuthority`] that verifies the host-signed live grant
//! presented in the carrier's admission preface ([`crate::carrier`]), rechecks
//! current membership on every observation, and validates frame bytes (the
//! canonical wire codec lives in the host crate; this crate stays free of that
//! dependency).
//!
//! Topology. Every admitted publisher session gets a private ingress origin
//! scoped to its own namespace `tilo-live/<room>/<publisher>` and the paths
//! below it. In that origin the client announces:
//!
//! * the namespace broadcast itself, carrying the `presence` track: object
//!   datagrams (single-frame groups where datagrams are unavailable);
//! * one broadcast per record it is writing, `<namespace>/ops/<hex record
//!   digest>`, carrying one ordered track [`RECORD_TRACK`]. Announcing a record
//!   broadcast opens the record; ending it closes the record.
//!
//! The relay (not the client) owns the egress broadcast for the namespace in
//! one shared egress origin and pumps validated, rate-limited frames into it:
//!
//! * `presence`: forwarded immediately and latest-value (queued upstream
//!   datagrams are coalesced to the newest, stale sequence numbers are
//!   dropped, the egress send path drops when congested).
//! * `ops/<hex record digest>`: ordered streams, served on subscriber demand.
//!   Each upstream group becomes one egress group, frames in order.
//!
//! Subscribers receive a scope of their room prefix, served from a view the
//! host owns per session ([`crate::mirror`]), and subscribe per member. The
//! host may inject frames into a member's `ops/<record>` track
//! ([`LiveRelay::inject_ops`]); host frames always form their own groups.
//! Revocation drops the revoked session's view (it receives nothing more),
//! aborts only its own egress broadcast (other subscribers see stream resets,
//! no session close) and tells the revoked session to close.
use crate::carrier;
pub use crate::relay::{AuthorizationStatus, Denied, GOAWAY_GRACE};
use crate::session_authority::{AuthorityDriver, Verdict};
use std::collections::HashMap;
use std::sync::{
    Arc, Mutex, PoisonError, Weak,
    atomic::{AtomicU64, Ordering},
};
use std::time::{Duration, Instant};

/// Exact WebTransport path of the live relay on the host's QUIC endpoint.
pub const LIVE_PATH: &str = "/moq/live";
/// WebTransport subprotocol; identical to voice.
pub const LIVE_PROTOCOL: &str = carrier::PROTOCOL;
/// First namespace tuple component of every live track.
pub const NAMESPACE_ROOT: &str = "tilo-live";
/// Latest-value datagram track.
pub const PRESENCE_TRACK: &str = "presence";
/// Ordered per-record stream track prefix on the egress broadcast, and the
/// path segment of a client's record broadcasts below its namespace.
pub const OPS_PREFIX: &str = "ops/";
/// The one track of a client's record broadcast.
pub const RECORD_TRACK: &str = "ops";
/// Grant ceiling (an encoded live grant).
pub const MAX_TOKEN_BYTES: usize = 2048;
/// Per-publisher presence ceiling (frames/s).
pub const MAX_PRESENCE_RATE: u32 = 120;
/// Per-publisher ordered frame ceiling (frames/s).
pub const MAX_OPS_RATE: u32 = 240;
/// Records one publisher may have open at once.
pub const MAX_OPEN_RECORDS: usize = 16;
/// Presence frame ceiling (one datagram).
pub const MAX_DATAGRAM_FRAME_BYTES: usize = 1100;
/// Ordered frame ceiling.
pub const MAX_STREAM_FRAME_BYTES: usize = 64 * 1024;
/// Concurrently admitted live sessions.
pub const MAX_LIVE_SESSIONS: usize = 32;
/// Egress ops tracks one namespace may carry (subscriber demand bound).
pub const MAX_EGRESS_OPS_TRACKS: usize = 64;
/// Upstream groups copied concurrently per ops track.
const MAX_GROUPS_IN_FLIGHT: usize = 4;
/// A namespace fed only by host injection is withdrawn after this idle time.
pub const HOST_SLOT_IDLE: Duration = Duration::from_secs(30);
/// In-memory reorder window for ops groups; not storage.
const OPS_WINDOW: Duration = Duration::from_secs(2);
/// Egress retention of a superseded (non-latest) group: long enough to finish
/// serving it to current subscribers, short enough that a late subscriber is
/// not replayed it once the track was sealed.
const EGRESS_RETAIN: Duration = OPS_WINDOW;
/// Presence retention: latest value only.
const PRESENCE_RETAIN: Duration = Duration::from_millis(100);
/// Upper bound on bytes one publisher session may have buffered at ingress.
const INGRESS_POOL_BYTES: u64 = 512 * 1024;
/// Upper bound on bytes buffered at egress across all namespaces.
const EGRESS_POOL_BYTES: u64 = 16 * 1024 * 1024;
/// Upper bound on bytes one subscriber's own view may buffer.
const VIEW_POOL_BYTES: u64 = 1024 * 1024;
/// Delay before a revoked, host-fed namespace is re-announced.
const REANNOUNCE_AFTER: Duration = Duration::from_millis(100);
/// A quiet ops track is sealed with an empty group after this long, so a late
/// subscriber (which starts at the latest group) is not replayed stale frames.
pub const SEAL_AFTER: Duration = Duration::from_millis(250);
/// Quiet time after which a late subscriber is no longer replayed old groups.
pub const SEAL_QUIET: Duration = Duration::from_millis(250 + 2000 + 100);
const PRESENCE_RETRY: Duration = Duration::from_millis(100);
/// An ops track without subscribers for this long is withdrawn.
pub(crate) const UNUSED_GRACE: Duration = Duration::from_secs(2);

/// Host-internal marker for an exact raw route comparison; not authority.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LiveRoute(());

/// One admitted live session, privately constructed by the host authority.
/// `room` and `publisher` are 64-character lowercase hex components.
pub trait LiveSession: Send + Sync {
    /// Room digest component.
    fn room(&self) -> &str;
    /// Own publisher (device) component.
    fn publisher(&self) -> &str;
    /// Grant allows publishing into the own namespace.
    fn publish(&self) -> bool;
    /// Grant allows subscribing to the room.
    fn subscribe(&self) -> bool;
    /// Current lease, signature binding, host boot and membership. An
    /// authority that cannot observe them answers `Denied`.
    fn status(&self) -> AuthorizationStatus;
}

/// Host-injected admission and frame validation. Called without awaits; must
/// not block on network I/O.
pub trait LiveAuthority: Send + Sync {
    /// Verify the grant.
    ///
    /// # Errors
    ///
    /// Returns [`Denied`] when the grant does not authorize a session.
    fn try_admit(&self, token: &[u8]) -> Result<Arc<dyn LiveSession>, Denied>;
    /// Validate one presence frame; returns its per-publisher sequence.
    fn presence_seq(&self, frame: &[u8]) -> Option<u64>;
    /// Validate one ops frame for the track's record digest (64 hex).
    fn ops_valid(&self, record: &str, frame: &[u8]) -> bool;
}

#[derive(Default)]
struct Counters {
    accepted: AtomicU64,
    rejected: AtomicU64,
    active: AtomicU64,
    revoked: AtomicU64,
    presence_forwarded: AtomicU64,
    presence_dropped: AtomicU64,
    ops_forwarded: AtomicU64,
    ops_dropped: AtomicU64,
    injected: AtomicU64,
    injected_dropped: AtomicU64,
    records_refused: AtomicU64,
}

/// Content-free live relay counters.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LiveCounters {
    pub accepted: u64,
    pub rejected: u64,
    pub active: u64,
    pub revoked: u64,
    pub presence_forwarded: u64,
    pub presence_dropped: u64,
    pub ops_forwarded: u64,
    pub ops_dropped: u64,
    pub injected: u64,
    pub injected_dropped: u64,
    pub records_refused: u64,
}

/// Token bucket: sustained `rate`/s with a quarter-second burst.
struct Bucket {
    tokens: f64,
    last: Instant,
    rate: f64,
    burst: f64,
}
impl Bucket {
    fn new(rate: u32) -> Self {
        let rate = f64::from(rate);
        let burst = (rate / 4.0).max(1.0);
        Self {
            tokens: burst,
            last: Instant::now(),
            rate,
            burst,
        }
    }
    fn take(&mut self, now: Instant) -> bool {
        let elapsed = now.saturating_duration_since(self.last).as_secs_f64();
        self.last = now;
        self.tokens = (self.tokens + elapsed * self.rate).min(self.burst);
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }
}

use crate::Task;

struct OpsEntry {
    producer: moq_net::track::Producer,
    /// The group a host stream is writing ([`LiveRelay::inject_stream`]).
    open: Option<moq_net::group::Producer>,
    _watch: Task,
}

/// The client currently owning a namespace: its session and the private
/// ingress origin its broadcasts are announced in.
struct Client {
    session: u64,
    /// Watches the ingress announcements and runs the presence pump and one
    /// pump per open record.
    _watch: Task,
}

struct SlotState {
    broadcast: Option<moq_net::broadcast::Producer>,
    presence: Option<moq_net::track::Producer>,
    ops: HashMap<String, OpsEntry>,
    client: Option<Client>,
    presence_bucket: Bucket,
    ops_bucket: Bucket,
    last_seq: Option<u64>,
    host_active: Option<Instant>,
    handler: Option<Task>,
}

struct Slot {
    state: Mutex<SlotState>,
}

struct Inner {
    authority: Arc<dyn LiveAuthority>,
    egress: moq_net::origin::Producer,
    _egress: carrier::OriginTask,
    slots: Mutex<HashMap<String, Arc<Slot>>>,
    sessions: Arc<tokio::sync::Semaphore>,
    next_session: AtomicU64,
    counters: Counters,
    /// HTTPS fallback carrier sessions ([`fallback`]).
    fallback: fallback::Registry,
    /// See [`LiveRelay::set_presence_tap`].
    presence_tap: std::sync::OnceLock<PresenceTap>,
}

/// Sees every presence frame a client session publishes and the relay
/// forwards: room, publisher (both 64 hex) and the frame. Host-injected
/// frames never reach it, so a host bridging frames both ways cannot loop.
pub type PresenceTap = Arc<dyn Fn(&str, &str, &[u8]) + Send + Sync>;

/// The live relay. Cheap to clone; all clones share one egress origin.
#[derive(Clone)]
pub struct LiveRelay(Arc<Inner>);

/// A publisher's private ingress origin.
struct Ingress {
    origin: moq_net::origin::Producer,
    task: carrier::OriginTask,
}

struct Scopes {
    session: Arc<dyn LiveSession>,
    authority: AuthorityDriver,
    path: String,
    ingress: Option<Ingress>,
    egress: Option<moq_net::origin::Consumer>,
    /// The session's own view of `egress`, the only thing it is served from.
    view: Option<crate::mirror::Mirror>,
}

fn hex64(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Exact egress path of one namespace.
pub fn namespace_path(room: &str, publisher: &str) -> Option<String> {
    (hex64(room) && hex64(publisher)).then(|| format!("{NAMESPACE_ROOT}/{room}/{publisher}"))
}

/// Record digest of an `ops/<hex>` track name.
pub fn ops_record(track: &str) -> Option<&str> {
    track
        .strip_prefix(OPS_PREFIX)
        .filter(|record| hex64(record))
}

/// The relay's presentation clock for egress ops frames: monotonic
/// milliseconds since the process first stamped a frame. Retention and late
/// joins are measured on it (see [`seal_later`]).
fn stamp() -> moq_net::Timestamp {
    static EPOCH: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    let elapsed = EPOCH.get_or_init(Instant::now).elapsed().as_millis();
    moq_net::Timestamp::from_millis(u64::try_from(elapsed).unwrap_or(u64::MAX))
        .unwrap_or(moq_net::Timestamp::ZERO)
}

/// Path of the broadcast a client announces for one record it writes.
pub fn record_path(namespace: &str, record: &str) -> Option<String> {
    hex64(record).then(|| format!("{namespace}/{OPS_PREFIX}{record}"))
}

/// A byte-bounded origin for one publisher's ingress.
fn ingress_origin() -> Ingress {
    let (origin, task) =
        carrier::OriginTask::produce(carrier::origin_config(INGRESS_POOL_BYTES, OPS_WINDOW));
    Ingress { origin, task }
}

/// The namespace and every path below it.
fn subtree(path: &str) -> Option<moq_net::Patterns> {
    moq_net::Pattern::subtree(path)
        .ok()
        .map(moq_net::Patterns::from)
}

impl LiveRelay {
    /// Must be called inside a Tokio runtime: the egress origin's driver runs
    /// on it for as long as any clone of the relay lives.
    pub fn new(authority: Arc<dyn LiveAuthority>) -> Self {
        let (egress, task) =
            carrier::OriginTask::produce(carrier::origin_config(EGRESS_POOL_BYTES, EGRESS_RETAIN));
        Self(Arc::new(Inner {
            authority,
            egress,
            _egress: task,
            slots: Mutex::default(),
            sessions: Arc::new(tokio::sync::Semaphore::new(MAX_LIVE_SESSIONS)),
            next_session: AtomicU64::new(1),
            counters: Counters::default(),
            fallback: fallback::Registry::default(),
            presence_tap: std::sync::OnceLock::new(),
        }))
    }

    /// Install the one [`PresenceTap`]; later calls are ignored.
    pub fn set_presence_tap(&self, tap: PresenceTap) {
        let _ = self.0.presence_tap.set(tap);
    }

    pub fn counters(&self) -> LiveCounters {
        let c = &self.0.counters;
        let load = |value: &AtomicU64| value.load(Ordering::Relaxed);
        LiveCounters {
            accepted: load(&c.accepted),
            rejected: load(&c.rejected),
            active: load(&c.active),
            revoked: load(&c.revoked),
            presence_forwarded: load(&c.presence_forwarded),
            presence_dropped: load(&c.presence_dropped),
            ops_forwarded: load(&c.ops_forwarded),
            ops_dropped: load(&c.ops_dropped),
            injected: load(&c.injected),
            injected_dropped: load(&c.injected_dropped),
            records_refused: load(&c.records_refused),
        }
    }

    /// Exact raw CONNECT route check, before URL normalization.
    pub fn validate_route(
        raw_authority: &str,
        expected_authority: &str,
        raw_target: &str,
    ) -> Option<LiveRoute> {
        (raw_authority == expected_authority && raw_target == LIVE_PATH).then_some(LiveRoute(()))
    }

    /// Host injection of one already encoded ops frame (for example a forwarded
    /// signed change) into `ops/<record>` of `(room, publisher)`. Never blocks.
    /// Creates a host-fed namespace if absent; the frame reaches only current
    /// subscribers of that track (dropped otherwise: nothing is stored).
    #[allow(
        clippy::significant_drop_tightening,
        reason = "the guard must stay held across the dependent state updates below"
    )]
    pub fn inject_ops(&self, room: &str, publisher: &str, record: &str, frame: &[u8]) -> bool {
        let inner = &self.0;
        let Some(path) = namespace_path(room, publisher) else {
            return false;
        };
        if !hex64(record) || frame.is_empty() || frame.len() > MAX_STREAM_FRAME_BYTES {
            inner
                .counters
                .injected_dropped
                .fetch_add(1, Ordering::Relaxed);
            return false;
        }
        let Some(slot) = inner.slot(&path, true) else {
            inner
                .counters
                .injected_dropped
                .fetch_add(1, Ordering::Relaxed);
            return false;
        };
        let mut state = slot.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.host_active = Some(Instant::now());
        let name = format!("{OPS_PREFIX}{record}");
        let Some(entry) = state.ops.get_mut(&name) else {
            inner
                .counters
                .injected_dropped
                .fetch_add(1, Ordering::Relaxed);
            return false;
        };
        let written = entry.producer.append_group().and_then(|mut group| {
            group.write_frame(stamp(), bytes::Bytes::copy_from_slice(frame))?;
            group.finish()?;
            Ok(group.sequence)
        });
        if let Ok(sequence) = written {
            seal_later(entry.producer.clone(), sequence);
        }
        let counter = if written.is_ok() {
            &inner.counters.injected
        } else {
            &inner.counters.injected_dropped
        };
        counter.fetch_add(1, Ordering::Relaxed);
        written.is_ok()
    }

    /// Host injection of a continuous ordered stream (for example a video
    /// window) into `ops/<record>` of `(room, publisher)`. `start` finishes
    /// the open group and opens a new one, so a subscriber that starts at the
    /// latest group receives it from its first frame; otherwise `frames` join
    /// the open group, and are dropped while none is open. Never blocks;
    /// frames reach only current subscribers of that track.
    #[allow(
        clippy::significant_drop_tightening,
        reason = "the guard must stay held across the dependent state updates below"
    )]
    pub fn inject_stream(
        &self,
        room: &str,
        publisher: &str,
        record: &str,
        start: bool,
        frames: &[bytes::Bytes],
    ) -> bool {
        let inner = &self.0;
        let dropped = || {
            inner
                .counters
                .injected_dropped
                .fetch_add(1, Ordering::Relaxed);
            false
        };
        let Some(path) = namespace_path(room, publisher) else {
            return dropped();
        };
        if !hex64(record)
            || frames.is_empty()
            || frames
                .iter()
                .any(|frame| frame.is_empty() || frame.len() > MAX_STREAM_FRAME_BYTES)
        {
            return dropped();
        }
        let Some(slot) = inner.slot(&path, true) else {
            return dropped();
        };
        let mut state = slot.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.host_active = Some(Instant::now());
        let Some(entry) = state.ops.get_mut(&format!("{OPS_PREFIX}{record}")) else {
            return dropped();
        };
        if start {
            if let Some(open) = entry.open.take() {
                let _ = open.finish();
            }
            entry.open = entry.producer.append_group().ok();
        }
        let Some(group) = entry.open.as_mut() else {
            return dropped();
        };
        for frame in frames {
            if group.write_frame(stamp(), frame.clone()).is_err() {
                entry.open = None;
                return dropped();
            }
        }
        inner.counters.injected.fetch_add(1, Ordering::Relaxed);
        true
    }

    /// Whether a host stream on `ops/<record>` of `(room, publisher)` still
    /// has its group open: false once its last subscriber left and the track
    /// was withdrawn, so the next sequence must start a new group.
    pub fn stream_open(&self, room: &str, publisher: &str, record: &str) -> bool {
        let Some(slot) = namespace_path(room, publisher).and_then(|path| self.0.slot(&path, false))
        else {
            return false;
        };
        slot.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .ops
            .get(&format!("{OPS_PREFIX}{record}"))
            .is_some_and(|entry| entry.open.is_some())
    }

    /// End a host stream on `ops/<record>` of `(room, publisher)`: its open
    /// group finishes and the track is sealed like a quiet ops track.
    #[allow(
        clippy::significant_drop_tightening,
        reason = "the guard must stay held across the dependent state updates below"
    )]
    pub fn end_stream(&self, room: &str, publisher: &str, record: &str) {
        let Some(path) = namespace_path(room, publisher) else {
            return;
        };
        let Some(slot) = self.0.slot(&path, false) else {
            return;
        };
        let mut state = slot.state.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(entry) = state.ops.get_mut(&format!("{OPS_PREFIX}{record}")) else {
            return;
        };
        if let Some(open) = entry.open.take()
            && open.finish().is_ok()
        {
            seal_later(entry.producer.clone(), open.sequence);
        }
    }

    /// Host injection of one presence frame into the `presence` datagram track
    /// of a host-fed namespace `(room, publisher)` (for example an agent the
    /// host speaks for). Never blocks. Creates the host-fed namespace if absent.
    /// Refused while a client session owns the namespace: the host never
    /// speaks inside a live member's own presence. The frame is validated like
    /// client presence (authority codec, size, strictly increasing sequence,
    /// the namespace's presence rate bucket) and reaches `MoQ` and HTTPS
    /// fallback subscribers through the same egress track. Counted in
    /// `injected` / `injected_dropped`.
    pub fn inject_presence(&self, room: &str, publisher: &str, frame: &[u8]) -> bool {
        let inner = &self.0;
        let dropped = || {
            inner
                .counters
                .injected_dropped
                .fetch_add(1, Ordering::Relaxed);
            false
        };
        let Some(path) = namespace_path(room, publisher) else {
            return dropped();
        };
        if frame.is_empty() || frame.len() > MAX_DATAGRAM_FRAME_BYTES {
            return dropped();
        }
        let Some(seq) = inner.authority.presence_seq(frame) else {
            return dropped();
        };
        // Never create (or announce) a namespace a client currently owns.
        if let Some(slot) = inner.slot(&path, false)
            && slot
                .state
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .client
                .is_some()
        {
            return dropped();
        }
        let Some(slot) = inner.slot(&path, true) else {
            return dropped();
        };
        let mut state = slot.state.lock().unwrap_or_else(PoisonError::into_inner);
        if state.client.is_some() {
            return dropped();
        }
        state.host_active = Some(Instant::now());
        if state.last_seq.is_some_and(|last| seq <= last)
            || !state.presence_bucket.take(Instant::now())
        {
            return dropped();
        }
        let Some(track) = state.presence.as_mut() else {
            return dropped();
        };
        if track
            .append_datagram(
                moq_net::Timestamp::ZERO,
                bytes::Bytes::copy_from_slice(frame),
            )
            .is_err()
        {
            return dropped();
        }
        state.last_seq = Some(seq);
        drop(state);
        inner.counters.injected.fetch_add(1, Ordering::Relaxed);
        true
    }

    /// Withdraw host-fed namespaces idle past [`HOST_SLOT_IDLE`] with no client.
    pub fn maintain(&self) {
        let now = Instant::now();
        let idle: Vec<String> = {
            let slots = self.0.slots.lock().unwrap_or_else(PoisonError::into_inner);
            slots
                .iter()
                .filter(|(_, slot)| {
                    let state = slot.state.lock().unwrap_or_else(PoisonError::into_inner);
                    state.client.is_none()
                        && state
                            .host_active
                            .is_none_or(|at| now.duration_since(at) >= HOST_SLOT_IDLE)
                })
                .map(|(path, _)| path.clone())
                .collect()
        };
        for path in idle {
            let _ = self.0.remove_slot(&path, None);
        }
    }

    /// Standalone listener for [`LIVE_PATH`] (tests and isolated proofs).
    pub async fn serve(self, endpoint: quinn::Endpoint) {
        self.serve_routes(endpoint, None).await;
    }

    /// Listener for [`LIVE_PATH`] and, on the same endpoint, `voice`'s
    /// [`VOICE_PATH`](crate::relay::VOICE_PATH).
    pub async fn serve_with_voice(self, endpoint: quinn::Endpoint, voice: crate::relay::Relay) {
        self.serve_routes(endpoint, Some(voice)).await;
    }

    async fn serve_routes(self, endpoint: quinn::Endpoint, voice: Option<crate::relay::Relay>) {
        const LIVE: &[&str] = &[LIVE_PATH];
        const BOTH: &[&str] = &[LIVE_PATH, crate::relay::VOICE_PATH];
        let relay = self.clone();
        let paths = if voice.is_some() { BOTH } else { LIVE };
        let serving = carrier::serve_endpoint(endpoint, paths, move |path, session| {
            let relay = relay.clone();
            let voice = voice.clone();
            async move {
                match voice {
                    Some(voice) if path == crate::relay::VOICE_PATH => {
                        voice
                            .serve_session(crate::relay::VoiceRoute::routed(), session)
                            .await;
                    }
                    _ => relay.serve_session(LiveRoute(()), session).await,
                }
            }
        });
        tokio::pin!(serving);
        let mut maintenance = tokio::time::interval(Duration::from_secs(1));
        loop {
            tokio::select! {
                () = &mut serving => break,
                _ = maintenance.tick() => self.maintain(),
            }
        }
    }

    /// Serve one WebTransport session answered with [`carrier::PROTOCOL`] on
    /// an exactly validated route. The driver stays in this future.
    pub async fn serve_session(&self, _route: LiveRoute, transport: web_transport_quinn::Session) {
        let Ok(_permit) = self.0.sessions.clone().try_acquire_owned() else {
            self.0.counters.rejected.fetch_add(1, Ordering::Relaxed);
            transport.close(carrier::CLOSE_DENIED, b"relay busy");
            return;
        };
        let accepted = carrier::accept(transport, |token| async move {
            let mut scopes = self.scopes(&token)?;
            let mut server = moq_net::Server::new();
            if let Some(ingress) = &scopes.ingress {
                let own = ingress.origin.scope("", &subtree(&scopes.path)?).ok()?;
                server = server.with_subscriber(own);
            }
            if let Some(egress) = &scopes.egress {
                let view =
                    crate::mirror::Mirror::start(egress.clone(), VIEW_POOL_BYTES, EGRESS_RETAIN);
                server = server.with_publisher(view.consume());
                scopes.view = Some(view);
            }
            Some((server, scopes))
        })
        .await;
        let Ok((accepted, scopes)) = accepted else {
            self.0.counters.rejected.fetch_add(1, Ordering::Relaxed);
            return;
        };
        self.run_session(accepted, scopes).await;
    }

    fn scopes(&self, token: &[u8]) -> Option<Scopes> {
        if token.is_empty() || token.len() > MAX_TOKEN_BYTES {
            return None;
        }
        let session = self.0.authority.try_admit(token).ok()?;
        let path = namespace_path(session.room(), session.publisher())?;
        if !session.publish() && !session.subscribe() {
            return None;
        }
        let authority = AuthorityDriver::admit(session.status())?;
        let ingress = session.publish().then(ingress_origin);
        let egress = if session.subscribe() {
            let room = format!("{NAMESPACE_ROOT}/{}", session.room());
            Some(self.0.egress.consume().scope("", &subtree(&room)?).ok()?)
        } else {
            None
        };
        Some(Scopes {
            session,
            authority,
            path,
            ingress,
            egress,
            view: None,
        })
    }

    async fn run_session(&self, accepted: carrier::Accepted, mut scopes: Scopes) {
        struct Active<'a>(&'a Counters);
        impl Drop for Active<'_> {
            fn drop(&mut self) {
                self.0.active.fetch_sub(1, Ordering::Relaxed);
            }
        }
        let carrier::Accepted {
            session,
            driver,
            mut admission,
            transport,
        } = accepted;
        let inner = &self.0;
        let id = inner.next_session.fetch_add(1, Ordering::Relaxed);
        inner.counters.accepted.fetch_add(1, Ordering::Relaxed);
        inner.counters.active.fetch_add(1, Ordering::Relaxed);
        let _active = Active(&inner.counters);
        // Attach the client's ingress: its namespace and record broadcasts.
        let _ingress_task = scopes.ingress.map(|ingress| {
            inner.attach_client(&scopes.path, id, ingress.origin.consume());
            ingress.task
        });
        let driver = moq_net::time::run(driver);
        tokio::pin!(driver);
        let revoked = loop {
            tokio::select! {
                _ = session.closed() => break false,
                _ = &mut driver => break false,
                verdict = scopes.authority.next(|| scopes.session.status()) => match verdict {
                    Verdict::Continue => {}
                    Verdict::Revoked => break true,
                },
            }
        };
        if revoked {
            inner.counters.revoked.fetch_add(1, Ordering::Relaxed);
            // The revoked member receives nothing more: its own view (every
            // track it is served) goes first.
            scopes.view.take();
            // Only this publisher's tracks end (subscriber stream resets).
            let host_fed = inner.remove_slot(&scopes.path, Some(id));
            // A namespace the host still feeds is re-announced at once as a
            // host-fed namespace, so subscribers can resubscribe for the
            // host's forwarded changes. The re-announce waits briefly so the
            // withdrawal reaches subscribers as a distinct close first.
            if host_fed {
                let relay = Arc::downgrade(&self.0);
                let path = scopes.path.clone();
                tokio::spawn(async move {
                    tokio::time::sleep(REANNOUNCE_AFTER).await;
                    if let Some(inner) = relay.upgrade()
                        && let Some(slot) = inner.slot(&path, true)
                    {
                        let mut state = slot.state.lock().unwrap_or_else(PoisonError::into_inner);
                        state.host_active.get_or_insert_with(Instant::now);
                    }
                });
            }
            // It is told to close first; the relay closes after the grace at
            // the latest. The grace only covers the transport teardown.
            admission.revoke().await;
            tokio::select! {
                _ = session.closed() => {}
                _ = &mut driver => {}
                () = tokio::time::sleep(GOAWAY_GRACE) => {}
            }
            session.abort(moq_net::Error::Unauthorized);
            transport.close(carrier::CLOSE_REVOKED, b"authority lapsed");
        } else {
            inner.detach_client(&scopes.path, id);
            session.abort(moq_net::Error::Cancel);
        }
    }
}

impl Inner {
    /// The namespace slot, created with its egress broadcast when `create`.
    fn slot(self: &Arc<Self>, path: &str, create: bool) -> Option<Arc<Slot>> {
        let mut slots = self.slots.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(slot) = slots.get(path) {
            return Some(slot.clone());
        }
        if !create {
            return None;
        }
        let broadcast = self.egress.create_broadcast(path).ok()?;
        let presence = broadcast
            .create_track(
                PRESENCE_TRACK,
                moq_net::track::Info::default().with_max_age(PRESENCE_RETAIN),
            )
            .ok()?;
        // The handler exists before the announcement, so the first subscriber
        // finds it.
        let dynamic = broadcast.dynamic();
        broadcast.announce(moq_net::origin::Route::default()).ok()?;
        let slot = Arc::new(Slot {
            state: Mutex::new(SlotState {
                broadcast: Some(broadcast),
                presence: Some(presence),
                ops: HashMap::new(),
                client: None,
                presence_bucket: Bucket::new(MAX_PRESENCE_RATE),
                ops_bucket: Bucket::new(MAX_OPS_RATE),
                last_seq: None,
                host_active: None,
                handler: None,
            }),
        });
        let handler = {
            let inner = Arc::downgrade(self);
            let weak = Arc::downgrade(&slot);
            let mut dynamic = dynamic;
            Task::spawn(async move {
                while let Ok(request) = dynamic.requested_track().await {
                    let (Some(inner), Some(slot)) = (inner.upgrade(), weak.upgrade()) else {
                        break;
                    };
                    inner.on_request(&slot, request);
                }
            })
        };
        slot.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .handler = Some(handler);
        slots.insert(path.to_owned(), slot.clone());
        drop(slots);
        Some(slot)
    }

    /// A subscriber asked for a track this namespace does not yet carry.
    #[allow(
        clippy::significant_drop_tightening,
        reason = "the guard must stay held across the dependent state updates below"
    )]
    fn on_request(&self, slot: &Arc<Slot>, request: moq_net::track::Request) {
        let name = request.name().to_owned();
        let mut state = slot.state.lock().unwrap_or_else(PoisonError::into_inner);
        if ops_record(&name).is_none()
            || state.ops.len() >= MAX_EGRESS_OPS_TRACKS
            || state.broadcast.is_none()
        {
            self.counters
                .records_refused
                .fetch_add(1, Ordering::Relaxed);
            request.reject(moq_net::Error::NotFound);
            return;
        }
        let producer = request.accept(moq_net::track::Info::default().with_max_age(EGRESS_RETAIN));
        // Demand ends: withdraw the track so nothing lingers without a reader.
        let watch = {
            let weak = Arc::downgrade(slot);
            let watched = producer.clone();
            let name = name.clone();
            Task::spawn(async move {
                // Consumer handles can be swapped transiently during SUBSCRIBE
                // handling: withdraw only after a continuous unused grace.
                loop {
                    if watched.used().await.is_err() || watched.unused().await.is_err() {
                        return;
                    }
                    tokio::time::sleep(UNUSED_GRACE).await;
                    if !watched.is_used() {
                        break;
                    }
                }
                if let Some(slot) = weak.upgrade() {
                    let removed = slot
                        .state
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .ops
                        .remove(&name);
                    if let Some(entry) = removed {
                        let OpsEntry {
                            producer,
                            _watch: watch,
                            ..
                        } = entry;
                        let _ = producer.abort(moq_net::Error::Cancel);
                        // This is the watch task itself, finishing now.
                        watch.detach();
                    }
                }
            })
        };
        state.ops.insert(
            name,
            OpsEntry {
                producer,
                open: None,
                _watch: watch,
            },
        );
    }

    /// Newest session wins the namespace: a reload replaces its older session.
    fn attach_client(
        self: &Arc<Self>,
        path: &str,
        session: u64,
        ingress: moq_net::origin::Consumer,
    ) {
        let Some(slot) = self.slot(path, true) else {
            return;
        };
        // A spawned watcher can run immediately on another worker. Publish
        // its ownership while holding the lock that its first check takes.
        let mut state = slot.state.lock().unwrap_or_else(PoisonError::into_inner);
        let watch = Task::spawn(watch_client(
            Arc::downgrade(self),
            Arc::downgrade(&slot),
            session,
            path.to_owned(),
            ingress,
        ));
        state.client = Some(Client {
            session,
            _watch: watch,
        });
    }

    fn detach_client(self: &Arc<Self>, path: &str, session: u64) {
        let Some(slot) = self.slot(path, false) else {
            return;
        };
        let (withdraw, client) = {
            let mut state = slot.state.lock().unwrap_or_else(PoisonError::into_inner);
            if state
                .client
                .as_ref()
                .is_none_or(|client| client.session != session)
            {
                return;
            }
            let client = state.client.take();
            let withdraw = state
                .host_active
                .is_none_or(|at| at.elapsed() >= HOST_SLOT_IDLE);
            drop(state);
            (withdraw, client)
        };
        drop(client);
        if withdraw {
            let _ = self.remove_slot(path, Some(session));
        }
    }

    /// Remove a namespace. With `session`, only if that session still owns it
    /// (or no client does). Revocation aborts the tracks: subscribers see
    /// their track streams reset, their sessions stay open.
    /// Returns whether the removed namespace was recently host-fed.
    fn remove_slot(&self, path: &str, session: Option<u64>) -> bool {
        let slot = {
            let mut slots = self.slots.lock().unwrap_or_else(PoisonError::into_inner);
            let Some(slot) = slots.get(path) else {
                return false;
            };
            let state = slot.state.lock().unwrap_or_else(PoisonError::into_inner);
            if let Some(session) = session {
                if state
                    .client
                    .as_ref()
                    .is_some_and(|client| client.session != session)
                {
                    return false;
                }
            } else if Arc::strong_count(slot) > 1
                || state.client.is_some()
                || state
                    .host_active
                    .is_some_and(|at| at.elapsed() < HOST_SLOT_IDLE)
            {
                // Recheck the idle candidate under both locks. slot() takes
                // the map lock to acquire a reference; its caller retains
                // that reference until attaching or updating host activity.
                // A stale snapshot cannot collect that in-flight operation.
                return false;
            }
            drop(state);
            slots.remove(path)
        };
        let Some(slot) = slot else { return false };
        let (client, handler, ops, presence, broadcast, host_fed) = {
            let mut state = slot.state.lock().unwrap_or_else(PoisonError::into_inner);
            let host_fed = state
                .host_active
                .is_some_and(|at| at.elapsed() < HOST_SLOT_IDLE);
            (
                state.client.take(),
                state.handler.take(),
                std::mem::take(&mut state.ops),
                state.presence.take(),
                state.broadcast.take(),
                host_fed,
            )
        };
        drop(client);
        for (_, entry) in ops {
            let OpsEntry {
                producer, _watch, ..
            } = entry;
            let _ = producer.abort(moq_net::Error::Unauthorized);
        }
        if let Some(presence) = presence {
            let _ = presence.abort(moq_net::Error::Unauthorized);
        }
        // Tracks were reset above (subscriber stream resets); the namespace
        // itself ends with a deliberate finish, which every hop forwards as a
        // clean withdrawal and which closes subscribers' broadcast handles. A
        // later re-announce (host-fed replacement) is a new, separate
        // announcement.
        if let Some(broadcast) = broadcast {
            broadcast.finish();
        }
        drop(handler);
        host_fed
    }

    /// Whether `session` still owns the namespace in `slot`.
    fn owns(slot: &Slot, session: u64) -> bool {
        slot.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .client
            .as_ref()
            .is_some_and(|client| client.session == session)
    }

    /// Validate and forward one presence frame. Latest-value: a sequence at or
    /// below the last forwarded one is stale and dropped.
    fn forward_presence(
        &self,
        slot: &Slot,
        session: u64,
        path: &str,
        payload: bytes::Bytes,
    ) -> bool {
        let valid = payload.len() <= MAX_DATAGRAM_FRAME_BYTES;
        let seq = valid
            .then(|| self.authority.presence_seq(&payload))
            .flatten();
        let mut state = slot.state.lock().unwrap_or_else(PoisonError::into_inner);
        if state
            .client
            .as_ref()
            .is_none_or(|client| client.session != session)
        {
            return false;
        }
        let now = Instant::now();
        let forward = seq.is_some_and(|seq| state.last_seq.is_none_or(|last| seq > last))
            && state.presence_bucket.take(now);
        if forward {
            state.last_seq = seq;
            let tapped = self.presence_tap.get().map(|tap| (tap, payload.clone()));
            if let Some(track) = state.presence.as_mut()
                && track
                    .append_datagram(moq_net::Timestamp::ZERO, payload)
                    .is_ok()
            {
                drop(state);
                self.counters
                    .presence_forwarded
                    .fetch_add(1, Ordering::Relaxed);
                if let Some((tap, payload)) = tapped
                    && let Some((room, publisher)) = path
                        .strip_prefix(NAMESPACE_ROOT)
                        .and_then(|rest| rest.strip_prefix('/'))
                        .and_then(|rest| rest.split_once('/'))
                {
                    tap(room, publisher, &payload);
                }
                return true;
            }
        }
        self.counters
            .presence_dropped
            .fetch_add(1, Ordering::Relaxed);
        true
    }

    /// The egress track currently serving `name`, if a subscriber asked for it.
    fn egress_ops(slot: &Slot, name: &str) -> Option<moq_net::track::Producer> {
        slot.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .ops
            .get(name)
            .map(|entry| entry.producer.clone())
    }
}

/// Follow one client's ingress announcements: the namespace broadcast feeds
/// presence, each announced record broadcast (at most [`MAX_OPEN_RECORDS`])
/// feeds its egress `ops/<record>` track.
async fn watch_client(
    inner: Weak<Inner>,
    slot: Weak<Slot>,
    session: u64,
    path: String,
    ingress: moq_net::origin::Consumer,
) {
    let mut announced = ingress.announced();
    let mut presence: Option<Task> = None;
    let mut records: HashMap<String, Task> = HashMap::new();
    while let Some(update) = announced.next().await {
        let (Some(owner), Some(current)) = (inner.upgrade(), slot.upgrade()) else {
            return;
        };
        if !Inner::owns(&current, session) {
            return;
        }
        let prefix = update.prefix.as_str().to_owned();
        let active = update.kind.is_active();
        if prefix == path {
            presence = active.then(|| {
                Task::spawn(presence_pump(
                    inner.clone(),
                    slot.clone(),
                    session,
                    ingress.clone(),
                    path.clone(),
                ))
            });
            continue;
        }
        let Some(name) = prefix
            .strip_prefix(path.as_str())
            .and_then(|rest| rest.strip_prefix('/'))
            .filter(|name| ops_record(name).is_some())
            .map(str::to_owned)
        else {
            continue;
        };
        records.retain(|_, task| !task.is_finished());
        if !active {
            records.remove(&name);
            continue;
        }
        if records.contains_key(&name) {
            continue;
        }
        if records.len() >= MAX_OPEN_RECORDS {
            owner
                .counters
                .records_refused
                .fetch_add(1, Ordering::Relaxed);
            continue;
        }
        let pump = Task::spawn(ops_pump(
            inner.clone(),
            slot.clone(),
            ingress.clone(),
            prefix,
            name.clone(),
        ));
        records.insert(name, pump);
    }
    drop(presence);
}

async fn presence_pump(
    inner: Weak<Inner>,
    slot: Weak<Slot>,
    session: u64,
    ingress: moq_net::origin::Consumer,
    path: String,
) {
    let Ok(broadcast) = ingress.routed_broadcast(path.as_str()).await else {
        return;
    };
    let mut subscriber = loop {
        let subscription = moq_net::track::Subscription::default().with_max_age(PRESENCE_RETAIN);
        if let Ok(track) = broadcast.track(PRESENCE_TRACK)
            && let Ok(subscriber) = track.subscribe(subscription).await
        {
            break subscriber;
        }
        if broadcast.is_closed() {
            return;
        }
        tokio::time::sleep(PRESENCE_RETRY).await;
    };
    loop {
        let Some(first) = next(&mut subscriber).await else {
            return;
        };
        let Some(owner) = inner.upgrade() else { return };
        // Coalesce whatever is already queued to the newest valid frame; an
        // invalid frame never masks a valid one.
        let mut latest = None;
        let mut offer = |payload: bytes::Bytes| {
            if payload.len() <= MAX_DATAGRAM_FRAME_BYTES
                && owner.authority.presence_seq(&payload).is_some()
            {
                if latest.replace(payload).is_some() {
                    owner
                        .counters
                        .presence_dropped
                        .fetch_add(1, Ordering::Relaxed);
                }
            } else {
                owner
                    .counters
                    .presence_dropped
                    .fetch_add(1, Ordering::Relaxed);
            }
        };
        offer(first);
        while let Some(payload) = next_ready(&mut subscriber) {
            offer(payload);
        }
        let Some(payload) = latest else { continue };
        let Some(slot) = slot.upgrade() else { return };
        if !owner.forward_presence(&slot, session, &path, payload) {
            return;
        }
    }
}

/// The next presence frame: a datagram or the first frame of a single-frame
/// group. `None` when the track ended.
async fn next(track: &mut moq_net::track::Subscriber) -> Option<bytes::Bytes> {
    loop {
        match poll_presence(track).await {
            Presence::Frame(payload) => return Some(payload),
            Presence::Group(mut group) => {
                if let Ok(Some(frame)) = group.read_frame().await {
                    return Some(frame.payload);
                }
            }
            Presence::End => return None,
        }
    }
}

/// A presence datagram that is already buffered, without waiting.
fn next_ready(track: &mut moq_net::track::Subscriber) -> Option<bytes::Bytes> {
    let waiter = moq_net::kio::Waiter::noop();
    match track.poll_recv_datagram(&waiter) {
        std::task::Poll::Ready(Ok(Some(datagram))) => Some(datagram.payload),
        _ => None,
    }
}

enum Presence {
    Frame(bytes::Bytes),
    Group(Box<moq_net::group::Consumer>),
    End,
}

async fn poll_presence(track: &mut moq_net::track::Subscriber) -> Presence {
    moq_net::kio::wait(|waiter| {
        if let std::task::Poll::Ready(datagram) = track.poll_recv_datagram(waiter) {
            return std::task::Poll::Ready(match datagram {
                Ok(Some(datagram)) => Presence::Frame(datagram.payload),
                _ => Presence::End,
            });
        }
        if let std::task::Poll::Ready(group) = track.poll_recv_group(waiter) {
            return std::task::Poll::Ready(match group {
                Ok(Some(group)) => Presence::Group(Box::new(group)),
                _ => Presence::End,
            });
        }
        std::task::Poll::Pending
    })
    .await
}

/// Copy one client record broadcast into the namespace's egress
/// `ops/<record>` track while a subscriber is asking for it.
async fn ops_pump(
    inner: Weak<Inner>,
    slot: Weak<Slot>,
    ingress: moq_net::origin::Consumer,
    path: String,
    name: String,
) {
    let Ok(broadcast) = ingress.routed_broadcast(path.as_str()).await else {
        return;
    };
    let subscription = moq_net::track::Subscription::default().with_max_age(OPS_WINDOW);
    let subscriber = match broadcast.track(RECORD_TRACK) {
        Ok(track) => track.subscribe(subscription).await.ok(),
        Err(_) => None,
    };
    let Some(mut subscriber) = subscriber else {
        return;
    };
    let record = ops_record(&name).unwrap_or_default().to_owned();
    let mut groups: std::collections::VecDeque<Task> = std::collections::VecDeque::new();
    while let Ok(Some(group)) = subscriber.recv_group().await {
        let Some(current) = slot.upgrade() else {
            return;
        };
        groups.retain(|task| !task.is_finished());
        let egress = Inner::egress_ops(&current, &name);
        let (Some(egress), true) = (egress, groups.len() < MAX_GROUPS_IN_FLIGHT) else {
            if let Some(owner) = inner.upgrade() {
                owner.counters.ops_dropped.fetch_add(1, Ordering::Relaxed);
            }
            continue;
        };
        let Ok(out) = egress.append_group() else {
            continue;
        };
        groups.push_back(Task::spawn(copy_group(
            inner.clone(),
            slot.clone(),
            record.clone(),
            group,
            out,
            egress,
        )));
    }
}

async fn copy_group(
    inner: Weak<Inner>,
    slot: Weak<Slot>,
    record: String,
    mut group: moq_net::group::Consumer,
    mut out: moq_net::group::Producer,
    egress: moq_net::track::Producer,
) {
    loop {
        let frame = match group.read_frame().await {
            Ok(Some(frame)) => frame,
            Ok(None) => {
                if out.finish().is_ok() {
                    seal_later(egress, out.sequence);
                }
                return;
            }
            Err(_) => {
                let _ = out.abort(moq_net::Error::Cancel);
                return;
            }
        };
        let (Some(inner), Some(slot)) = (inner.upgrade(), slot.upgrade()) else {
            let _ = out.abort(moq_net::Error::Cancel);
            return;
        };
        let admitted = frame.payload.len() <= MAX_STREAM_FRAME_BYTES
            && inner.authority.ops_valid(&record, &frame.payload)
            && slot
                .state
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .ops_bucket
                .take(Instant::now());
        if !admitted || out.write_frame(stamp(), frame.payload).is_err() {
            // An ordered group with a gap is useless: reset it so receivers know.
            inner.counters.ops_dropped.fetch_add(1, Ordering::Relaxed);
            let _ = out.abort(moq_net::Error::ProtocolViolation);
            return;
        }
        inner.counters.ops_forwarded.fetch_add(1, Ordering::Relaxed);
    }
}

/// After [`SEAL_AFTER`] of quiet, append a seal group so the last delivered
/// group is no longer the latest; after [`EGRESS_RETAIN`] more, a second seal
/// moves the live edge far enough that the delivered groups fall outside the
/// track's retention. A late subscriber then starts at the seal instead of
/// being replayed already delivered frames. A seal group holds one empty,
/// timestamped frame (retention is measured on frame timestamps); readers
/// skip empty frames, which no valid ops frame is. Nothing is sent once new
/// groups arrive.
fn seal_later(track: moq_net::track::Producer, sequence: u64) {
    tokio::spawn(async move {
        let mut expected = sequence;
        for wait in [
            SEAL_AFTER,
            EGRESS_RETAIN.saturating_add(Duration::from_millis(100)),
        ] {
            tokio::time::sleep(wait).await;
            if track.is_closed() || track.latest() != Some(expected) {
                return;
            }
            let Ok(mut seal) = track.append_group() else {
                return;
            };
            expected = seal.sequence;
            let _ = seal.write_frame(stamp(), bytes::Bytes::new());
            let _ = seal.finish();
        }
    });
}

/// Isolated loopback live relay with a generated short-lived certificate, for
/// device-free tests. Never binds a non-loopback address.
pub struct LoopbackLive {
    pub endpoint: String,
    pub fingerprints: Vec<String>,
    pub relay: LiveRelay,
    task: tokio::task::JoinHandle<()>,
}
impl LoopbackLive {
    /// Must be called inside a Tokio runtime.
    ///
    /// # Errors
    ///
    /// Returns a message when the loopback server cannot be configured or bound.
    pub fn start(authority: Arc<dyn LiveAuthority>) -> Result<Self, String> {
        let listener = carrier::loopback()?;
        let relay = LiveRelay::new(authority);
        let task = tokio::spawn(relay.clone().serve(listener.endpoint));
        Ok(Self {
            endpoint: format!("https://127.0.0.1:{}{LIVE_PATH}", listener.port),
            fingerprints: listener.fingerprints,
            relay,
            task,
        })
    }
}
impl Drop for LoopbackLive {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// HTTPS fallback carrier for clients without a working WebTransport.
pub mod fallback;

/// Native live participant (the client side of this relay).
pub mod peer;

#[cfg(test)]
mod tests;
