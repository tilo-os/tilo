//! Host-injected authority for the `moq-lite` voice relay.
//!
//! The protocol does not mint workspace permissions. The native host supplies
//! an implementation whose private session evidence comes from its verified
//! grant/challenge service and current committed room authority. The grant
//! arrives in the carrier's admission preface ([`crate::carrier`]).
//!
//! Every admitted publisher writes into a private ingress origin scoped to its
//! exact namespace. The relay copies its `audio` track into the shared origin,
//! validating every object and datagram bundle and bounding the groups in
//! flight, so subscribers only ever observe validated tilo-voice/1 objects.
//! Every session is served from its own host-owned view of the shared origin
//! ([`crate::mirror`]), which revocation drops before anything else.

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::sync::{
    Arc, Mutex, Weak,
    atomic::{AtomicU64, Ordering},
};
use std::task::Poll;
use std::time::{Duration, Instant};

use crate::carrier;
pub use crate::carrier::{HANDSHAKE_BUDGET, SETUP_BUDGET};
use crate::session_authority::{AuthorityDriver, Verdict};
use crate::voice;

pub const MAX_SESSIONS: usize = 8;
pub const MAX_AUTHORIZATION_BYTES: usize = carrier::MAX_GRANT_BYTES;
/// Exact WebTransport path of the voice relay.
pub const VOICE_PATH: &str = "/moq/voice";
/// The one track a voice broadcast carries.
pub const VOICE_TRACK: &str = "audio";
/// Longest an offered namespace whose publisher authority lapsed may stay
/// offered to a subscriber before that subscriber fails closed.
const LAPSED_OFFER_FENCE: Duration = Duration::from_millis(900);
/// How long a revoked or expired session may stay open after the revocation
/// notice so the client closes first. Chromium can crash its renderer when the
/// server closes a WebTransport session with streams in flight. No media flows
/// to or from the session during this grace: its view and its publication are
/// dropped when authority is lost.
pub const GOAWAY_GRACE: Duration = Duration::from_secs(1);
/// Voice retention: late audio is dropped, never queued.
const VOICE_RETAIN: Duration = Duration::from_millis(200);
/// Bytes one publisher may have cached at the relay's ingress.
const INGRESS_BYTES: u64 = 64 * 1024;
/// Bytes cached across every forwarded voice track.
const SHARED_BYTES: u64 = 512 * 1024;
/// Bytes one subscriber's own view may cache.
const VIEW_BYTES: u64 = 64 * 1024;
/// Groups copied concurrently per publisher; a newer group evicts the oldest.
const MAX_GROUPS_IN_FLIGHT: usize = 3;

/// Host-internal marker for a fresh per-request exact raw route comparison.
///
/// This marker is neither authentication nor independent authority evidence.
/// Its private field only requires callers to use [`Relay::validate_route`]
/// before URL normalization for each accepted request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VoiceRoute(());

impl VoiceRoute {
    /// The route of a session this crate's own listener matched exactly.
    pub(crate) const fn routed() -> Self {
        Self(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    Publish,
    Subscribe,
}

/// One observation of a session's authority. There is no "cannot tell"
/// state: an authority that cannot observe current permission answers
/// `Denied`, and the session ends on that observation
/// (see `session_authority`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AuthorizationStatus {
    Current,
    Denied,
}

/// A host-owned, privately constructed authorization result. Implementations
/// must recheck expiry, revocation, membership and participation generation in
/// `current`. Names are exact opaque broadcast paths, never arbitrary prefixes.
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
    /// stays offered past the lapsed-offer fence (900 ms). The default fails
    /// closed.
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
    /// # Errors
    ///
    /// Returns [`Denied`] when the proof does not authorize a session.
    fn admit(&self, proof: &[u8]) -> Result<Arc<dyn SessionAuthorization>, Denied>;
}

#[derive(Clone, Copy, Debug, thiserror::Error)]
#[error("media admission denied")]
pub struct Denied;

#[derive(Default)]
struct Counters {
    accepted: AtomicU64,
    rejected: AtomicU64,
    expired: AtomicU64,
    active: AtomicU64,
    lapsed: AtomicU64,
    forwarded: AtomicU64,
    dropped: AtomicU64,
    max_tick_gap_ms: AtomicU64,
    max_observe_ms: AtomicU64,
}

