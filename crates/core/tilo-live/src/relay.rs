//! Host-injected authority for the `moq-lite` voice relay.
//!
//! The protocol does not mint workspace permissions. The native host supplies
//! an implementation whose private session evidence comes from its verified
//! grant/challenge service and current committed workspace authority. The grant
//! arrives in the carrier's admission preface ([`crate::carrier`]).
//!
//! Every admitted publisher writes into a private ingress origin scoped to its
//! exact namespace. The relay copies its call's voice into the shared origin,
//! validating every object and datagram bundle and bounding the groups in
//! flight, so subscribers only ever observe validated tilo-voice/1 objects.
//! Its picture tracks (tilo-video/1: a camera or screen in one resolution
//! layer, [`crate::video`]) are copied the same way. Each is copied only while
//! some subscriber asks for it, so a publisher sends, and the relay forwards,
//! only what someone hears or shows. A call's media names its call
//! ([`crate::live::call_track`]), and the relay copies it from a publisher, and
//! serves it to a subscriber, only while that call's rule lets each
//! ([`SessionAuthorization::call`]).
//! Every session is served from its own host-owned view of the shared origin
//! (`crate::mirror`), which revocation drops before anything else.

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::sync::{
    Arc, Mutex, Weak,
    atomic::{AtomicU64, Ordering},
};
use std::time::{Duration, Instant};

use crate::authority::{AuthorityDriver, Verdict};
use crate::carrier;
pub use crate::carrier::{HANDSHAKE_BUDGET, SETUP_BUDGET};
use crate::live_session::{Next, next};
use crate::voice;

/// Sessions still being admitted at once: what a relay holds for peers it has
/// not judged yet, each for no longer than its preface, its denial and its
/// setup may take. One more is closed as busy ([`carrier::CLOSE_BUSY`]) with
/// nothing of it read, which is no answer about its grant, and is judged when
/// it asks again. An admitted session is bounded by the authority that
/// admitted it (one per participation of a member device), never by a count
/// here, so a call has no device limit.
///
/// These places are for endpoints a roster names, and for every endpoint of
/// an authority that cannot tell ([`RelayAuthority::stranger`]). Anyone who
/// reaches a relay can open sessions and say nothing, and each is held for
/// as long as a preface may take: endpoints no roster names wait in places of
/// their own ([`MAX_ADMITTING_STRANGERS`]), so they cannot keep a workspace's
/// devices out, and no endpoint holds more than [`MAX_ADMITTING_OF_ONE`].
pub const MAX_ADMITTING: usize = 16;
/// Sessions of endpoints no roster names that are still being admitted at
/// once. They are judged like any other, by their grant: a page that dials
/// under a key its device signed for, and a device a workspace took in a
/// moment ago, come in through these. Whoever holds them all holds none of
/// [`MAX_ADMITTING`].
pub const MAX_ADMITTING_STRANGERS: usize = 4;
/// Sessions of one endpoint still being admitted at once: a device asks for
/// the live session of each workspace one relay carries for it, so it may hold a
/// few, and never enough to take every place.
pub const MAX_ADMITTING_OF_ONE: usize = 4;
pub const MAX_AUTHORIZATION_BYTES: usize = carrier::MAX_GRANT_BYTES;
/// Voice tracks of one publisher served at once: the call it is in, and the
/// one it is leaving.
const MAX_VOICE_TRACKS: usize = 2;
/// Longest an offered namespace whose publisher authority lapsed may stay
/// offered to a subscriber before that subscriber fails closed.
const LAPSED_OFFER_FENCE: Duration = Duration::from_millis(900);
/// How long a revoked or expired session may stay open after the revocation
/// notice so the client closes first, rather than having the session cut
/// under it with streams in flight. No media flows
/// to or from the session during this grace: its view and its publication are
/// dropped when authority is lost.
pub const GOAWAY_GRACE: Duration = Duration::from_secs(1);
/// Voice retention: late audio is dropped, never queued.
const VOICE_RETAIN: Duration = Duration::from_millis(200);
/// Picture retention: a late subscriber starts at the newest keyframe group,
/// and a picture older than this is not worth showing.
const PICTURE_RETAIN: Duration = Duration::from_secs(2);
/// Retention of the live session's opaque stream tracks, as
/// [`crate::live::Delivery::Stream`] gives it: a presence record older than
/// this says nothing about who is here now.
const OPAQUE_RETAIN: Duration = Duration::from_secs(30);
/// Bytes one publisher may have cached at the relay's ingress: its voice and
/// the picture layers asked of it, a [`PICTURE_RETAIN`] of each.
const INGRESS_BYTES: u64 = 2 * 1024 * 1024;
/// Bytes cached across every forwarded track of the relay.
const SHARED_BYTES: u64 = 64 * 1024 * 1024;
/// Bytes one subscriber's own view may cache: the voices and the pictures it
/// shows.
const VIEW_BYTES: u64 = 4 * 1024 * 1024;
/// Picture tracks of one publisher served at once: each source's layers.
const MAX_PICTURE_TRACKS: usize = 5;
/// Groups copied concurrently per publisher; a newer group evicts the oldest.
const MAX_GROUPS_IN_FLIGHT: usize = 3;
/// Bytes one opaque object may have: a presence record or an activity note.
const MAX_OPAQUE_OBJECT_BYTES: usize = 4096;
/// Objects one opaque group may have.
const MAX_OPAQUE_GROUP_OBJECTS: u64 = 16;

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
    /// on every change, so a roster change needs no new session.
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
    /// Told whenever this session's authority is observed while its connection
    /// is open (its idle timeout ends one whose device went away): the owner
    /// may count it as the holder being present. The default ignores it.
    fn connected(&self) {}

    /// Whether this session may now send (`Publish`) or be sent (`Subscribe`)
    /// the media of the call `call` tags ([`crate::live::call_track`]): what
    /// the call's rule lets this session's person do, whatever its presence
    /// says. Asked whenever a call track is asked for, and again on every
    /// change ([`Self::changed`]): a call track it no longer lets through stops
    /// at once. The default lets every call through.
    fn call(&self, direction: Direction, call: &str) -> bool {
        let _ = (direction, call);
        true
    }

    /// The change signal of everything [`status_all`](Self::status_all) reads:
    /// a membership change, a roster change, a new channel epoch. The relay
    /// re-observes on each change and never on a timer, so a removal cuts when
    /// its event arrives. `None` for an authority that cannot change while the
    /// connection lives, which is never re-observed.
    fn changed(&self) -> Option<tokio::sync::watch::Receiver<u64>> {
        None
    }
}

