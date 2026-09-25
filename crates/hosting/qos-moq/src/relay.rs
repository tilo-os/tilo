//! Host-injected authority for a Draft 21 WebTransport voice relay.
//!
//! The protocol does not mint workspace permissions. The native host supplies
//! an implementation whose private session evidence comes from its verified
//! grant/challenge service and current committed room authority.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::{
    Arc, Mutex, Weak,
    atomic::{AtomicU64, Ordering},
};
use std::time::{Duration, Instant};

pub const MAX_SESSIONS: usize = 8;
pub const MAX_AUTHORIZATION_BYTES: usize = 8192;
pub const VOICE_PATH: &str = "/moq/v21/voice";
const AUTHORITY_INTERVAL: Duration = Duration::from_millis(100);
/// Longest continuous authority outage (owner contention) a live session may
/// ride through, and the longest an offered namespace whose publisher
/// authority lapsed may stay present before the subscriber fails closed.
const AUTHORITY_FENCE: Duration = Duration::from_millis(900);
/// Budget for the QUIC + WebTransport handshake of one pending session. A
/// satellite path (600 ms RTT) needs ~4 round trips plus a loss recovery
/// (PTO ~1 s), so the former 3 s bound refused honest joins on such links.
pub const HANDSHAKE_BUDGET: Duration = Duration::from_secs(8);
/// Budget for the MoQ SETUP exchange after the CONNECT response.
pub const SETUP_BUDGET: Duration = Duration::from_secs(5);
const EXPIRE_INTERVAL: Duration = Duration::from_millis(20);
/// How long a revoked or expired session may stay open, carrying no media, after
/// GOAWAY so the client closes first. Chromium can crash its renderer when the
/// server closes a WebTransport session with streams in flight. Forwarding stops
/// when authority is lost, not when this grace ends.
pub const GOAWAY_GRACE: Duration = Duration::from_secs(1);

/// Host-internal marker for a fresh per-request exact raw route comparison.
///
/// This marker is neither authentication nor independent authority evidence.
/// Its private field only requires callers to use [`Relay::validate_route`]
/// before URL normalization or MoQ SETUP parsing for each accepted request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VoiceRoute(());

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    Publish,
    Subscribe,
}

/// A host-owned, privately constructed authorization result. Implementations
/// must recheck expiry, revocation, membership and participation generation in
/// `current`. Names are exact opaque broadcast paths, never arbitrary prefixes.
#[derive(Clone, Copy, Debug)]
pub enum AuthorizationStatus {
    Current,
    /// Temporary observation contention, bounded by the verified lease's
    /// monotonic deadline. This never grants a new session admission.
    Unavailable {
        expires_at: Instant,
    },
    Denied,
}

pub trait SessionAuthorization: Send + Sync {
    fn publication(&self) -> Option<&str>;
    fn subscriptions(&self) -> &[String];
    fn current(&self, direction: Direction, namespace: &str) -> bool;
    fn status(&self, direction: Direction, namespace: &str) -> AuthorizationStatus {
        if self.current(direction, namespace) {
            AuthorizationStatus::Current
        } else {
            AuthorizationStatus::Denied
        }
    }
    /// Observe the complete exact scope set. Owners that reconstruct current
    /// authority can override this to validate every scope in one guarded turn.
    fn status_all(&self) -> AuthorizationStatus {
        combine_scope_status(self)
    }
    /// Exact call component under which this session is offered every
    /// broadcast the relay carries, instead of the fixed exact
    /// [`subscriptions`](Self::subscriptions) set. The relay observes each
    /// offered namespace through [`status_offered`](Self::status_offered)
    /// every authority interval, so a roster change needs no new session.
    /// Only a publisher admitted into the same call can publish beneath it.
    fn subscription_prefix(&self) -> Option<&str> {
        None
    }
    /// Observe this session's own authority, its publication and every
    /// namespace currently offered under the subscription prefix, in one
    /// guarded turn where possible. An offered namespace whose *publisher*
    /// authority lapsed while this session's own authority is current is
    /// pushed to `lapsed` instead of denying the whole session: the relay
    /// closes that publication and fails this session closed only if it
    /// stays offered past the authority fence. The default fails closed.
    fn status_offered(&self, offered: &[&str], lapsed: &mut Vec<String>) -> AuthorizationStatus {
        let _ = lapsed;
        offered.iter().fold(self.status_all(), |status, name| {
            merge_status(status, self.status(Direction::Subscribe, name))
        })
    }
}