/// Content-free lifecycle counters. These are not packet or audio-quality
/// measurements, and never expose the proof or participant namespace.
/// `lapsed` counts publications closed because a subscriber observed their
/// publisher authority lapse. `forwarded` and `dropped` count
/// objects and bundles the relay copied or refused. The two maxima
/// (milliseconds) diagnose scheduler stalls versus slow owner observations.
#[derive(Clone, Copy, Debug, Default, serde::Serialize)]
pub struct CounterSnapshot {
    pub accepted: u64,
    pub rejected: u64,
    pub expired: u64,
    pub active: u64,
    pub lapsed: u64,
    pub forwarded: u64,
    pub dropped: u64,
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
    _origin: Arc<carrier::OriginTask>,
}

struct Active(Arc<Counters>);
impl Drop for Active {
    fn drop(&mut self) {
        self.0.active.fetch_sub(1, Ordering::Relaxed);
    }
}

/// A publisher's private ingress origin: the session writes into it, the
/// relay's forwarder reads from it.
struct Ingress {
    origin: moq_net::origin::Producer,
    _task: carrier::OriginTask,
}

struct RelayScopes {
    authorization: Arc<dyn SessionAuthorization>,
    authority: AuthorityDriver,
    ingress: Option<Ingress>,
    subscribe: Option<moq_net::origin::Consumer>,
    /// The session's own view of `subscribe`, the only thing it is served from.
    view: Option<crate::mirror::Mirror>,
    /// Re-bundles the voice datagrams `view` serves for this session's loss.
    downlink: Arc<crate::mirror::Downlink>,
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
            let name = update.prefix.as_str().to_owned();
            if update.kind.is_active() {
                self.present.insert(name);
            } else {
                self.present.remove(&name);
            }
        }
    }
}

/// Exact-path patterns, or `None` when a name cannot be spelled as a path.
pub(crate) fn exact(names: &[&str]) -> Option<moq_net::Patterns> {
    names
        .iter()
        .map(|name| moq_net::Pattern::literal(name).ok())
        .collect()
}

impl Relay {
    /// Must be called inside a Tokio runtime: the shared origin's driver runs
    /// on it for as long as any clone of the relay lives.
    pub fn new(authority: Arc<dyn RelayAuthority>) -> Self {
        let (origin, task) =
            carrier::OriginTask::produce(carrier::origin_config(SHARED_BYTES, VOICE_RETAIN));
        Self {
            authority,
            origin,
            counters: Arc::default(),
            sessions: Arc::new(tokio::sync::Semaphore::new(MAX_SESSIONS)),
            publications: Arc::default(),
            _origin: Arc::new(task),
        }
    }