/// Who is asking to be admitted: the iroh endpoint id the QUIC/TLS handshake
/// proved. Every device reaches the live session over iroh, a page included.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Peer(pub [u8; 32]);

/// What an authority answers a session's admission with, once it has read
/// what it decides on.
pub type Admission<'a> = std::pin::Pin<
    Box<
        dyn std::future::Future<Output = Result<Arc<dyn SessionAuthorization>, Denied>> + Send + 'a,
    >,
>;

/// Admission runs before any origin is attached. The opaque proof is passed
/// only here and is never included in errors, URLs, tracing or counters.
///
/// An authority may wait for what it decides on (a workspace's governance
/// another task is changing): the session waits with it, and is admitted or
/// refused on that state, never refused because it was busy.
pub trait RelayAuthority: Send + Sync {
    fn admit<'a>(&'a self, peer: Peer, proof: &'a [u8]) -> Admission<'a>;

    /// Whether `peer` is an endpoint none of the rosters this authority
    /// admits from names now: told from the key its handshake proved alone,
    /// before anything of its session is read.
    ///
    /// It decides nothing about who is admitted, which stays
    /// [`Self::admit`]'s word for every session. It decides where a session
    /// waits while it is admitted: a stranger's in
    /// [`MAX_ADMITTING_STRANGERS`], so that endpoints nobody knows cannot hold
    /// the places a workspace's own devices are admitted through. The default
    /// knows no roster and calls nobody a stranger: every session of such an
    /// authority shares [`MAX_ADMITTING`].
    fn stranger(&self, peer: Peer) -> bool {
        let _ = peer;
        false
    }
}

#[derive(Clone, Copy, Debug, thiserror::Error)]
#[error("media admission denied")]
pub struct Denied;

