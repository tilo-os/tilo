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
//! Its picture tracks (tilo-video/1: a camera or screen in one resolution
//! layer, [`crate::video`]) are copied the same way, each only while some
//! subscriber asks for it, so a publisher sends, and the relay forwards, only
//! the layers someone shows.
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
use crate::plane::{Next, next};
use crate::voice;

/// Sessions still being admitted at once. An admitted session is bounded by
/// the authority that admitted it (one per participation of a member
/// device), never by a count here, so a call has no participant limit.
pub const MAX_ADMITTING: usize = 16;
pub const MAX_AUTHORIZATION_BYTES: usize = carrier::MAX_GRANT_BYTES;
/// The one track a voice broadcast carries.
pub const VOICE_TRACK: &str = "audio";
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
/// Retention of the plane's opaque stream tracks, as
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
/// proved. Every device reaches the plane over iroh, a page included.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Peer(pub [u8; 32]);

/// Admission runs before any origin is attached. The opaque proof is passed
/// only here and is never included in errors, URLs, tracing or counters.
pub trait RelayAuthority: Send + Sync {
    fn admit(&self, peer: Peer, proof: &[u8]) -> Result<Arc<dyn SessionAuthorization>, Denied>;
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
    admitting: Arc<tokio::sync::Semaphore>,
    publications: Publications,
    _origin: Arc<carrier::OriginTask>,
}

struct Active(Arc<Counters>);
impl Drop for Active {
    fn drop(&mut self) {
        self.0.active.fetch_sub(1, Ordering::Relaxed);
    }
}