/// Admission runs before any origin is attached. The opaque proof is passed
/// only here and is never included in errors, URLs, tracing or counters.
pub trait RelayAuthority: Send + Sync {
    fn admit(&self, proof: &[u8]) -> Result<Arc<dyn SessionAuthorization>, Denied>;
    /// Like [`admit`](Self::admit), but reports owner/state contention that
    /// occurred *before* any proof was verified or consumed as
    /// [`Admission::Busy`], so the relay may retry within a short bound.
    /// Implementations must never return `Busy` after consuming the proof.
    fn try_admit(&self, proof: &[u8]) -> Result<Arc<dyn SessionAuthorization>, Admission> {
        self.admit(proof).map_err(|_| Admission::Denied)
    }
}

#[derive(Clone, Copy, Debug, thiserror::Error)]
#[error("media admission denied")]
pub struct Denied;

/// Admission failure classification. `Busy` is transient contention; it never
/// admits by itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Admission {
    Denied,
    Busy,
}

/// New-session admission waits at most this long for transient owner/state
/// contention to clear. It admits only on a `Current` observation.
pub const ADMISSION_CONTENTION_WAIT: Duration = Duration::from_millis(250);
const ADMISSION_RETRY: Duration = Duration::from_millis(5);

#[derive(Default)]
struct Counters {
    accepted: AtomicU64,
    rejected: AtomicU64,
    expired: AtomicU64,
    active: AtomicU64,
    fenced: AtomicU64,
    lapsed: AtomicU64,
    max_tick_gap_ms: AtomicU64,
    max_observe_ms: AtomicU64,
}

/// Content-free lifecycle counters. These are not packet or audio-quality
/// measurements, and never expose the proof or participant namespace.
/// `fenced` counts sessions closed because authority stayed unobservable past
/// the fence; `lapsed` counts publications closed because a subscriber
/// observed their publisher authority lapse. The two maxima (milliseconds)
/// diagnose scheduler stalls versus slow owner observations.
#[derive(Clone, Copy, Debug, Default, serde::Serialize)]
pub struct CounterSnapshot {
    pub accepted: u64,
    pub rejected: u64,
    pub expired: u64,
    pub active: u64,
    pub fenced: u64,
    pub lapsed: u64,
    pub max_tick_gap_ms: u64,
    pub max_observe_ms: u64,
}

/// Wakes one live session so it closes its publication.
#[derive(Default)]
struct Revocation(tokio::sync::Notify);

/// Live publishing sessions by exact namespace. Holds no strong reference, so
/// a finished session never outlives its task through this index.
type Publications = Arc<Mutex<HashMap<String, Vec<Weak<Revocation>>>>>;

struct Publication {
    index: Publications,
    name: String,
    revocation: Arc<Revocation>,
}
impl Drop for Publication {
    fn drop(&mut self) {
        let Ok(mut index) = self.index.lock() else {
            return;
        };
        if let Some(entries) = index.get_mut(&self.name) {
            entries.retain(|entry| {
                entry.strong_count() > 0
                    && !std::ptr::eq(entry.as_ptr(), Arc::as_ptr(&self.revocation))
            });
            if entries.is_empty() {
                index.remove(&self.name);
            }
        }
    }
}

#[derive(Clone)]
pub struct Relay {
    authority: Arc<dyn RelayAuthority>,
    origin: moq_net::origin::Producer,
    counters: Arc<Counters>,
    sessions: Arc<tokio::sync::Semaphore>,
    publications: Publications,
}