#[derive(Default)]
struct Counters {
    accepted: AtomicU64,
    rejected: AtomicU64,
    busy: AtomicU64,
    expired: AtomicU64,
    active: AtomicU64,
    lapsed: AtomicU64,
    forwarded: AtomicU64,
    dropped: AtomicU64,
    max_tick_gap_ms: AtomicU64,
    max_observe_ms: AtomicU64,
}

/// Content-free lifecycle counters. These are not packet or audio-quality
/// measurements, and never expose the proof or device namespace.
/// `rejected` counts sessions that were judged and not admitted, or that sent
/// no usable preface; `busy` counts the ones closed unread because the places
/// of their kind in admission were taken, or their endpoint held as many as
/// one may ([`MAX_ADMITTING`]), which is nobody's refusal.
/// `lapsed` counts publications closed because a subscriber observed their
/// publisher authority lapse. `forwarded` and `dropped` count
/// objects and bundles the relay copied or refused. The two maxima
/// (milliseconds) diagnose scheduler stalls versus slow owner observations.
#[derive(Clone, Copy, Debug, Default, serde::Serialize)]
pub struct CounterSnapshot {
    pub accepted: u64,
    pub rejected: u64,
    pub busy: u64,
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
#[derive(Clone, Default)]
struct Publications(Arc<Mutex<HashMap<String, Vec<Weak<Revocation>>>>>);

impl Publications {
    /// Register a session publishing `name`, waking every older session that publishes it.
    ///
    /// One publication is served by one session, its newest: a device that connects again takes
    /// the place of the session it left, as its newest connection is the one it publishes on.
    /// Left both live, the newer one's broadcast shadows the older's, and a subscriber of the
    /// device hears the older one's cached group and then silence.
    fn register(&self, name: &str) -> Arc<Revocation> {
        let revocation = Arc::new(Revocation::default());
        let Ok(mut index) = self.0.lock() else {
            return revocation;
        };
        let entries = index.entry(name.to_owned()).or_default();
        entries.retain(|entry| entry.strong_count() > 0);
        for older in entries.drain(..).filter_map(|entry| entry.upgrade()) {
            older.0.notify_one();
        }
        entries.push(Arc::downgrade(&revocation));
        revocation
    }