    pub fn counters(&self) -> CounterSnapshot {
        let load = |value: &AtomicU64| value.load(Ordering::Relaxed);
        let c = &self.counters;
        CounterSnapshot {
            accepted: load(&c.accepted),
            rejected: load(&c.rejected),
            expired: load(&c.expired),
            active: load(&c.active),
            lapsed: load(&c.lapsed),
            forwarded: load(&c.forwarded),
            dropped: load(&c.dropped),
            max_tick_gap_ms: load(&c.max_tick_gap_ms),
            max_observe_ms: load(&c.max_observe_ms),
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

    /// Serve a standalone listener for [`VOICE_PATH`]. At most eight admitted
    /// or authorizing sessions exist. Dropping this future aborts every task
    /// and its protocol session; no detached session survives shutdown.
    pub async fn serve(self, endpoint: quinn::Endpoint) {
        carrier::serve_endpoint(endpoint, &[VOICE_PATH], move |_, session| {
            let relay = self.clone();
            async move { relay.serve_session(VoiceRoute(()), session).await }
        })
        .await;
    }

    /// Serve one WebTransport session answered with [`carrier::PROTOCOL`] on
    /// an exactly validated route.
    ///
    /// This runs the admission preface, attaches only host-owned scopes, and
    /// keeps the `MoQ` driver in this future: it is cancelled on any
    /// authorization failure or caller cancellation and never detached.
    pub async fn serve_session(&self, _route: VoiceRoute, transport: web_transport_quinn::Session) {
        let Ok(_permit) = self.sessions.clone().try_acquire_owned() else {
            self.counters.rejected.fetch_add(1, Ordering::Relaxed);
            transport.close(carrier::CLOSE_DENIED, b"relay busy");
            return;
        };
        let accepted = carrier::accept(transport, |proof| async move {
            let mut scopes = self.scopes(&proof)?;
            let mut server = moq_net::Server::new();
            if let Some(ingress) = &scopes.ingress {
                let name = scopes.authorization.publication()?;
                let own = ingress.origin.scope("", &exact(&[name])?).ok()?;
                server = server.with_subscriber(own);
            }
            if let Some(subscribe) = &scopes.subscribe {
                let view = crate::mirror::Mirror::start(
                    subscribe.clone(),
                    VIEW_BYTES,
                    VOICE_RETAIN,
                    Some(scopes.downlink.clone()),
                );
                server = server.with_publisher(view.consume());
                scopes.view = Some(view);
            }
            Some((server, scopes))
        })
        .await;
        let Ok((accepted, scopes)) = accepted else {
            self.counters.rejected.fetch_add(1, Ordering::Relaxed);
            return;
        };
        self.run_session(accepted, scopes).await;
    }

    fn scopes(&self, proof: &[u8]) -> Option<RelayScopes> {
        if proof.is_empty() || proof.len() > MAX_AUTHORIZATION_BYTES {
            return None;
        }
        let authorization = self.authority.admit(proof).ok()?;
        if !scopes_well_formed(authorization.as_ref()) {
            return None;
        }
        // The subscriber scope and its private cursor exist before the first
        // observation, so admission observes exactly what will be offered.
        let (subscribe, mut offered) = match authorization.subscription_prefix() {
            Some(prefix) => {
                let subtree = moq_net::Pattern::subtree(prefix).ok()?;
                let consumer = self
                    .origin
                    .consume()
                    .scope("", &moq_net::Patterns::from(subtree))
                    .ok()?;
                let offered = Offered {
                    cursor: consumer.announced(),
                    present: BTreeSet::new(),
                    lapsed: BTreeMap::new(),
                };
                (Some(consumer), Some(offered))
            }
            None if authorization.subscriptions().is_empty() => (None, None),
            None => {
                let names: Vec<&str> = authorization
                    .subscriptions()
                    .iter()
                    .map(String::as_str)
                    .collect();
                let consumer = self.origin.consume().scope("", &exact(&names)?).ok()?;
                (Some(consumer), None)
            }
        };
        let authority =
            AuthorityDriver::admit(self.observe(authorization.as_ref(), offered.as_mut()))?;
        let ingress = authorization.publication().map(|_| {
            let (origin, task) =
                carrier::OriginTask::produce(carrier::origin_config(INGRESS_BYTES, VOICE_RETAIN));
            Ingress {
                origin,
                _task: task,
            }
        });
        Some(RelayScopes {
            authorization,
            authority,
            ingress,
            subscribe,
            view: None,
            downlink: Arc::default(),
            offered,
        })
    }

    /// One authority observation. A prefix-scoped subscriber is observed
    /// against exactly the namespaces the relay currently offers it. A lapsed
    /// publisher is not this subscriber's revocation: its publication is
    /// closed instead, and the subscriber fails closed only if that namespace
    /// is still offered after [`LAPSED_OFFER_FENCE`].
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
                        .any(|since| now.duration_since(*since) >= LAPSED_OFFER_FENCE);
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

    #[allow(
        clippy::too_many_lines,
        reason = "one select loop owns the session's authority, revocation and driver"
    )]
    async fn run_session(&self, accepted: carrier::Accepted, scopes: RelayScopes) {
        let carrier::Accepted {
            session,
            driver,
            mut admission,
            transport,
        } = accepted;
        let RelayScopes {
            authorization,
            mut authority,
            ingress,
            subscribe: _subscribe,
            mut view,
            downlink,
            mut offered,
        } = scopes;
        // Voice redundancy toward this session follows the loss its own
        // connection measures (TIL-624).
        downlink.attach((*transport).clone());
        // Scopes were observed Current during admission; the loop below
        // re-observes them on the authority driver's schedule.
        if !scopes_well_formed(authorization.as_ref()) {
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
        // The forwarder is the only path from this publisher to subscribers.
        let mut forwarder = match (&ingress, authorization.publication()) {
            (Some(ingress), Some(name)) => Some(crate::Task::spawn(forward(
                self.origin.clone(),
                ingress.origin.consume(),
                name.to_owned(),
                self.counters.clone(),
            ))),
            _ => None,
        };
        let revoked = async {
            match &publication {
                Some(publication) => publication.revocation.0.notified().await,
                None => std::future::pending::<()>().await,
            }
        };
        tokio::pin!(revoked);
        let driver = moq_net::time::run(driver);
        tokio::pin!(driver);
        let mut last_tick = Instant::now();
        let lapsed = loop {
            tokio::select! {
                _ = session.closed() => break false,
                _ = &mut driver => break false,
                // Another session observed this publication's authority lapse.
                () = &mut revoked => {
                    self.counters.lapsed.fetch_add(1, Ordering::Relaxed);
                    break true;
                },
                verdict = authority.next(|| {
                    let ticked = Instant::now();
                    let gap = ticked.duration_since(last_tick).as_millis().try_into().unwrap_or(u64::MAX);
                    self.counters.max_tick_gap_ms.fetch_max(gap, Ordering::Relaxed);
                    last_tick = ticked;
                    self.observe(authorization.as_ref(), offered.as_mut())
                }) => match verdict {
                    Verdict::Continue => {}
                    Verdict::Revoked => break true,
                },
            }
        };
        if lapsed {
            self.counters.expired.fetch_add(1, Ordering::Relaxed);
            // Media stops at once in both directions: the session's own view
            // (every track it is served) and its publication are dropped
            // before the client is told to close. The grace below only
            // covers the transport teardown.
            view.take();
            forwarder.take();
            admission.revoke().await;
            tokio::select! {
                _ = session.closed() => {}
                _ = &mut driver => {}
                () = tokio::time::sleep(GOAWAY_GRACE) => {}
            }
            session.abort(moq_net::Error::Unauthorized);
            transport.close(carrier::CLOSE_REVOKED, b"authority lapsed");
        } else {
            session.abort(moq_net::Error::Cancel);
        }
        drop(forwarder);
    }
}

/// Isolated loopback voice relay with a generated short-lived certificate,
/// for device-free native pipeline tests and proofs. It never
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
    ///
    /// # Errors
    ///
    /// Returns a message when the loopback server cannot be configured or bound.
    pub fn start(authority: Arc<dyn RelayAuthority>) -> Result<Self, String> {
        let listener = carrier::loopback()?;
        let relay = Relay::new(authority);
        let task = tokio::spawn(relay.clone().serve(listener.endpoint));
        Ok(Self {
            endpoint: format!("https://127.0.0.1:{}{VOICE_PATH}", listener.port),
            fingerprints: listener.fingerprints,
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

/// The next datagram or group of a subscribed track.
enum Next {
    Datagram(moq_net::Datagram),
    Group(Box<moq_net::group::Consumer>),
    End,
}

async fn next(track: &mut moq_net::track::Subscriber) -> Next {
    moq_net::kio::wait(|waiter| {
        if let Poll::Ready(datagram) = track.poll_recv_datagram(waiter) {
            return Poll::Ready(match datagram {
                Ok(Some(datagram)) => Next::Datagram(datagram),
                _ => Next::End,
            });
        }
        if let Poll::Ready(group) = track.poll_recv_group(waiter) {
            return Poll::Ready(match group {
                Ok(Some(group)) => Next::Group(Box::new(group)),
                _ => Next::End,
            });
        }
        Poll::Pending
    })
    .await
}

/// Copy one publisher's voice broadcast from its private ingress into the
/// shared origin for as long as it is announced there. Every datagram bundle
/// and object is validated before any subscriber can observe it.
async fn forward(
    shared: moq_net::origin::Producer,
    ingress: moq_net::origin::Consumer,
    name: String,
    counters: Arc<Counters>,
) {
    loop {
        let Ok(source) = ingress.routed_broadcast(name.as_str()).await else {
            return;
        };
        let Ok(out) = shared.create_broadcast(name.as_str()) else {
            return;
        };
        let Ok(track) = out.create_track(
            VOICE_TRACK,
            moq_net::track::Info::default().with_max_age(VOICE_RETAIN),
        ) else {
            return;
        };
        if out.announce(moq_net::origin::Route::default()).is_err() {
            return;
        }
        let subscription = moq_net::track::Subscription::default().with_max_age(VOICE_RETAIN);
        let input = match source.track(VOICE_TRACK) {
            Ok(input) => input.subscribe(subscription).await.ok(),
            Err(_) => None,
        };
        if let Some(input) = input {
            copy_voice(input, track, &counters).await;
        } else {
            let _ = source.closed().await;
            let _ = track.abort(moq_net::Error::Cancel);
        }
        out.finish();
        // A publisher that ends and re-announces is picked up afresh.
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn copy_voice(
    mut input: moq_net::track::Subscriber,
    mut output: moq_net::track::Producer,
    counters: &Arc<Counters>,
) {
    let mut groups: VecDeque<crate::Task> = VecDeque::new();
    loop {
        match next(&mut input).await {
            Next::Datagram(datagram) => {
                let valid = voice::validate_bundle(&datagram.payload).is_ok()
                    && output
                        .insert_datagram(datagram.sequence, datagram.timestamp, datagram.payload)
                        .is_ok();
                let counter = if valid {
                    &counters.forwarded
                } else {
                    &counters.dropped
                };
                counter.fetch_add(1, Ordering::Relaxed);
            }
            Next::Group(group) => {
                groups.retain(|task| !task.is_finished());
                if groups.len() >= MAX_GROUPS_IN_FLIGHT {
                    // A lossy publisher keeps earlier streams open while newer
                    // ones arrive: the oldest yields, the session stays up.
                    groups.pop_front();
                    counters.dropped.fetch_add(1, Ordering::Relaxed);
                }
                let Ok(out) = output.create_group(moq_net::group::Info::from(group.sequence))
                else {
                    counters.dropped.fetch_add(1, Ordering::Relaxed);
                    continue;
                };
                groups.push_back(crate::Task::spawn(copy_voice_group(
                    *group,
                    out,
                    counters.clone(),
                )));
            }
            Next::End => break,
        }
    }
    drop(groups);
    let _ = output.abort(moq_net::Error::Cancel);
}

async fn copy_voice_group(
    mut input: moq_net::group::Consumer,
    mut output: moq_net::group::Producer,
    counters: Arc<Counters>,
) {
    let mut count: u64 = 0;
    loop {
        match input.read_frame().await {
            Ok(Some(frame)) => {
                count = count.saturating_add(1);
                if count > voice::FRAMES_PER_GROUP
                    || voice::validate(&frame.payload).is_err()
                    || output.write_frame(frame.timestamp, frame.payload).is_err()
                {
                    counters.dropped.fetch_add(1, Ordering::Relaxed);
                    let _ = output.abort(moq_net::Error::ProtocolViolation);
                    return;
                }
                counters.forwarded.fetch_add(1, Ordering::Relaxed);
            }
            Ok(None) => {
                let _ = output.finish();
                return;
            }
            Err(_) => {
                let _ = output.abort(moq_net::Error::Cancel);
                return;
            }
        }
    }
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
        valid_namespace(namespace)
            && !subscriptions
                .get(..index)
                .is_some_and(|prior| prior.contains(namespace))
    })
}

fn scope_status(authorization: &dyn SessionAuthorization) -> AuthorizationStatus {
    authorization.status_all()
}

/// Denial dominates.
fn merge_status(a: AuthorizationStatus, b: AuthorizationStatus) -> AuthorizationStatus {
    match (a, b) {
        (AuthorizationStatus::Current, AuthorizationStatus::Current) => {
            AuthorizationStatus::Current
        }
        _ => AuthorizationStatus::Denied,
    }
}

fn combine_scope_status<A: SessionAuthorization + ?Sized>(
    authorization: &A,
) -> AuthorizationStatus {
    let publication = authorization
        .publication()
        .map(|name| (Direction::Publish, name));
    let subscriptions = authorization
        .subscriptions()
        .iter()
        .map(|name| (Direction::Subscribe, name.as_str()));
    for (direction, namespace) in publication.into_iter().chain(subscriptions) {
        if authorization.status(direction, namespace) == AuthorizationStatus::Denied {
            return AuthorizationStatus::Denied;
        }
    }
    AuthorizationStatus::Current
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
                "call/two" if self.denied => AuthorizationStatus::Denied,
                _ => AuthorizationStatus::Current,
            }
        }
    }

    #[test]
    fn default_aggregate_denies_on_any_namespace_denial() {
        let mut owner = PerScope {
            names: vec!["call/one".into(), "call/two".into()],
            denied: false,
        };
        assert!(matches!(scope_status(&owner), AuthorizationStatus::Current));
        owner.denied = true;
        assert!(matches!(scope_status(&owner), AuthorizationStatus::Denied));
        owner.names.clear();
        assert!(matches!(scope_status(&owner), AuthorizationStatus::Current));
    }
}