/// Another relay's shared origin, kept in this relay's: every broadcast the
/// other relay announces, republished here and only while someone here asks
/// for it.
///
/// The members of one workspace can end up on different relays — the first
/// relay one device reaches is not the first another reaches — and a member
/// that landed here hears every member of the workspace, not only those that
/// landed here too. What crosses is sealed end to end: a bridge copies
/// objects it cannot open, like any relay between them.
pub struct Bridge {
    _watch: crate::Task,
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
            admitting: Arc::new(tokio::sync::Semaphore::new(MAX_ADMITTING)),
            publications: Arc::default(),
            _origin: Arc::new(task),
        }
    }

    /// The origin every admitted session publishes into and hears from: what
    /// a plane on this device's own session reads and writes, so the device
    /// that relays a workspace takes part in it without connecting to
    /// itself.
    #[must_use]
    pub fn shared(&self) -> moq_net::origin::Producer {
        self.origin.clone()
    }

    /// Carry what another relay of this workspace serves into this one, so a
    /// member that reached a different relay is heard here too. `source` is
    /// what this relay reads from the other one; the bytes are sealed end to
    /// end, so nothing is opened here. Dropping the answer stops the bridge.
    pub fn bridge(&self, source: moq_net::origin::Consumer) -> Bridge {
        Bridge {
            _watch: crate::Task::spawn(crate::mirror::watch(source, self.origin.clone(), None)),
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

    /// Serve one session answered with [`carrier::PROTOCOL`].
    ///
    /// This runs the admission preface, attaches only host-owned scopes, and
    /// keeps the `MoQ` driver in this future: it is cancelled on any
    /// authorization failure or caller cancellation and never detached.
    /// `peer` is who the transport proved is asking; the authority sees it
    /// beside the preface's own proof.
    pub async fn serve_session<S: carrier::Carrier>(&self, transport: S, peer: Peer) {
        let Ok(admitting) = self.admitting.clone().try_acquire_owned() else {
            self.counters.rejected.fetch_add(1, Ordering::Relaxed);
            carrier::close(&transport, carrier::CLOSE_DENIED, "relay busy");
            return;
        };
        let accepted = carrier::accept(transport, |proof| async move {
            let mut scopes = self.scopes(peer, &proof)?;
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
                );
                server = server.with_publisher(view.consume());
                scopes.view = Some(view);
            }
            Some((server, scopes))
        })
        .await;
        drop(admitting);
        let Ok((accepted, scopes)) = accepted else {
            self.counters.rejected.fetch_add(1, Ordering::Relaxed);
            return;
        };
        self.run_session(accepted, scopes).await;
    }

    fn scopes(&self, peer: Peer, proof: &[u8]) -> Option<RelayScopes> {
        if proof.is_empty() || proof.len() > MAX_AUTHORIZATION_BYTES {
            return None;
        }
        let authorization = self.authority.admit(peer, proof).ok()?;
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

    async fn run_session<S: carrier::Carrier>(
        &self,
        accepted: carrier::Accepted<S>,
        scopes: RelayScopes,
    ) {
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

/// Copy one publisher's voice broadcast from its private ingress into the
/// shared origin for as long as it is announced there. Every datagram bundle
/// and object is validated before any subscriber can observe it.
///
/// The plane's opaque tracks (presence, activity) are copied beside it: they
/// carry the scope's own sealed bytes, so the relay bounds their size and
/// copies them unread.
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
        let demands = crate::Task::spawn(forward_demands(
            out.dynamic(),
            source.clone(),
            counters.clone(),
        ));
        let mut opaque = Vec::new();
        for plane_track in [crate::live::Track::Presence, crate::live::Track::Activity] {
            let name = crate::live::track(plane_track);
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
        let subscription = moq_net::track::Subscription::default().with_max_age(VOICE_RETAIN);
        let input = match source.track(VOICE_TRACK) {
            Ok(input) => input.subscribe(subscription).await.ok(),
            Err(_) => None,
        };
        if let Some(input) = input {
            copy_checked(input, track, &counters, voice_bounds()).await;
        } else {
            let _ = source.closed().await;
            let _ = track.abort(moq_net::Error::Cancel);
        }
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

/// The plane's opaque tracks (presence, activity): the scope's own sealed
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

/// Copy one opaque plane track (presence, activity) unread ([`opaque_bounds`]).
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

/// Serve the tracks subscribers ask of one publication — a call's picture
/// layers, a publisher's viewed windows and a window's page — each copied
/// from the publisher, validated, only while someone asks for it. Any other
/// track name is refused.
async fn forward_demands(
    mut requests: moq_net::broadcast::Dynamic,
    source: moq_net::broadcast::Consumer,
    counters: Arc<Counters>,
) {
    let mut pictures: Vec<crate::Task> = Vec::new();
    let mut windows: Vec<crate::Task> = Vec::new();
    let mut pages: Vec<crate::Task> = Vec::new();
    while let Ok(request) = requests.requested_track().await {
        pictures.retain(|task| !task.is_finished());
        windows.retain(|task| !task.is_finished());
        pages.retain(|task| !task.is_finished());
        let Some(track) = crate::live::parse_track(request.name()) else {
            request.reject(moq_net::Error::NotFound);
            continue;
        };
        // A call's layers, a publisher's viewed windows and its pages are
        // bounded apart: a window viewer never takes a call's picture slots.
        let (tasks, bound, bounds) = match track {
            crate::live::Track::Camera(layer) | crate::live::Track::Screen(layer) => {
                (&mut pictures, MAX_PICTURE_TRACKS, picture_bounds(layer))
            }
            crate::live::Track::Window(_, layer) => (
                &mut windows,
                crate::video::MAX_WINDOW_TRACKS,
                picture_bounds(layer),
            ),
            crate::live::Track::Page(_) => (&mut pages, crate::page::MAX_TRACKS, page_bounds()),
            _ => {
                request.reject(moq_net::Error::NotFound);
                continue;
            }
        };
        if tasks.len() >= bound {
            request.reject(moq_net::Error::BoundsExceeded(moq_net::BoundsExceeded));
            continue;
        }
        tasks.push(crate::Task::spawn(forward_demand(
            request,
            source.clone(),
            bounds,
            counters.clone(),
        )));
    }
}

/// Copy one demanded track for as long as a subscriber asks for it.
async fn forward_demand(
    request: moq_net::track::Request,
    source: moq_net::broadcast::Consumer,
    bounds: Arc<Bounds>,
    counters: Arc<Counters>,
) {
    let subscription = moq_net::track::Subscription::default().with_max_age(PICTURE_RETAIN);
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
    let output = request.accept(moq_net::track::Info::default().with_max_age(PICTURE_RETAIN));
    let watched = output.clone();
    tokio::select! {
        () = copy_checked(input, output, &counters, bounds) => {}
        // Nobody shows this track any more: stop asking the publisher for it.
        _ = watched.unused() => {}
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
        // live plane's `scope/participant/device`).
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

    #[test]
    fn one_owner_turn_observes_every_scope() {
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
