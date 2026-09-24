//! LIVE_V1 relay: ephemeral multiplayer presence and ordered ops carried as
//! MoQ Draft 21 tracks over WebTransport at [`LIVE_PATH`], beside voice.
//!
//! The relay never stores, orders durably or authorizes content. The native
//! host supplies a [`LiveAuthority`] that verifies the host-signed live grant
//! presented as the Draft 21 Authorization Token, rechecks current membership
//! on every observation, and validates frame bytes (the canonical wire codec
//! lives in the host crate; this crate stays free of that dependency).
//!
//! Topology. Every admitted publisher session gets a private ingress origin
//! scoped to exactly its own namespace `tilo-live/<room>/<publisher>`. The
//! relay (not the client) owns the egress broadcast for that namespace in one
//! shared egress origin, and pumps validated, rate-limited frames from the
//! client's tracks into it:
//!
//! * `presence`: object datagrams, forwarded immediately and latest-value
//!   (queued upstream datagrams are coalesced to the newest, stale sequence
//!   numbers are dropped, the egress send path drops when congested).
//! * `ops/<hex record digest>`: ordered streams. Each upstream group becomes
//!   one egress group, frames in order; pulled on subscriber demand.
//!
//! Subscribers receive a scope of their room prefix and subscribe per member.
//! The host may inject frames into a member's `ops/<record>` track
//! ([`LiveRelay::inject_ops`]); host frames always form their own groups.
//! Revocation aborts only the revoked publisher's egress broadcast (subscriber
//! stream resets, no session close) and sends GOAWAY to the revoked session.
use crate::relay::{
    ADMISSION_CONTENTION_WAIT, Admission, AuthorizationStatus, GOAWAY_GRACE, HANDSHAKE_BUDGET,
    SETUP_BUDGET,
};
use std::collections::HashMap;
use std::sync::{
    Arc, Mutex, Weak,
    atomic::{AtomicU64, Ordering},
};
use std::time::{Duration, Instant};

/// Exact WebTransport path of the live relay on the host's QUIC endpoint.
pub const LIVE_PATH: &str = "/moq/v21/live";
/// WebTransport subprotocol; identical to voice.
pub const LIVE_PROTOCOL: &str = "moqt-21";
/// First namespace tuple component of every live track.
pub const NAMESPACE_ROOT: &str = "tilo-live";
/// Latest-value datagram track.
pub const PRESENCE_TRACK: &str = "presence";
/// Ordered per-record stream track prefix.
pub const OPS_PREFIX: &str = "ops/";
/// Authorization token ceiling (a encoded live grant).
pub const MAX_TOKEN_BYTES: usize = 2048;
/// Per-publisher presence ceiling (frames/s).
pub const MAX_PRESENCE_RATE: u32 = 120;
/// Per-publisher ordered frame ceiling (frames/s).
pub const MAX_OPS_RATE: u32 = 240;
/// Records one publisher may have open client ops tracks for at once.
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
/// Upper bound on bytes one publisher session may have buffered at ingress.
const INGRESS_POOL_BYTES: u64 = 512 * 1024;
/// Upper bound on bytes buffered at egress across all namespaces.
const EGRESS_POOL_BYTES: u64 = 16 * 1024 * 1024;
const AUTHORITY_INTERVAL: Duration = Duration::from_millis(100);
/// Delay before a revoked, host-fed namespace is re-announced.
const REANNOUNCE_AFTER: Duration = Duration::from_millis(100);
/// A quiet ops track is sealed with an empty group after this long, so a late
/// subscriber (which starts at the latest group) is not replayed stale frames.
pub const SEAL_AFTER: Duration = Duration::from_millis(250);
/// Quiet time after which a late subscriber is no longer replayed old groups.
pub const SEAL_QUIET: Duration = Duration::from_millis(250 + 2000 + 100);
const ADMISSION_RETRY: Duration = Duration::from_millis(5);
const PRESENCE_RETRY: Duration = Duration::from_millis(100);
/// An ops track without subscribers for this long is withdrawn.
const UNUSED_GRACE: Duration = Duration::from_secs(2);

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
    /// Current lease, signature binding, host boot and membership. `Unavailable`
    /// is bounded owner contention and never admits a new session.
    fn status(&self) -> AuthorizationStatus;
}