    /// Wake every session publishing `name`, that each closes its publication.
    fn revoke(&self, name: &str) {
        let Ok(index) = self.0.lock() else {
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

    /// Forget `revocation`'s session, which published `name` and is ending.
    fn forget(&self, name: &str, revocation: &Arc<Revocation>) {
        let Ok(mut index) = self.0.lock() else {
            return;
        };
        if let Some(entries) = index.get_mut(name) {
            entries.retain(|entry| {
                entry.strong_count() > 0 && !std::ptr::eq(entry.as_ptr(), Arc::as_ptr(revocation))
            });
            if entries.is_empty() {
                index.remove(name);
            }
        }
    }
}

struct Publication {
    index: Publications,
    name: String,
    revocation: Arc<Revocation>,
}
impl Drop for Publication {
    fn drop(&mut self) {
        self.index.forget(&self.name, &self.revocation);
    }
}

/// The sessions a relay is admitting now: how many places are left of each
/// kind, and how many each endpoint holds.
struct Admitting {
    /// Places for endpoints a roster names, and for every endpoint of an
    /// authority that cannot tell.
    named: Arc<tokio::sync::Semaphore>,
    /// Places for endpoints no roster names.
    strangers: Arc<tokio::sync::Semaphore>,
    /// The places each endpoint holds now.
    held: Arc<Mutex<BTreeMap<[u8; 32], usize>>>,
}

/// One session's place in admission, given back when it is dropped.
struct Place {
    _permit: tokio::sync::OwnedSemaphorePermit,
    held: Arc<Mutex<BTreeMap<[u8; 32], usize>>>,
    endpoint: [u8; 32],
}

impl Drop for Place {
    fn drop(&mut self) {
        let mut held = self
            .held
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(places) = held.get_mut(&self.endpoint) {
            *places = places.saturating_sub(1);
            if *places == 0 {
                held.remove(&self.endpoint);
            }
        }
    }
}

impl Admitting {
    fn new() -> Self {
        Self {
            named: Arc::new(tokio::sync::Semaphore::new(MAX_ADMITTING)),
            strangers: Arc::new(tokio::sync::Semaphore::new(MAX_ADMITTING_STRANGERS)),
            held: Arc::default(),
        }
    }

    /// A place for one more session of `peer`, a `stranger` or not: `None`
    /// when its endpoint holds as many as one may, or the places of its kind
    /// are taken.
    fn place(&self, peer: Peer, stranger: bool) -> Option<Place> {
        let Peer(endpoint) = peer;
        let mut held = self
            .held
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let of_one = held.get(&endpoint).copied().unwrap_or(0);
        if of_one >= MAX_ADMITTING_OF_ONE {
            return None;
        }
        let places = if stranger {
            &self.strangers
        } else {
            &self.named
        };
        let permit = Arc::clone(places).try_acquire_owned().ok()?;
        held.insert(endpoint, of_one.saturating_add(1));
        drop(held);
        Some(Place {
            _permit: permit,
            held: Arc::clone(&self.held),
            endpoint,
        })
    }
}

#[derive(Clone)]
pub struct Relay {
    authority: Arc<dyn RelayAuthority>,
    origin: moq_net::origin::Producer,
    counters: Arc<Counters>,
    admitting: Arc<Admitting>,
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

pub(crate) use crate::live::exact;

impl Relay {
    /// Must be called inside a Tokio runtime: the shared origin's driver runs
    /// on it for as long as any clone of the relay lives.
    pub fn new(authority: Arc<dyn RelayAuthority>) -> Self {
        let (origin, task) =
            carrier::OriginTask::produce(carrier::origin_config(SHARED_BYTES, PICTURE_RETAIN));
        Self {
            authority,
            origin,
            counters: Arc::default(),
            admitting: Arc::new(Admitting::new()),
            publications: Publications::default(),
            _origin: Arc::new(task),
        }
    }

    /// The origin every admitted session publishes into and hears from: what
    /// a live session on this device's own session reads and writes, so the device
    /// that relays a workspace takes part in it without connecting to
    /// itself.
    #[must_use]
    pub fn shared(&self) -> moq_net::origin::Producer {
        self.origin.clone()
    }

    pub fn counters(&self) -> CounterSnapshot {
        let load = |value: &AtomicU64| value.load(Ordering::Relaxed);
        let c = &self.counters;
        CounterSnapshot {
            accepted: load(&c.accepted),
            rejected: load(&c.rejected),
            busy: load(&c.busy),
            expired: load(&c.expired),
            active: load(&c.active),
            lapsed: load(&c.lapsed),
            forwarded: load(&c.forwarded),
            dropped: load(&c.dropped),
            max_tick_gap_ms: load(&c.max_tick_gap_ms),
            max_observe_ms: load(&c.max_observe_ms),
        }
    }

    /// Serve one session answered with [`carrier::PROTOCOL`].
    ///
    /// This runs the admission preface, attaches only host-owned scopes, and
    /// keeps the `MoQ` driver in this future: it is cancelled on any
    /// authorization failure or caller cancellation and never detached.
    /// `peer` is who the transport proved is asking; the authority sees it
    /// beside the preface's own proof.
    pub async fn serve_session<S: carrier::Carrier>(&self, transport: S, peer: Peer) {
        self.serve_session_then(transport, peer, || {}).await;
    }

    /// Serve one session as [`Self::serve_session`] does, and run `admitted`
    /// once the authority admitted it, before the session is told: where its
    /// carrier allows it the streams an admitted session opens
    /// ([`crate::native::serve_on`]), which a session nobody judged is not
    /// allowed.
    pub async fn serve_session_then<S: carrier::Carrier>(
        &self,
        transport: S,
        peer: Peer,
        admitted: impl FnOnce() + Send,
    ) {
        // Which places the session waits in is told from the key its
        // handshake proved, before anything of it is read.
        let stranger = self.authority.stranger(peer);
        let Some(admitting) = self.admitting.place(peer, stranger) else {
            // Nothing of the session was read and its peer was not judged: the
            // close says busy and never refused, so a member that arrived in a
            // burst asks again, and who is refused stays the authority's word.
            self.counters.busy.fetch_add(1, Ordering::Relaxed);
            carrier::close(&transport, carrier::CLOSE_BUSY, "relay busy");
            return;
        };
        let admit = |proof: Vec<u8>| async move {
            let mut scopes = self.scopes(peer, &proof).await?;
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
                    PICTURE_RETAIN,
                    Some(scopes.downlink.clone()),
                    Some(Arc::clone(&scopes.authorization)),
                );
                server = server.with_publisher(view.consume());
                scopes.view = Some(view);
            }
            Some((server, scopes))
        };
        let accepted = carrier::accept(transport, admit, admitted).await;
        drop(admitting);
        let Ok((accepted, scopes)) = accepted else {
            self.counters.rejected.fetch_add(1, Ordering::Relaxed);
            return;
        };
        self.run_session(accepted, scopes).await;
    }

    async fn scopes(&self, peer: Peer, proof: &[u8]) -> Option<RelayScopes> {
        if proof.is_empty() || proof.len() > MAX_AUTHORIZATION_BYTES {
            return None;
        }
        let authorization = self.authority.admit(peer, proof).await.ok()?;
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
        let authority = AuthorityDriver::admit(
            self.observe(authorization.as_ref(), offered.as_mut()),
            authorization.changed(),
        )?;
        let ingress = authorization.publication().map(|_| {
            let (origin, task) =
                carrier::OriginTask::produce(carrier::origin_config(INGRESS_BYTES, PICTURE_RETAIN));
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
        };
        let elapsed = started.elapsed().as_millis().try_into().unwrap_or(u64::MAX);
        self.counters
            .max_observe_ms
            .fetch_max(elapsed, Ordering::Relaxed);
        status
    }

    fn register_publication(&self, name: &str) -> Publication {
        Publication {
            index: self.publications.clone(),
            name: name.to_owned(),
            revocation: self.publications.register(name),
        }
    }

    /// Close every live session publishing exactly `name`.
    fn revoke_publication(&self, name: &str) {
        self.publications.revoke(name);
    }

    async fn run_session<S: carrier::Carrier>(
        &self,
        accepted: carrier::Accepted<S>,
        scopes: RelayScopes,
    ) {
        let carrier::Accepted {
            session,
            driver,
            pace,
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
        // connection measures.
        downlink.attach(carrier::wire(&transport));
        // Scopes were observed Current during admission; the loop below
        // re-observes them when the authority's own source reports a change.
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
                Arc::clone(&authorization),
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
        let driver = pace.drive(driver);
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
                    authorization.connected();
                    self.observe(authorization.as_ref(), offered.as_mut())
                }) => match verdict {
                    Verdict::Continue => {}
                    Verdict::Revoked => break true,
                },
            }
        };
        if lapsed {
            // Media stops at once in both directions: the session's own view
            // (every track it is served) and its publication are dropped
            // before the lapse counts and the client is told to close. The
            // grace below only covers the transport teardown.
            view.take();
            forwarder.take();
            self.counters.expired.fetch_add(1, Ordering::Relaxed);
            admission.revoke().await;
            tokio::select! {
                _ = session.closed() => {}
                _ = &mut driver => {}
                () = tokio::time::sleep(GOAWAY_GRACE) => {}
            }
            session.abort(moq_net::Error::Unauthorized);
            carrier::close(&transport, carrier::CLOSE_REVOKED, "authority lapsed");
        } else {
            session.abort(moq_net::Error::Cancel);
        }
        drop(forwarder);
    }
}