struct Active(Arc<Counters>);
impl Drop for Active {
    fn drop(&mut self) {
        self.0.active.fetch_sub(1, Ordering::Relaxed);
    }
}

struct RelayScopes {
    authorization: Arc<dyn SessionAuthorization>,
    publish: Option<moq_net::origin::Producer>,
    subscribe: Option<moq_net::origin::Consumer>,
    offered: Option<Offered>,
}

/// The exact namespaces currently offered to one prefix-scoped subscriber:
/// a relay-private announce cursor over the same scope the client sees.
struct Offered {
    cursor: moq_net::announce::Consumer,
    present: BTreeSet<String>,
    /// First observation of each offered namespace whose publisher lapsed.
    lapsed: BTreeMap<String, Instant>,
}
impl Offered {
    fn drain(&mut self) {
        while let Some(update) = self.cursor.try_next() {
            let name = update.path.as_str().to_owned();
            if update.broadcast.is_some() {
                self.present.insert(name);
            } else {
                self.present.remove(&name);
            }
        }
    }
}

impl Relay {
    pub fn new(authority: Arc<dyn RelayAuthority>) -> Self {
        let limits = moq_net::origin::Limits::voice()
            .with_frame_validator(|bytes| crate::voice::validate_bundle(bytes).is_ok());
        Self {
            authority,
            origin: moq_net::origin::Info::new(moq_net::Origin::random())
                .with_limits(limits)
                .produce(),
            counters: Arc::default(),
            sessions: Arc::new(tokio::sync::Semaphore::new(MAX_SESSIONS)),
            publications: Arc::default(),
        }
    }

    pub fn counters(&self) -> CounterSnapshot {
        CounterSnapshot {
            accepted: self.counters.accepted.load(Ordering::Relaxed),
            rejected: self.counters.rejected.load(Ordering::Relaxed),
            expired: self.counters.expired.load(Ordering::Relaxed),
            active: self.counters.active.load(Ordering::Relaxed),
            fenced: self.counters.fenced.load(Ordering::Relaxed),
            lapsed: self.counters.lapsed.load(Ordering::Relaxed),
            max_tick_gap_ms: self.counters.max_tick_gap_ms.load(Ordering::Relaxed),
            max_observe_ms: self.counters.max_observe_ms.load(Ordering::Relaxed),
        }
    }

    /// Validate the exact raw HTTP/3 route selected by a shared host ingress.
    ///
    /// Both arguments must come from the unnormalized CONNECT pseudo-headers.
    /// `expected_authority` is the canonical authority from the installed host
    /// configuration. Queries, dot segments, percent aliases, credentials and
    /// alternate authority spellings fail byte-for-byte comparison.
    pub fn validate_route(
        raw_authority: &str,
        expected_authority: &str,
        raw_target: &str,
    ) -> Option<VoiceRoute> {
        (raw_authority == expected_authority && raw_target == VOICE_PATH).then_some(VoiceRoute(()))
    }