/// Host-injected admission and frame validation. Called without awaits; must
/// not block on network I/O.
pub trait LiveAuthority: Send + Sync {
    /// Verify the Authorization Token. `Busy` only before any state is consumed.
    fn try_admit(&self, token: &[u8]) -> Result<Arc<dyn LiveSession>, Admission>;
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

/// Aborts its task on drop, so no pump outlives the slot that owns it.
struct Task(Option<tokio::task::JoinHandle<()>>);
impl Task {
    fn spawn(future: impl std::future::Future<Output = ()> + Send + 'static) -> Self {
        Self(Some(tokio::spawn(future)))
    }
    /// Release without aborting: used by a task that is finishing itself.
    fn detach(mut self) {
        self.0.take();
    }
}
impl Drop for Task {
    fn drop(&mut self) {
        if let Some(task) = self.0.take() {
            task.abort();
        }
    }
}

struct OpsEntry {
    producer: moq_net::track::Producer,
    /// Client pump for this record; counts toward [`MAX_OPEN_RECORDS`] while set.
    pump: Option<(u64, Task)>,
    _watch: Task,
}

struct Client {
    session: u64,
    broadcast: moq_net::broadcast::Consumer,
}

struct SlotState {
    broadcast: Option<moq_net::broadcast::Producer>,
    presence: Option<moq_net::track::Producer>,
    ops: HashMap<String, OpsEntry>,
    client: Option<Client>,
    presence_pump: Option<Task>,
    presence_bucket: Bucket,
    ops_bucket: Bucket,
    last_seq: Option<u64>,
    host_active: Option<Instant>,
    handler: Option<Task>,
    pump_ids: u64,
}
impl SlotState {
    fn open_records(&self) -> usize {
        self.ops
            .values()
            .filter(|entry| entry.pump.is_some())
            .count()
    }
}

struct Slot {
    state: Mutex<SlotState>,
}

struct Inner {
    authority: Arc<dyn LiveAuthority>,
    egress: moq_net::origin::Producer,
    slots: Mutex<HashMap<String, Arc<Slot>>>,
    sessions: Arc<tokio::sync::Semaphore>,
    next_session: AtomicU64,
    counters: Counters,
    /// HTTPS fallback carrier sessions ([`fallback`]).
    fallback: fallback::Registry,
}

/// The live relay. Cheap to clone; all clones share one egress origin.
#[derive(Clone)]
pub struct LiveRelay(Arc<Inner>);

struct Scopes {
    session: Arc<dyn LiveSession>,
    path: String,
    ingress: Option<moq_net::origin::Producer>,
    egress: Option<moq_net::origin::Consumer>,
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

impl LiveRelay {
    pub fn new(authority: Arc<dyn LiveAuthority>) -> Self {
        let egress = moq_net::origin::Info::new(moq_net::Origin::random())
            .with_pool(moq_net::cache::Pool::new(EGRESS_POOL_BYTES))
            .with_cache_duration(EGRESS_RETAIN)
            .with_latency_default(EGRESS_RETAIN)
            .produce();
        Self(Arc::new(Inner {
            authority,
            egress,
            slots: Mutex::default(),
            sessions: Arc::new(tokio::sync::Semaphore::new(MAX_LIVE_SESSIONS)),
            next_session: AtomicU64::new(1),
            counters: Counters::default(),
            fallback: fallback::Registry::default(),
        }))
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

    /// Exact raw CONNECT route check, before URL normalization or SETUP parsing.
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
        let mut state = slot.state.lock().unwrap();
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
            group.write_frame(
                moq_net::Timestamp::ZERO,
                bytes::Bytes::copy_from_slice(frame),
            )?;
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

    /// Withdraw host-fed namespaces idle past [`HOST_SLOT_IDLE`] with no client.
    pub fn maintain(&self) {
        let now = Instant::now();
        let idle: Vec<String> = {
            let slots = self.0.slots.lock().unwrap();
            slots
                .iter()
                .filter(|(_, slot)| {
                    let state = slot.state.lock().unwrap();
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

    /// Standalone Draft 21 listener (tests and isolated proofs).
    pub async fn serve(self, server: moq_native::Server) {
        let Ok(mut server) = server.with_accept_limits(16, HANDSHAKE_BUDGET) else {
            return;
        };
        let mut sessions = tokio::task::JoinSet::new();
        let mut maintenance = tokio::time::interval(Duration::from_secs(1));
        loop {
            tokio::select! {
                request = server.accept() => {
                    let Some(request) = request else { break };
                    let relay = self.clone();
                    sessions.spawn(async move { relay.serve_request(request).await });
                }
                _ = sessions.join_next(), if !sessions.is_empty() => {}
                _ = maintenance.tick() => self.maintain(),
            }
        }
        sessions.abort_all();
        while sessions.join_next().await.is_some() {}
        server.close().await;
    }

    /// Serve one request from a dedicated [`moq_native`] listener.
    pub async fn serve_request(&self, mut request: moq_native::Request) {
        let Ok(_permit) = self.0.sessions.clone().try_acquire_owned() else {
            self.0.counters.rejected.fetch_add(1, Ordering::Relaxed);
            let _ = tokio::time::timeout(Duration::from_millis(20), request.close(429)).await;
            return;
        };
        let webtransport = request.url().is_some_and(|url| url.scheme() == "https")
            && request.path() == LIVE_PATH
            && request.query().is_none()
            && request.version().as_str() == "moq-transport-21";
        let token = request.authorization().map(<[u8]>::to_vec);
        let scopes = match webtransport {
            true => self.scopes(request.version(), token.as_deref()).await,
            false => None,
        };
        let Some(mut scopes) = scopes else {
            self.0.counters.rejected.fetch_add(1, Ordering::Relaxed);
            let _ = tokio::time::timeout(Duration::from_secs(1), request.close(403)).await;
            return;
        };
        if let Some(ingress) = scopes.ingress.clone() {
            request = request.with_subscriber(ingress);
        }
        if let Some(egress) = scopes.egress.take() {
            request = request.with_publisher(egress);
        }
        let Ok(Ok(session)) = tokio::time::timeout(SETUP_BUDGET, request.ok()).await else {
            return;
        };
        self.run_session(session, None, scopes).await;
    }

    /// Serve a Draft 21 request from a shared H3 listener after
    /// [`validate_route`](Self::validate_route). The driver stays in this future.
    pub async fn serve_accepted<S>(&self, _route: LiveRoute, mut request: moq_net::Request<S>)
    where
        S: moq_net::web_transport_trait::Session,
    {
        let Ok(_permit) = self.0.sessions.clone().try_acquire_owned() else {
            self.0.counters.rejected.fetch_add(1, Ordering::Relaxed);
            request.close(moq_net::Error::Unauthorized);
            return;
        };
        let token = request.authorization().map(<[u8]>::to_vec);
        let Some(mut scopes) = self.scopes(request.version(), token.as_deref()).await else {
            self.0.counters.rejected.fetch_add(1, Ordering::Relaxed);
            request.close(moq_net::Error::Unauthorized);
            return;
        };
        if let Some(ingress) = scopes.ingress.clone() {
            request = request.with_subscriber(ingress);
        }
        if let Some(egress) = scopes.egress.take() {
            request = request.with_publisher(egress);
        }
        let Ok(Ok((session, driver))) = tokio::time::timeout(SETUP_BUDGET, request.ok()).await
        else {
            return;
        };
        self.run_session(session, Some(driver), scopes).await;
    }

    async fn scopes(&self, version: moq_net::Version, token: Option<&[u8]>) -> Option<Scopes> {
        if version.as_str() != "moq-transport-21" {
            return None;
        }
        let token = token.filter(|token| !token.is_empty() && token.len() <= MAX_TOKEN_BYTES)?;
        let deadline = Instant::now() + ADMISSION_CONTENTION_WAIT;
        let session = loop {
            match self.0.authority.try_admit(token) {
                Ok(session) => break session,
                Err(Admission::Busy) if Instant::now() < deadline => {
                    tokio::time::sleep(ADMISSION_RETRY).await
                }
                Err(_) => return None,
            }
        };
        let path = namespace_path(session.room(), session.publisher())?;
        if !session.publish() && !session.subscribe() {
            return None;
        }
        loop {
            match session.status() {
                AuthorizationStatus::Current => break,
                AuthorizationStatus::Unavailable { expires_at }
                    if Instant::now() < deadline.min(expires_at) =>
                {
                    tokio::time::sleep(ADMISSION_RETRY).await
                }
                _ => return None,
            }
        }
        let ingress = match session.publish() {
            true => Some(
                moq_net::origin::Info::new(moq_net::Origin::random())
                    .with_pool(moq_net::cache::Pool::new(INGRESS_POOL_BYTES))
                    .with_cache_duration(OPS_WINDOW)
                    .with_latency_default(OPS_WINDOW)
                    .produce()
                    .scope_exact(&[path.as_str().into()])?,
            ),
            false => None,
        };
        let egress = match session.subscribe() {
            true => {
                let room = format!("{NAMESPACE_ROOT}/{}", session.room());
                Some(self.0.egress.consume().scope(&[room.as_str().into()])?)
            }
            false => None,
        };
        Some(Scopes {
            session,
            path,
            ingress,
            egress,
        })
    }

    async fn run_session(
        &self,
        session: moq_net::Session,
        driver: Option<moq_net::Driver>,
        scopes: Scopes,
    ) {
        let inner = &self.0;
        if session.version().as_str() != "moq-transport-21" {
            session.abort(moq_net::Error::Unauthorized);
            inner.counters.rejected.fetch_add(1, Ordering::Relaxed);
            return;
        }
        let id = inner.next_session.fetch_add(1, Ordering::Relaxed);
        inner.counters.accepted.fetch_add(1, Ordering::Relaxed);
        inner.counters.active.fetch_add(1, Ordering::Relaxed);
        struct Active<'a>(&'a Counters);
        impl Drop for Active<'_> {
            fn drop(&mut self) {
                self.0.active.fetch_sub(1, Ordering::Relaxed);
            }
        }
        let _active = Active(&inner.counters);
        // Attach the client's own announced broadcast (re-announcements included).
        let _attach = scopes.ingress.clone().map(|ingress| {
            let relay = self.clone();
            let path = scopes.path.clone();
            Task::spawn(async move {
                loop {
                    let Some(broadcast) =
                        ingress.consume().announced_broadcast(path.as_str()).await
                    else {
                        break;
                    };
                    relay.0.attach_client(&path, id, broadcast.clone());
                    let _ = broadcast.closed().await;
                    relay.0.detach_client(&path, id);
                }
            })
        });
        let retained_driver = async move {
            match driver {
                Some(driver) => {
                    let _ = driver.await;
                }
                None => std::future::pending::<()>().await,
            }
        };
        tokio::pin!(retained_driver);
        let mut validity = tokio::time::interval(AUTHORITY_INTERVAL);
        validity.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        // Owner contention (`Unavailable`) is never revocation: sustained
        // commits can hold the committed authority for seconds. The session
        // then rides on its verified, renewable lease deadline alone; only an
        // explicit `Denied` observation (or that deadline) ends it.
        let mut unavailable: Option<Instant> = None;
        let revoked = loop {
            tokio::select! {
                _ = session.closed() => break false,
                _ = &mut retained_driver => break false,
                _ = async {
                    match unavailable {
                        Some(deadline) => tokio::time::sleep_until(deadline.into()).await,
                        None => std::future::pending::<()>().await,
                    }
                } => break true,
                _ = validity.tick() => {
                    let observed = scopes.session.status();
                    let now = Instant::now();
                    if unavailable.is_some_and(|deadline| now >= deadline) {
                        break true;
                    }
                    match observed {
                        AuthorizationStatus::Current => unavailable = None,
                        // Renewal may extend the lease while contended: track
                        // the latest reported deadline, not the earliest.
                        AuthorizationStatus::Unavailable { expires_at } => unavailable = Some(expires_at),
                        AuthorizationStatus::Denied => break true,
                    }
                }
            }
        };
        drop(_attach);
        if revoked {
            inner.counters.revoked.fetch_add(1, Ordering::Relaxed);
            // Only this publisher's tracks end (subscriber stream resets).
            let host_fed = inner.remove_slot(&scopes.path, Some(id));
            // A namespace the host still feeds is re-announced at once as a
            // host-fed namespace, so subscribers can resubscribe for the
            // host's forwarded changes. The re-announce waits briefly so the
            // withdrawal reaches subscribers as a distinct close first (an
            // immediate re-announce can be spliced into the old broadcast).
            if host_fed {
                let relay = Arc::downgrade(&self.0);
                let path = scopes.path.clone();
                tokio::spawn(async move {
                    tokio::time::sleep(REANNOUNCE_AFTER).await;
                    if let Some(inner) = relay.upgrade()
                        && let Some(slot) = inner.slot(&path, true)
                    {
                        let mut state = slot.state.lock().unwrap();
                        state.host_active.get_or_insert_with(Instant::now);
                    }
                });
            }
            // The revoked member itself may no longer subscribe: GOAWAY first so
            // the client closes; forwarding to it is cut by the abort below.
            if session.go_away(GOAWAY_GRACE) {
                tokio::select! {
                    _ = session.closed() => {}
                    _ = &mut retained_driver => {}
                    _ = tokio::time::sleep(GOAWAY_GRACE) => {}
                }
            }
            session.abort(moq_net::Error::Unauthorized);
        } else {
            inner.detach_client(&scopes.path, id);
            session.abort(moq_net::Error::Cancel);
        }
    }
}

impl Inner {
    /// The namespace slot, created with its egress broadcast when `create`.
    fn slot(self: &Arc<Self>, path: &str, create: bool) -> Option<Arc<Slot>> {
        let mut slots = self.slots.lock().unwrap();
        if let Some(slot) = slots.get(path) {
            return Some(slot.clone());
        }
        if !create {
            return None;
        }
        let mut broadcast = self
            .egress
            .create_broadcast(path, moq_net::broadcast::Route::new().with_announce(true))
            .ok()?;
        let presence = broadcast.create_track(PRESENCE_TRACK, None).ok()?;
        let dynamic = broadcast.dynamic();
        let slot = Arc::new(Slot {
            state: Mutex::new(SlotState {
                broadcast: Some(broadcast),
                presence: Some(presence),
                ops: HashMap::new(),
                client: None,
                presence_pump: None,
                presence_bucket: Bucket::new(MAX_PRESENCE_RATE),
                ops_bucket: Bucket::new(MAX_OPS_RATE),
                last_seq: None,
                host_active: None,
                handler: None,
                pump_ids: 0,
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
        slot.state.lock().unwrap().handler = Some(handler);
        slots.insert(path.to_owned(), slot.clone());
        Some(slot)
    }

    /// A subscriber asked for a track this namespace does not yet carry.
    fn on_request(self: &Arc<Self>, slot: &Arc<Slot>, request: moq_net::track::Request) {
        let name = request.name().to_owned();
        let mut state = slot.state.lock().unwrap();
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
        let producer = request.accept(
            moq_net::track::Info::default()
                .with_ordered(true)
                .with_latency_max(EGRESS_RETAIN),
        );
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
                    let idle = tokio::select! {
                        biased;
                        unused = watched.unused() => unused.is_ok(),
                        _ = std::future::ready(()) => false,
                    };
                    if idle {
                        break;
                    }
                }
                if let Some(slot) = weak.upgrade() {
                    let removed = {
                        let mut state = slot.state.lock().unwrap();
                        if let Some(broadcast) = state.broadcast.as_mut() {
                            let _ = broadcast.remove_track(&name);
                        }
                        state.ops.remove(&name)
                    };
                    if let Some(entry) = removed {
                        let OpsEntry {
                            producer,
                            pump,
                            _watch,
                        } = entry;
                        drop(pump);
                        let _ = producer.abort(moq_net::Error::Cancel);
                        // This is the watch task itself, finishing now.
                        _watch.detach();
                    }
                }
            })
        };
        state.ops.insert(
            name.clone(),
            OpsEntry {
                producer,
                pump: None,
                _watch: watch,
            },
        );
        self.start_ops_pump(slot, &mut state, &name);
    }

    fn start_ops_pump(self: &Arc<Self>, slot: &Arc<Slot>, state: &mut SlotState, name: &str) {
        let Some(client) = state.client.as_ref() else {
            return;
        };
        if state.ops.get(name).is_none_or(|entry| entry.pump.is_some()) {
            return;
        }
        if state.open_records() >= MAX_OPEN_RECORDS {
            self.counters
                .records_refused
                .fetch_add(1, Ordering::Relaxed);
            return;
        }
        state.pump_ids += 1;
        let pump_id = state.pump_ids;
        let broadcast = client.broadcast.clone();
        let entry = state.ops.get_mut(name).expect("checked entry");
        let producer = entry.producer.clone();
        let task = Task::spawn(ops_pump(
            Arc::downgrade(self),
            Arc::downgrade(slot),
            name.to_owned(),
            pump_id,
            broadcast,
            producer,
        ));
        entry.pump = Some((pump_id, task));
    }

    /// Newest session wins the namespace: a reload replaces its older session.
    fn attach_client(
        self: &Arc<Self>,
        path: &str,
        session: u64,
        broadcast: moq_net::broadcast::Consumer,
    ) {
        let Some(slot) = self.slot(path, true) else {
            return;
        };
        let mut state = slot.state.lock().unwrap();
        state.client = Some(Client {
            session,
            broadcast: broadcast.clone(),
        });
        for entry in state.ops.values_mut() {
            entry.pump = None;
        }
        state.presence_pump = state.presence.is_some().then(|| {
            Task::spawn(presence_pump(
                Arc::downgrade(self),
                Arc::downgrade(&slot),
                session,
                broadcast,
            ))
        });
        let names: Vec<String> = state.ops.keys().cloned().collect();
        for name in names {
            self.start_ops_pump(&slot, &mut state, &name);
        }
    }

    fn detach_client(self: &Arc<Self>, path: &str, session: u64) {
        let Some(slot) = self.slot(path, false) else {
            return;
        };
        let withdraw = {
            let mut state = slot.state.lock().unwrap();
            if state
                .client
                .as_ref()
                .is_none_or(|client| client.session != session)
            {
                return;
            }
            state.client = None;
            state.presence_pump = None;
            for entry in state.ops.values_mut() {
                entry.pump = None;
            }
            state
                .host_active
                .is_none_or(|at| at.elapsed() >= HOST_SLOT_IDLE)
        };
        if withdraw {
            let _ = self.remove_slot(path, Some(session));
        }
    }

    /// Remove a namespace. With `session`, only if that session still owns it
    /// (or no client does). Revocation aborts the broadcast: subscribers see
    /// their track streams reset, their sessions stay open.
    /// Returns whether the removed namespace was recently host-fed.
    fn remove_slot(&self, path: &str, session: Option<u64>) -> bool {
        let slot = {
            let mut slots = self.slots.lock().unwrap();
            let Some(slot) = slots.get(path) else {
                return false;
            };
            if let Some(session) = session {
                let state = slot.state.lock().unwrap();
                if state
                    .client
                    .as_ref()
                    .is_some_and(|client| client.session != session)
                {
                    return false;
                }
            }
            slots.remove(path)
        };
        let Some(slot) = slot else { return false };
        let mut state = slot.state.lock().unwrap();
        state.presence_pump = None;
        state.client = None;
        for (_, entry) in state.ops.drain() {
            let OpsEntry {
                producer,
                pump,
                _watch,
            } = entry;
            drop(pump);
            let _ = producer.abort(moq_net::Error::Unauthorized);
        }
        if let Some(presence) = state.presence.take() {
            let _ = presence.abort(moq_net::Error::Unauthorized);
        }
        // Tracks were reset above (subscriber stream resets); the namespace
        // itself ends with a deliberate finish, which every hop forwards as a
        // clean withdrawal (announce inactive / PUBLISH_NAMESPACE_DONE) and
        // which closes subscribers' broadcast handles. A later re-announce
        // (host-fed replacement) is a new, separate announcement.
        if let Some(mut broadcast) = state.broadcast.take() {
            broadcast.finish();
        }
        state.handler = None;
        state
            .host_active
            .is_some_and(|at| at.elapsed() < HOST_SLOT_IDLE)
    }

    /// Validate and forward one presence frame. Latest-value: a sequence at or
    /// below the last forwarded one is stale and dropped.
    fn forward_presence(&self, slot: &Slot, session: u64, payload: bytes::Bytes) -> bool {
        let valid = payload.len() <= MAX_DATAGRAM_FRAME_BYTES;
        let seq = valid
            .then(|| self.authority.presence_seq(&payload))
            .flatten();
        let mut state = slot.state.lock().unwrap();
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
            if let Some(track) = state.presence.as_mut() {
                if track
                    .append_datagram(moq_net::Timestamp::ZERO, payload)
                    .is_ok()
                {
                    self.counters
                        .presence_forwarded
                        .fetch_add(1, Ordering::Relaxed);
                    return true;
                }
            }
        }
        self.counters
            .presence_dropped
            .fetch_add(1, Ordering::Relaxed);
        true
    }

    fn release_pump(&self, slot: &Slot, name: &str, pump_id: u64) {
        let mut state = slot.state.lock().unwrap();
        if let Some(entry) = state.ops.get_mut(name) {
            if entry.pump.as_ref().is_some_and(|(id, _)| *id == pump_id) {
                // The task is finishing on its own; do not abort it from inside.
                if let Some((_, task)) = entry.pump.take() {
                    task.detach();
                }
            }
        }
    }
}

async fn presence_pump(
    inner: Weak<Inner>,
    slot: Weak<Slot>,
    session: u64,
    broadcast: moq_net::broadcast::Consumer,
) {
    let mut subscriber = loop {
        if let Ok(track) = broadcast.track(PRESENCE_TRACK) {
            if let Ok(subscriber) = track.subscribe(None).await {
                break subscriber;
            }
        }
        if broadcast.is_closed() {
            return;
        }
        tokio::time::sleep(PRESENCE_RETRY).await;
    };
    while let Ok(Some(first)) = subscriber.recv_datagram().await {
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
        offer(first.payload);
        loop {
            tokio::select! {
                biased;
                next = subscriber.recv_datagram() => match next {
                    Ok(Some(next)) => offer(next.payload),
                    _ => break,
                },
                _ = std::future::ready(()) => break,
            }
        }
        let Some(payload) = latest else { continue };
        let Some(slot) = slot.upgrade() else { return };
        if !owner.forward_presence(&slot, session, payload) {
            return;
        }
    }
}

async fn ops_pump(
    inner: Weak<Inner>,
    slot: Weak<Slot>,
    name: String,
    pump_id: u64,
    broadcast: moq_net::broadcast::Consumer,
    egress: moq_net::track::Producer,
) {
    let release = || {
        if let (Some(inner), Some(slot)) = (inner.upgrade(), slot.upgrade()) {
            inner.release_pump(&slot, &name, pump_id);
        }
    };
    let mut subscription = moq_net::track::Subscription::default();
    subscription.ordered = true;
    subscription.latency_max = OPS_WINDOW;
    let subscriber = match broadcast.track(&name) {
        Ok(track) => track.subscribe(subscription).await.ok(),
        Err(_) => None,
    };
    let Some(mut subscriber) = subscriber else {
        // Not published (yet): release the open-record slot; the egress track
        // stays for host-fed frames and a later client attach.
        release();
        return;
    };
    let record = ops_record(&name).unwrap_or_default().to_owned();
    let mut groups = tokio::task::JoinSet::new();
    loop {
        tokio::select! {
            group = subscriber.recv_group() => {
                let Ok(Some(group)) = group else { break };
                if groups.len() >= MAX_GROUPS_IN_FLIGHT {
                    if let Some(inner) = inner.upgrade() {
                        inner.counters.ops_dropped.fetch_add(1, Ordering::Relaxed);
                    }
                    continue;
                }
                let mut producer = egress.clone();
                let Ok(out) = producer.append_group() else { break };
                groups.spawn(copy_group(inner.clone(), slot.clone(), record.clone(), group, out, egress.clone()));
            }
            _ = groups.join_next(), if !groups.is_empty() => {}
        }
    }
    while groups.join_next().await.is_some() {}
    release();
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
            && slot.state.lock().unwrap().ops_bucket.take(Instant::now());
        if !admitted
            || out
                .write_frame(moq_net::Timestamp::ZERO, frame.payload)
                .is_err()
        {
            // An ordered group with a gap is useless: reset it so receivers know.
            inner.counters.ops_dropped.fetch_add(1, Ordering::Relaxed);
            let _ = out.abort(moq_net::Error::ProtocolViolation);
            return;
        }
        inner.counters.ops_forwarded.fetch_add(1, Ordering::Relaxed);
    }
}

/// After [`SEAL_AFTER`] of quiet, append an empty finished group so the last
/// delivered group is no longer the latest; after [`EGRESS_RETAIN`] more, a
/// second empty group lets the cache (which evicts on write) drop the stale
/// groups. A late subscriber then starts at the next group instead of being
/// replayed already delivered frames. Nothing is sent once new groups arrive.
fn seal_later(mut track: moq_net::track::Producer, sequence: u64) {
    tokio::spawn(async move {
        let mut expected = sequence;
        for wait in [SEAL_AFTER, EGRESS_RETAIN + Duration::from_millis(100)] {
            tokio::time::sleep(wait).await;
            if track.is_closed() || track.latest() != Some(expected) {
                return;
            }
            let Ok(mut seal) = track.append_group() else {
                return;
            };
            expected = seal.sequence;
            let _ = seal.finish();
        }
    });
}

/// Isolated loopback live relay with a generated localhost certificate, for
/// device-free tests. Never binds a non-loopback address.
pub struct LoopbackLive {
    pub endpoint: String,
    pub fingerprints: Vec<String>,
    pub relay: LiveRelay,
    task: tokio::task::JoinHandle<()>,
}
impl LoopbackLive {
    /// Must be called inside a Tokio runtime.
    pub fn start(authority: Arc<dyn LiveAuthority>) -> Result<Self, String> {
        let mut config = moq_native::ServerConfig::default();
        config.bind = Some("127.0.0.1:0".into());
        config.version = vec!["moq-transport-21".parse().map_err(|_| "draft 21")?];
        config.tls.generate = vec!["localhost".into()];
        let server = config.init().map_err(|error| error.to_string())?;
        let port = server
            .local_addr()
            .map_err(|error| error.to_string())?
            .port();
        let fingerprints = server.certificates().fingerprints();
        let relay = LiveRelay::new(authority);
        let task = tokio::spawn(relay.clone().serve(server));
        Ok(Self {
            endpoint: format!("https://localhost:{port}{LIVE_PATH}"),
            fingerprints,
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

#[cfg(test)]
mod tests;