/// Copy one publisher's broadcast from its private ingress into the shared
/// origin for as long as it is announced there: its opaque tracks at once, and
/// every other track only while a subscriber asks for it ([`forward_demands`]).
/// Every datagram bundle and object is validated before any subscriber can
/// observe it.
///
/// The live session's opaque tracks (presence, activity) carry the scope's own
/// sealed bytes, so the relay bounds their size and copies them unread.
async fn forward(
    shared: moq_net::origin::Producer,
    ingress: moq_net::origin::Consumer,
    name: String,
    counters: Arc<Counters>,
    authorization: Arc<dyn SessionAuthorization>,
) {
    loop {
        let Ok(source) = ingress.routed_broadcast(name.as_str()).await else {
            return;
        };
        let Ok(out) = shared.create_broadcast(name.as_str()) else {
            return;
        };
        let demands = crate::Task::spawn(forward_demands(
            out.dynamic(),
            source.clone(),
            counters.clone(),
            Arc::clone(&authorization),
        ));
        let mut opaque = Vec::new();
        for live_session_track in [crate::live::Track::Presence, crate::live::Track::Activity] {
            let name = crate::live::track(live_session_track);
            let Ok(output) = out.create_track(
                name.as_str(),
                moq_net::track::Info::default().with_max_age(OPAQUE_RETAIN),
            ) else {
                continue;
            };
            opaque.push(crate::Task::spawn(copy_opaque(
                source.clone(),
                name,
                output,
                counters.clone(),
            )));
        }
        if out.announce(moq_net::origin::Route::default()).is_err() {
            return;
        }
        let _ = source.closed().await;
        drop(demands);
        drop(opaque);
        out.finish();
        // A publisher that ends and re-announces is picked up afresh.
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Whether a group's frame, at its place in the group, is one to copy.
type FrameCheck = Box<dyn Fn(u64, &[u8]) -> bool + Send + Sync>;

/// What one copied track lets through: each datagram and each group's frames
/// are checked before any subscriber can observe them, and a group carries
/// at most `frames` of them. `frame` is told each frame's place in its group.
struct Bounds {
    datagram: fn(&[u8]) -> bool,
    frame: FrameCheck,
    frames: u64,
}

/// Voice: every bundle and object validated.
fn voice_bounds() -> Arc<Bounds> {
    Arc::new(Bounds {
        datagram: |payload| voice::validate_bundle(payload).is_ok(),
        frame: Box::new(|_, payload| voice::validate(payload).is_ok()),
        frames: voice::FRAMES_PER_GROUP,
    })
}

/// The live session's opaque tracks (presence, activity): the scope's own sealed
/// bytes, copied unread, bounded in size.
fn opaque_bounds() -> Arc<Bounds> {
    Arc::new(Bounds {
        datagram: |payload| payload.len() <= MAX_OPAQUE_OBJECT_BYTES,
        frame: Box::new(|_, payload| payload.len() <= MAX_OPAQUE_OBJECT_BYTES),
        frames: MAX_OPAQUE_GROUP_OBJECTS,
    })
}

/// Pictures of `layer`, in groups only: a keyframe first, every picture of
/// the track's own layer.
fn picture_bounds(layer: crate::video::Layer) -> Arc<Bounds> {
    Arc::new(Bounds {
        datagram: |_| false,
        frame: Box::new(move |index, payload| {
            crate::video::validate(payload)
                .is_ok_and(|header| header.layer == layer && (index > 0 || header.keyframe))
        }),
        frames: crate::video::MAX_GROUP_OBJECTS,
    })
}

/// A window's page: every object a valid page object, a group entered at a
/// whole frame.
fn page_bounds() -> Arc<Bounds> {
    Arc::new(Bounds {
        datagram: |_| false,
        frame: Box::new(|index, payload| {
            crate::page::validate(payload).is_ok_and(|header| index > 0 || header.whole)
        }),
        frames: crate::page::MAX_GROUP_OBJECTS,
    })
}

/// Copy `input` into `output` within `bounds` until the input ends.
async fn copy_checked(
    mut input: moq_net::track::Subscriber,
    mut output: moq_net::track::Producer,
    counters: &Arc<Counters>,
    bounds: Arc<Bounds>,
) {
    let mut groups: VecDeque<crate::Task> = VecDeque::new();
    loop {
        match next(&mut input).await {
            Next::Datagram(datagram) => {
                let valid = (bounds.datagram)(&datagram.payload)
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
                groups.push_back(crate::Task::spawn(copy_checked_group(
                    *group,
                    out,
                    counters.clone(),
                    Arc::clone(&bounds),
                )));
            }
            Next::End => break,
        }
    }
    drop(groups);
    let _ = output.abort(moq_net::Error::Cancel);
}

async fn copy_checked_group(
    mut input: moq_net::group::Consumer,
    mut output: moq_net::group::Producer,
    counters: Arc<Counters>,
    bounds: Arc<Bounds>,
) {
    let mut count: u64 = 0;
    loop {
        match input.read_frame().await {
            Ok(Some(frame)) => {
                let valid = (bounds.frame)(count, &frame.payload);
                count = count.saturating_add(1);
                if !valid
                    || count > bounds.frames
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

/// Copy one opaque live session track (presence, activity) unread ([`opaque_bounds`]).
async fn copy_opaque(
    source: moq_net::broadcast::Consumer,
    name: String,
    output: moq_net::track::Producer,
    counters: Arc<Counters>,
) {
    let input = match source.track(name.as_str()) {
        Ok(track) => {
            track
                .subscribe(moq_net::track::Subscription::default().with_max_age(OPAQUE_RETAIN))
                .await
        }
        Err(error) => Err(error),
    };
    let Ok(input) = input else {
        // The publisher does not carry this track: announce nothing.
        let _ = output.abort(moq_net::Error::NotFound);
        return;
    };
    copy_checked(input, output, &counters, opaque_bounds()).await;
}

/// Serve the tracks subscribers ask of one publication — its call's voice and
/// picture layers, its viewed windows and a window's page — each copied from
/// the publisher, validated, only while someone asks for it. A call's track is
/// copied only while the call lets its publisher speak ([`SessionAuthorization::call`]);
/// a call's media that names no call, and any other track name, is refused.
async fn forward_demands(
    mut requests: moq_net::broadcast::Dynamic,
    source: moq_net::broadcast::Consumer,
    counters: Arc<Counters>,
    authorization: Arc<dyn SessionAuthorization>,
) {
    let mut voices: Vec<crate::Task> = Vec::new();
    let mut pictures: Vec<crate::Task> = Vec::new();
    let mut windows: Vec<crate::Task> = Vec::new();
    let mut pages: Vec<crate::Task> = Vec::new();
    while let Ok(request) = requests.requested_track().await {
        voices.retain(|task| !task.is_finished());
        pictures.retain(|task| !task.is_finished());
        windows.retain(|task| !task.is_finished());
        pages.retain(|task| !task.is_finished());
        let call = crate::live::parse_call_track(request.name());
        let Some(track) = call
            .map(|(track, _)| track)
            .or_else(|| crate::live::parse_track(request.name()))
        else {
            request.reject(moq_net::Error::NotFound);
            continue;
        };
        let call = call.map(|(_, call)| call.to_owned());
        if call
            .as_deref()
            .is_some_and(|call| !authorization.call(Direction::Publish, call))
        {
            counters.dropped.fetch_add(1, Ordering::Relaxed);
            request.reject(moq_net::Error::Unauthorized);
            continue;
        }
        // A call's voice and layers, a publisher's viewed windows and its pages
        // are bounded apart: a window viewer never takes a call's picture slots.
        let (tasks, bound, bounds, retain) = match (track, call.is_some()) {
            (crate::live::Track::Audio, true) => {
                (&mut voices, MAX_VOICE_TRACKS, voice_bounds(), VOICE_RETAIN)
            }
            (crate::live::Track::Camera(layer) | crate::live::Track::Screen(layer), true) => (
                &mut pictures,
                MAX_PICTURE_TRACKS,
                picture_bounds(layer),
                PICTURE_RETAIN,
            ),
            (crate::live::Track::Window(_, layer), false) => (
                &mut windows,
                crate::video::MAX_WINDOW_TRACKS,
                picture_bounds(layer),
                PICTURE_RETAIN,
            ),
            (crate::live::Track::Page(_), false) => (
                &mut pages,
                crate::page::MAX_TRACKS,
                page_bounds(),
                PICTURE_RETAIN,
            ),
            _ => {
                request.reject(moq_net::Error::NotFound);
                continue;
            }
        };
        if tasks.len() >= bound {
            request.reject(moq_net::Error::BoundsExceeded(moq_net::BoundsExceeded));
            continue;
        }
        let held = call.map(|call| (Arc::clone(&authorization), Direction::Publish, call));
        tasks.push(crate::Task::spawn(forward_demand(
            request,
            source.clone(),
            (bounds, retain),
            counters.clone(),
            held,
        )));
    }
}

/// A session's authority over one call track: the session, the direction it
/// is let through in, and the call.
pub(crate) type CallHeld = (Arc<dyn SessionAuthorization>, Direction, String);

/// Wait until the call `held` names no longer lets its session through: on
/// the first change that says so, or when the session's authority can no
/// longer change and so no longer be told. Never for a track that is no
/// call's.
pub(crate) async fn call_refused(held: Option<&CallHeld>) {
    let Some((authorization, direction, call)) = held else {
        return std::future::pending().await;
    };
    let Some(mut changed) = authorization.changed() else {
        return std::future::pending().await;
    };
    loop {
        if !authorization.call(*direction, call) {
            return;
        }
        if changed.changed().await.is_err() {
            return;
        }
    }
}

/// Copy one demanded track for as long as a subscriber asks for it and, for a
/// call's track, the call lets its publisher speak.
async fn forward_demand(
    request: moq_net::track::Request,
    source: moq_net::broadcast::Consumer,
    (bounds, retain): (Arc<Bounds>, Duration),
    counters: Arc<Counters>,
    held: Option<CallHeld>,
) {
    let subscription = moq_net::track::Subscription::default().with_max_age(retain);
    let input = match source.track(request.name()) {
        Ok(track) => track.subscribe(subscription).await,
        Err(error) => Err(error),
    };
    let input = match input {
        Ok(input) => input,
        Err(error) => {
            request.reject(error);
            return;
        }
    };
    let output = request.accept(moq_net::track::Info::default().with_max_age(retain));
    let watched = output.clone();
    let refused = output.clone();
    tokio::select! {
        () = copy_checked(input, output, &counters, bounds) => {}
        // Nobody shows this track any more: stop asking the publisher for it.
        _ = watched.unused() => {}
        // The call no longer lets its publisher speak: nothing more of it is copied.
        () = call_refused(held.as_ref()) => {
            let _ = refused.abort(moq_net::Error::Unauthorized);
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
        // A whole path whose every segment is checked, so the scope is exact
        // rather than a partial segment; the publication lies beneath it,
        // where the scheme's own naming puts it (a call's `call/device`, the
        // live session's `scope/device/device`).
        return subscriptions.is_empty()
            && valid_namespace(prefix)
            && publication.is_none_or(|name| {
                valid_namespace(name)
                    && name
                        .strip_prefix(prefix)
                        .and_then(|rest| rest.strip_prefix('/'))
                        .is_some_and(|rest| !rest.is_empty())
            });
    }
    // An exact subscription set is bounded by the grant's size
    // ([`MAX_AUTHORIZATION_BYTES`]), not by a count.
    if publication.is_none() && subscriptions.is_empty() {
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
            AuthorizationStatus::Denied
        }
    }

    tilo_test_env::scenario! {
        fn a_sessions_status_is_what_its_owner_says_of_every_scope;

        phase one_owner_turn_observes_every_scope;
        {
            let owner = OneTurn {
                observed: AtomicUsize::new(0),
                names: vec!["call/one".into(), "call/two".into()],
            };
            assert!(matches!(scope_status(&owner), AuthorizationStatus::Denied));
            assert_eq!(owner.observed.load(Ordering::Relaxed), 1);
        }

        phase default_aggregate_denies_on_any_namespace_denial;
        {
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
}

#[cfg(test)]
mod publication_tests {
    use super::Publications;
    use std::time::Duration;

    /// Two sessions publishing one namespace never live at once: the second takes the first's
    /// place and the first is woken to close its publication, so no session shadows another's
    /// broadcast.
    #[tokio::test]
    async fn a_second_publisher_of_a_namespace_replaces_the_first() {
        let publications = Publications::default();
        let first = publications.register("device/workspace");
        tokio::time::timeout(Duration::from_millis(50), first.0.notified())
            .await
            .expect_err("the first session hears nothing while it is the one publishing");
        let second = publications.register("device/workspace");
        tokio::time::timeout(Duration::from_secs(1), first.0.notified())
            .await
            .expect("the first session is woken to close its publication");
        // Another namespace is another publication: its session is left alone.
        let other = publications.register("device/other");
        tokio::time::timeout(Duration::from_millis(50), other.0.notified())
            .await
            .expect_err("a session of another namespace is not woken");
        // The second is the one publishing now, and it is woken to close by the relay.
        publications.revoke("device/workspace");
        tokio::time::timeout(Duration::from_secs(1), second.0.notified())
            .await
            .expect("the live session is woken to close its publication");
    }
}