    /// Serve an already configured Draft 21-only listener. At most eight
    /// admitted/authorizing tasks exist. Dropping this future aborts all tasks
    /// and their protocol sessions; no detached session survives shutdown.
    pub async fn serve(self, server: moq_native::Server) {
        let Ok(mut server) = server.with_accept_limits(16, HANDSHAKE_BUDGET) else {
            return;
        };
        let mut sessions = tokio::task::JoinSet::new();
        let mut expiry = tokio::time::interval(EXPIRE_INTERVAL);
        expiry.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                request = server.accept() => {
                    let Some(request) = request else { break; };
                    if sessions.len() >= MAX_SESSIONS {
                        self.counters.rejected.fetch_add(1, Ordering::Relaxed);
                        let _ = tokio::time::timeout(Duration::from_millis(20), request.close(429)).await;
                    } else {
                        let relay = self.clone();
                        sessions.spawn(async move { relay.serve_request(request).await });
                    }
                }
                _ = sessions.join_next(), if !sessions.is_empty() => {}
                _ = expiry.tick() => self.origin.expire(),
            }
        }
        sessions.abort_all();
        while sessions.join_next().await.is_some() {}
        server.close().await;
    }

    /// Handle a request from the existing dedicated [`moq_native`] listener.
    ///
    /// This path preserves the isolated proof server. A shared raw H3 listener
    /// must validate the unnormalized route and call [`serve_accepted`](Self::serve_accepted)
    /// so its MoQ driver remains in the host-owned task.
    pub async fn serve_request(&self, mut request: moq_native::Request) {
        let Ok(_permit) = self.sessions.clone().try_acquire_owned() else {
            self.counters.rejected.fetch_add(1, Ordering::Relaxed);
            let _ = tokio::time::timeout(Duration::from_millis(20), request.close(429)).await;
            return;
        };
        // This endpoint is the WebTransport binding only. URL components carry
        // routing, never credentials. Raw QUIC and every other draft fail closed.
        let webtransport = request.url().is_some_and(|url| url.scheme() == "https")
            && request.path() == VOICE_PATH
            && request.query().is_none()
            && request.version().as_str() == "moq-transport-21";
        if !webtransport {
            self.reject(request).await;
            return;
        }
        let proof = request.authorization().map(<[u8]>::to_vec);
        let Some(scopes) = self.scopes(request.version(), proof.as_deref()).await else {
            self.reject(request).await;
            return;
        };

        // Only host-owned scopes select the shared origin. The client has not
        // been attached to any producer/consumer until admission is complete.
        if let Some(publish) = scopes.publish {
            request = request.with_subscriber(publish);
        }
        if let Some(subscribe) = scopes.subscribe {
            request = request.with_publisher(subscribe);
        }
        let Ok(Ok(session)) = tokio::time::timeout(SETUP_BUDGET, request.ok()).await else {
            return;
        };
        self.run_session(session, None, scopes.authorization, scopes.offered)
            .await;
    }

    /// Serve a Draft 21 MoQ request produced by a shared, H3-only WebTransport
    /// listener after exact raw route validation.
    ///
    /// This uses the same admission, exact namespace scoping, eight-session
    /// budget, 900 ms authority fence and cache expiry loop as [`serve_request`].
    /// The MoQ protocol driver is retained in this future and cancelled on any
    /// authorization failure or caller cancellation; it is never detached.
    pub async fn serve_accepted<S>(&self, _route: VoiceRoute, mut request: moq_net::Request<S>)
    where
        S: moq_net::web_transport_trait::Session,
    {
        let Ok(_permit) = self.sessions.clone().try_acquire_owned() else {
            self.reject_accepted(request);
            return;
        };
        let proof = request.authorization().map(<[u8]>::to_vec);
        let Some(scopes) = self.scopes(request.version(), proof.as_deref()).await else {
            self.reject_accepted(request);
            return;
        };
        if let Some(publish) = scopes.publish {
            request = request.with_subscriber(publish);
        }
        if let Some(subscribe) = scopes.subscribe {
            request = request.with_publisher(subscribe);
        }
        let Ok(Ok((session, driver))) = tokio::time::timeout(SETUP_BUDGET, request.ok()).await
        else {
            return;
        };
        self.run_session(session, Some(driver), scopes.authorization, scopes.offered)
            .await;
    }

    async fn scopes(&self, version: moq_net::Version, proof: Option<&[u8]>) -> Option<RelayScopes> {
        if version.as_str() != "moq-transport-21" {
            return None;
        }
        let proof =
            proof.filter(|proof| !proof.is_empty() && proof.len() <= MAX_AUTHORIZATION_BYTES)?;
        // Transient contention (an owner commit in progress, or another
        // session's periodic check holding the authority) is waited out for a
        // short bound instead of turning an honest join into a denial. Only a
        // Current observation admits; the bound and expiry keep this fail-closed.
        let deadline = Instant::now() + ADMISSION_CONTENTION_WAIT;
        let authorization = loop {
            match self.authority.try_admit(proof) {
                Ok(authorization) => break authorization,
                Err(Admission::Busy) if Instant::now() < deadline => {
                    tokio::time::sleep(ADMISSION_RETRY).await;
                }
                Err(_) => return None,
            }
        };
        if !scopes_well_formed(authorization.as_ref()) {
            return None;
        }
        // The subscriber scope and its private cursor exist before the first
        // observation, so admission observes exactly what will be offered.
        let (subscribe, mut offered) = match authorization.subscription_prefix() {
            Some(prefix) => {
                let consumer = self.origin.consume().scope(&[prefix.into()])?;
                let offered = Offered {
                    cursor: consumer.announced(),
                    present: BTreeSet::new(),
                    lapsed: BTreeMap::new(),
                };
                (Some(consumer), Some(offered))
            }
            None if authorization.subscriptions().is_empty() => (None, None),
            None => {
                let names: Vec<moq_net::Path<'_>> = authorization
                    .subscriptions()
                    .iter()
                    .map(|name| name.as_str().into())
                    .collect();
                (Some(self.origin.consume().scope_exact(&names)?), None)
            }
        };
        loop {
            match self.observe(authorization.as_ref(), offered.as_mut()) {
                AuthorizationStatus::Current => break,
                AuthorizationStatus::Unavailable { expires_at }
                    if Instant::now() < deadline.min(expires_at) =>
                {
                    tokio::time::sleep(ADMISSION_RETRY).await;
                }
                _ => return None,
            }
        }
        let publish = match authorization.publication() {
            Some(namespace) => Some(self.origin.scope_exact(&[namespace.into()])?),
            None => None,
        };
        Some(RelayScopes {
            authorization,
            publish,
            subscribe,
            offered,
        })
    }

    /// One authority observation. A prefix-scoped subscriber is observed
    /// against exactly the namespaces the relay currently offers it. A lapsed
    /// publisher is not this subscriber's revocation: its publication is
    /// closed instead, and the subscriber fails closed only if that namespace
    /// is still offered after the authority fence.
    fn observe(
        &self,
        authorization: &dyn SessionAuthorization,
        offered: Option<&mut Offered>,
    ) -> AuthorizationStatus {
        let started = Instant::now();
        let status = match offered {
            None => scope_status(authorization),
            Some(offered) => {
                offered.drain();
                let own = authorization.publication();
                let names: Vec<&str> = offered
                    .present
                    .iter()
                    .map(String::as_str)
                    .filter(|name| Some(*name) != own)
                    .collect();
                if names.len() > MAX_SESSIONS {
                    AuthorizationStatus::Denied
                } else {
                    let mut lapsed = Vec::new();
                    let status = authorization.status_offered(&names, &mut lapsed);
                    let now = Instant::now();
                    offered.lapsed.retain(|name, _| lapsed.contains(name));
                    for name in lapsed {
                        self.revoke_publication(&name);
                        offered.lapsed.entry(name).or_insert(now);
                    }
                    let stale = offered
                        .lapsed
                        .values()
                        .any(|since| now.duration_since(*since) >= AUTHORITY_FENCE);
                    if stale {
                        AuthorizationStatus::Denied
                    } else {
                        status
                    }
                }
            }
        };
        let elapsed = started.elapsed().as_millis().try_into().unwrap_or(u64::MAX);
        self.counters
            .max_observe_ms
            .fetch_max(elapsed, Ordering::Relaxed);
        status
    }

    fn register_publication(&self, name: &str) -> Publication {
        let revocation = Arc::new(Revocation::default());
        if let Ok(mut index) = self.publications.lock() {
            let entries = index.entry(name.to_owned()).or_default();
            entries.retain(|entry| entry.strong_count() > 0);
            entries.push(Arc::downgrade(&revocation));
        }
        Publication {
            index: self.publications.clone(),
            name: name.to_owned(),
            revocation,
        }
    }

    /// Close every live session publishing exactly `name`.
    fn revoke_publication(&self, name: &str) {
        let Ok(index) = self.publications.lock() else {
            return;
        };
        for revocation in index
            .get(name)
            .into_iter()
            .flatten()
            .filter_map(Weak::upgrade)
        {
            revocation.0.notify_one();
        }
    }

    async fn run_session(
        &self,
        session: moq_net::Session,
        driver: Option<moq_net::Driver>,
        authorization: Arc<dyn SessionAuthorization>,
        mut offered: Option<Offered>,
    ) {
        // Scopes were observed Current during admission; the validity loop
        // below re-observes every 100 ms with the bounded outage fence.
        if session.version().as_str() != "moq-transport-21"
            || !scopes_well_formed(authorization.as_ref())
        {
            session.abort(moq_net::Error::Unauthorized);
            self.counters.rejected.fetch_add(1, Ordering::Relaxed);
            return;
        }
        self.counters.accepted.fetch_add(1, Ordering::Relaxed);
        self.counters.active.fetch_add(1, Ordering::Relaxed);
        let _active = Active(self.counters.clone());
        let publication = authorization
            .publication()
            .map(|name| self.register_publication(name));
        let revoked = async {
            match &publication {
                Some(publication) => publication.revocation.0.notified().await,
                None => std::future::pending::<()>().await,
            }
        };
        tokio::pin!(revoked);
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
        let mut expiry = tokio::time::interval(EXPIRE_INTERVAL);
        expiry.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut last_current = Instant::now();
        let mut last_tick = last_current;
        let mut unavailable_deadline: Option<Instant> = None;
        loop {
            tokio::select! {
                _ = session.closed() => break,
                _ = &mut retained_driver => break,
                // Another session observed this publication's authority lapse.
                _ = &mut revoked => {
                    self.counters.lapsed.fetch_add(1, Ordering::Relaxed);
                    self.counters.expired.fetch_add(1, Ordering::Relaxed);
                    revoke(&session, retained_driver.as_mut()).await;
                    break;
                },
                _ = async {
                    if let Some(deadline) = unavailable_deadline {
                        tokio::time::sleep_until(deadline.into()).await;
                    } else {
                        std::future::pending::<()>().await;
                    }
                } => {
                    self.counters.fenced.fetch_add(1, Ordering::Relaxed);
                    self.counters.expired.fetch_add(1, Ordering::Relaxed);
                    revoke(&session, retained_driver.as_mut()).await;
                    break;
                },
                _ = expiry.tick() => self.origin.expire(),
                _ = validity.tick() => {
                    let ticked = Instant::now();
                    let gap = ticked.duration_since(last_tick).as_millis().try_into().unwrap_or(u64::MAX);
                    self.counters.max_tick_gap_ms.fetch_max(gap, Ordering::Relaxed);
                    last_tick = ticked;
                    let observed = self.observe(authorization.as_ref(), offered.as_mut());
                    // Deadline expiry is terminal even if an unbiased select
                    // chooses revalidation first, or the observation itself
                    // returns after the deadline. A late Current cannot revive
                    // an expired outage/verified-grant fence.
                    let observed_at = Instant::now();
                    let observed = if unavailable_deadline.is_some_and(|deadline| observed_at >= deadline) {
                        AuthorizationStatus::Denied
                    } else {
                        observed
                    };
                    match observed {
                        AuthorizationStatus::Current => {
                            last_current = observed_at;
                            unavailable_deadline = None;
                        }
                        AuthorizationStatus::Unavailable { expires_at } => {
                            let deadline = (last_current + AUTHORITY_FENCE).min(expires_at);
                            unavailable_deadline = Some(unavailable_deadline.map_or(deadline, |old| old.min(deadline)));
                        }
                        AuthorizationStatus::Denied => {
                            self.counters.expired.fetch_add(1, Ordering::Relaxed);
                            revoke(&session, retained_driver.as_mut()).await;
                            break;
                        }
                    }
                }
            }
        }
    }

    fn reject_accepted<S>(&self, request: moq_net::Request<S>)
    where
        S: moq_net::web_transport_trait::Session,
    {
        self.counters.rejected.fetch_add(1, Ordering::Relaxed);
        request.close(moq_net::Error::Unauthorized);
    }

    async fn reject(&self, request: moq_native::Request) {
        self.counters.rejected.fetch_add(1, Ordering::Relaxed);
        let _ = tokio::time::timeout(Duration::from_secs(1), request.close(403)).await;
    }
}

/// Isolated loopback Draft 21 endpoint with a generated short-lived localhost
/// certificate, for device-free native pipeline tests and proofs. It never
/// binds a non-loopback address and advertises no production authorization:
/// the caller supplies the complete (test) authority.
pub struct LoopbackRelay {
    pub endpoint: String,
    pub fingerprints: Vec<String>,
    relay: Relay,
    task: tokio::task::JoinHandle<()>,
}
impl LoopbackRelay {
    /// Must be called inside a Tokio runtime.
    pub fn start(authority: Arc<dyn RelayAuthority>) -> Result<Self, String> {
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
        let relay = Relay::new(authority);
        let task = tokio::spawn(relay.clone().serve(server));
        Ok(Self {
            endpoint: format!("https://localhost:{port}{VOICE_PATH}"),
            fingerprints,
            relay,
            task,
        })
    }
    pub fn counters(&self) -> CounterSnapshot {
        self.relay.counters()
    }
}
impl Drop for LoopbackRelay {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// End a session whose authority is gone. Forwarding stops at once (the drain
/// gate drops every datagram and cuts every stream), then GOAWAY lets the client
/// close first; the relay closes after [`GOAWAY_GRACE`] at the latest. The driver
/// keeps running meanwhile so GOAWAY is written and the client's close observed.
async fn revoke(
    session: &moq_net::Session,
    driver: std::pin::Pin<&mut impl std::future::Future<Output = ()>>,
) {
    if session.go_away(GOAWAY_GRACE) {
        tokio::select! {
            _ = session.closed() => {}
            _ = driver => {}
            _ = tokio::time::sleep(GOAWAY_GRACE) => {}
        }
    }
    session.abort(moq_net::Error::Unauthorized);
}

fn valid_namespace(namespace: &str) -> bool {
    !namespace.is_empty()
        && namespace.len() <= 256
        && namespace.split('/').all(|part| {
            !part.is_empty()
                && part
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
        })
}

/// Structural scope checks only; authority currency is observed separately.
fn scopes_well_formed(authorization: &dyn SessionAuthorization) -> bool {
    let publication = authorization.publication();
    let subscriptions = authorization.subscriptions();
    if let Some(prefix) = authorization.subscription_prefix() {
        // One opaque call component; any publication lies directly beneath it.
        return subscriptions.is_empty()
            && valid_namespace(prefix)
            && !prefix.contains('/')
            && publication.is_none_or(|name| {
                valid_namespace(name)
                    && name
                        .strip_prefix(prefix)
                        .and_then(|rest| rest.strip_prefix('/'))
                        .is_some_and(|rest| !rest.contains('/'))
            });
    }
    if (publication.is_none() && subscriptions.is_empty()) || subscriptions.len() > MAX_SESSIONS {
        return false;
    }
    if let Some(namespace) = publication
        && !valid_namespace(namespace)
    {
        return false;
    }
    subscriptions.iter().enumerate().all(|(index, namespace)| {
        valid_namespace(namespace) && !subscriptions[..index].contains(namespace)
    })
}

fn scope_status(authorization: &dyn SessionAuthorization) -> AuthorizationStatus {
    authorization.status_all()
}

/// Denial dominates; otherwise the earliest outage deadline is kept.
fn merge_status(a: AuthorizationStatus, b: AuthorizationStatus) -> AuthorizationStatus {
    match (a, b) {
        (AuthorizationStatus::Denied, _) | (_, AuthorizationStatus::Denied) => {
            AuthorizationStatus::Denied
        }
        (
            AuthorizationStatus::Unavailable { expires_at: x },
            AuthorizationStatus::Unavailable { expires_at: y },
        ) => AuthorizationStatus::Unavailable {
            expires_at: x.min(y),
        },
        (status @ AuthorizationStatus::Unavailable { .. }, AuthorizationStatus::Current)
        | (AuthorizationStatus::Current, status) => status,
    }
}

fn combine_scope_status<A: SessionAuthorization + ?Sized>(
    authorization: &A,
) -> AuthorizationStatus {
    let mut expires: Option<Instant> = None;
    let publication = authorization
        .publication()
        .map(|name| (Direction::Publish, name));
    let subscriptions = authorization
        .subscriptions()
        .iter()
        .map(|name| (Direction::Subscribe, name.as_str()));
    for (direction, namespace) in publication.into_iter().chain(subscriptions) {
        match authorization.status(direction, namespace) {
            AuthorizationStatus::Denied => return AuthorizationStatus::Denied,
            AuthorizationStatus::Current => {}
            AuthorizationStatus::Unavailable { expires_at } => {
                expires = Some(expires.map_or(expires_at, |old| old.min(expires_at)))
            }
        }
    }
    expires.map_or(AuthorizationStatus::Current, |expires_at| {
        AuthorizationStatus::Unavailable { expires_at }
    })
}

#[cfg(test)]
mod status_all_tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    struct OneTurn {
        observed: AtomicUsize,
        names: Vec<String>,
    }
    impl SessionAuthorization for OneTurn {
        fn publication(&self) -> Option<&str> {
            Some("call/publisher")
        }
        fn subscriptions(&self) -> &[String] {
            &self.names
        }
        fn current(&self, _: Direction, _: &str) -> bool {
            true
        }
        fn status_all(&self) -> AuthorizationStatus {
            self.observed.fetch_add(1, Ordering::Relaxed);
            // One of the current namespace permissions was revoked.
            AuthorizationStatus::Denied
        }
    }

    #[test]
    fn periodic_observation_uses_one_owner_turn_for_all_scopes() {
        let owner = OneTurn {
            observed: AtomicUsize::new(0),
            names: vec!["call/one".into(), "call/two".into()],
        };
        assert!(matches!(scope_status(&owner), AuthorizationStatus::Denied));
        assert_eq!(owner.observed.load(Ordering::Relaxed), 1);
    }

    struct PerScope {
        names: Vec<String>,
        first: Instant,
        second: Instant,
        denied: bool,
    }
    impl SessionAuthorization for PerScope {
        fn publication(&self) -> Option<&str> {
            Some("call/publisher")
        }
        fn subscriptions(&self) -> &[String] {
            &self.names
        }
        fn current(&self, _: Direction, _: &str) -> bool {
            true
        }
        fn status(&self, _: Direction, name: &str) -> AuthorizationStatus {
            match name {
                "call/one" => AuthorizationStatus::Unavailable {
                    expires_at: self.first,
                },
                "call/two" if self.denied => AuthorizationStatus::Denied,
                "call/two" => AuthorizationStatus::Unavailable {
                    expires_at: self.second,
                },
                _ => AuthorizationStatus::Current,
            }
        }
    }

    #[test]
    fn default_aggregate_keeps_earliest_deadline_and_any_namespace_denial() {
        let now = Instant::now();
        let mut owner = PerScope {
            names: vec!["call/one".into(), "call/two".into()],
            first: now + Duration::from_secs(5),
            second: now + Duration::from_secs(2),
            denied: false,
        };
        assert!(
            matches!(scope_status(&owner), AuthorizationStatus::Unavailable { expires_at } if expires_at == owner.second)
        );
        owner.denied = true;
        assert!(matches!(scope_status(&owner), AuthorizationStatus::Denied));
        owner.names.clear();
        assert!(matches!(scope_status(&owner), AuthorizationStatus::Current));
    }
}
