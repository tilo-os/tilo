//! Content a record references, reaching whoever may read the record.
//!
//! A document holds a reference to its binaries — a picture's content hash —
//! never the bytes. The bytes travel beside the record, on the record's own
//! route: an app hands the host content it stored (`offerBlob`), asks for
//! content its record references and it lacks (`wantBlob`), and receives
//! what arrives (`subscribeBlobs`). The host keeps the bytes in the device
//! content store, under the scope of the record that references them, and
//! asks the route's other devices for what it lacks.
//!
//! [`RouteContent`] is that host service, shared by every host that runs
//! the broker. A host supplies two things:
//!
//! - its [`ContentShelf`]: the device content store, read and written
//!   through the scope a record's content lives under ([`ContentScope`]);
//! - its [`ContentPeers`]: the record's route on the network — the workspace's
//!   topic for a workspace record, the person's own devices for a person record.
//!
//! The rules:
//!
//! - **Possessing a hash never grants access.** A device answers a peer's
//!   request only when the host says the peer may read the record's route,
//!   and only with content kept under that record's scope on this device.
//!   What a peer may read is asked again when content it asked for earlier
//!   is handed over later ([`ContentPeers::may_read`]).
//! - **Bytes are verified, never trusted.** Content travels in bounded
//!   pieces addressed by hash; the assembled bytes must hash to what was
//!   asked for before they are kept or handed to an app.
//! - **Nothing arrives unasked.** Pieces for content this device did not
//!   ask for on that route are dropped.
//! - **What was lacking follows.** A device asked for content it does not
//!   keep yet says so, and hands it to the peers that asked, and may still
//!   read the record, once it keeps it:
//!   a record can name content a moment before its writer keeps the bytes
//!   where peers are served from.
//!
//! A want is in one of three states, which its subscribers hear as they
//! change ([`ContentNews`]):
//!
//! - **fetching** ([`ContentState::Pending`]): a device that could hand the
//!   content over — a peer on the route, or the host serving it — is
//!   connected and has not said it lacks it;
//! - **waiting** ([`ContentState::Waiting`]): no peer is connected right
//!   now, and the hosts that are (if any) keep none of it;
//! - **unavailable** ([`ContentState::Unavailable`]): peers are connected,
//!   and every connected device said it does not keep it.
//!
//! A host serving the route keeps what it was given, not what its members
//! hold: its saying it lacks the content does not make it unavailable while
//! a member that might hold it is yet to connect. The members a host
//! reaches are peers of the route ([`ContentSources::peers`]).
//!
//! The state follows the connections of the devices serving the workspace,
//! never a timer: those devices say when they change
//! ([`RouteContent::sources_changed`]), and a device that connects is asked
//! for what is still wanted. A want that got no content is asked again once
//! the patience of its last answer passed ([`RouteContent::retry`],
//! [`patience_ms`]), naming the pieces it holds: a device that said it keeps
//! none may keep the bytes a moment later, and an answer carries only one
//! window of a large content at a time.

use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, Mutex};

use neighbour_wire::NeighbourMsg;
use neighbour_wire::content::{Span, WANT_TTL_MS, answer_pieces, patience_ms};
/// The route of a record whose content is asked for: its app and workspace name the scope the
/// content lives under.
pub use tilo_broker_core::route::CrdtRoute;
pub use tilo_content::ContentState;
use tilo_content::{hash_bytes, valid_hash};
use tilo_peer_state::peer_handle;
use tilo_server_protocol::WorkspaceId;
use tokio::sync::mpsc;

use crate::workspaces::{ContentKeeper, WorkspaceContent, Workspaces};

mod put;
pub use put::shelf_puts;

pub use neighbour_wire::content::{MAX_CONTENT_BYTES, PIECE_BYTES, PIECES_PER_ANSWER};

/// Requests this device keeps open at once; past it, the oldest go.
const MAX_OPEN_WANTS: usize = 256;

/// Where a record's content lives on a device: the workspace the record is
/// kept in, and the app whose record it is. A person record's content
/// lives under the person's own workspace, whichever workspace the window showing it
/// is in.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ContentScope {
    pub workspace: String,
    pub app: String,
}

impl ContentScope {
    /// The scope of the record `route` carries.
    pub fn of(route: &CrdtRoute) -> Self {
        Self {
            workspace: route.cluster_id.clone(),
            app: route.app_id.clone(),
        }
    }

    /// Whether both names are ones a device keeps content under: a plain name of at most
    /// [`MAX_SCOPE_COMPONENT`] bytes, never a path that leaves the store.
    #[must_use]
    pub fn names_a_store(&self) -> bool {
        component(&self.workspace) && component(&self.app)
    }
}

/// The longest a workspace or app name may be, as a content store names one.
const MAX_SCOPE_COMPONENT: usize = 128;

/// Whether `value` names one directory of a content store.
fn component(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_SCOPE_COMPONENT
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
        && !value.bytes().all(|byte| byte == b'.')
}

/// The device content store, as the host reaches it for records.
pub trait ContentShelf: Send + Sync {
    /// The bytes of `hash`, when this device keeps them under `scope`.
    fn read(&self, scope: &ContentScope, hash: &str) -> Option<Vec<u8>>;
    /// Keep `bytes`, whose hash is `hash`, under `scope`.
    fn keep(&self, scope: &ContentScope, hash: &str, bytes: &[u8]) -> Result<(), String>;
    /// Whether this device keeps `hash` under `scope`.
    fn has(&self, scope: &ContentScope, hash: &str) -> bool {
        self.read(scope, hash).is_some()
    }

    /// How long `hash` is under `scope`, when this device keeps it.
    fn size(&self, scope: &ContentScope, hash: &str) -> Option<u64> {
        self.read(scope, hash)
            .map(|bytes| u64::try_from(bytes.len()).unwrap_or(u64::MAX))
    }

    /// `len` bytes of `hash` from `offset`, when this device keeps them under
    /// `scope`: a holder hands over a large content a piece at a time, never
    /// the whole of it.
    fn read_range(
        &self,
        scope: &ContentScope,
        hash: &str,
        offset: u64,
        len: usize,
    ) -> Option<Vec<u8>> {
        let bytes = self.read(scope, hash)?;
        let start = usize::try_from(offset)
            .unwrap_or(usize::MAX)
            .min(bytes.len());
        let end = start.saturating_add(len).min(bytes.len());
        bytes.get(start..end).map(<[u8]>::to_vec)
    }

    /// Keep the file at `path` as content `hash`, `size` bytes long, under
    /// `scope`: a file a transfer carries whole to disk first, so the shelf
    /// holds a content larger than one read without the bytes waiting in
    /// memory. Refused when the file does not hash to `hash`.
    fn keep_file(
        &self,
        scope: &ContentScope,
        hash: &str,
        path: &std::path::Path,
        size: u64,
    ) -> Result<(), String> {
        let bytes = std::fs::read(path).map_err(|error| error.to_string())?;
        if u64::try_from(bytes.len()).unwrap_or(u64::MAX) != size
            || blake3::hash(&bytes).to_hex().as_str() != hash
        {
            return Err("the file is not the content its address names".to_owned());
        }
        self.keep(scope, hash, &bytes)
    }
}

/// One device on a record's route, as the host's network names it.
pub type ContentPeer = u64;

/// One bounded piece of content on its way to the device that asked.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ContentPiece {
    /// Hex blake3 of the whole content.
    pub hash: String,
    /// Length of the whole content.
    pub total: u64,
    /// Where these bytes start in it.
    pub offset: u64,
    pub bytes: Vec<u8>,
}

/// The record's route on the network.
pub trait ContentPeers: Send + Sync {
    /// Ask the devices on `route` for `hash`, naming `have`, the stretches of
    /// it this device holds already: an answer carries only the pieces missing.
    fn want(&self, route: &CrdtRoute, hash: &str, have: &[Span]);
    /// Send `piece` of content to `peer`, which asked for it on `route`.
    fn answer(&self, route: &CrdtRoute, peer: ContentPeer, piece: ContentPiece);
    /// Tell `peer`, which asked on `route` for `hash`, that this device
    /// keeps none of it.
    fn lacks(&self, route: &CrdtRoute, peer: ContentPeer, hash: &str);
    /// The devices connected now that could hand over `route`'s content,
    /// never this device.
    fn sources(&self, route: &CrdtRoute) -> ContentSources;

    /// Whether `peer` may read `route` now. An ask is answered on the
    /// admission the workspace gave the peer; content owed to it is handed
    /// over later, when no message of its own arrives, so the route's
    /// devices are asked again then.
    fn may_read(&self, route: &CrdtRoute, peer: ContentPeer) -> bool {
        self.sources(route).all().contains(&peer)
    }

    /// This device keeps `hash` for `route` now: `written` by an app here,
    /// rather than fetched from another device. Asks the route's devices made
    /// while it kept none are answered then, by the device that wrote it.
    fn kept(&self, route: &CrdtRoute, hash: &str, written: bool) {
        let _ = (route, hash, written);
    }
}

/// The devices connected now that could hand over a route's content.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ContentSources {
    /// The route's other devices: those connected directly, and the members
    /// a host serving the route reaches.
    pub peers: BTreeSet<ContentPeer>,
    /// The hosts serving the route, which keep what they were given.
    pub hosts: BTreeSet<ContentPeer>,
}

impl ContentSources {
    fn all(&self) -> BTreeSet<ContentPeer> {
        self.peers.union(&self.hosts).copied().collect()
    }
}

/// What a scope's subscribers hear about content: it arrived (`state` is
/// [`ContentState::Resolved`] and `bytes` are the content), or where a want
/// of it has got to (`bytes` empty).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContentNews {
    pub hash: String,
    pub state: ContentState,
    pub bytes: Vec<u8>,
}

impl ContentNews {
    fn arrived(hash: &str, bytes: &[u8]) -> Self {
        Self {
            hash: hash.to_owned(),
            state: ContentState::Resolved,
            bytes: bytes.to_vec(),
        }
    }
}

/// A request this device made and the pieces gathered for it.
struct Want {
    /// The route it was asked on, to ask devices that connect later.
    route: CrdtRoute,
    /// When it was made among the open ones: the oldest go first.
    order: u64,
    /// Connected devices it was asked of. A device that leaves is asked
    /// again when it is back.
    asked: BTreeSet<ContentPeer>,
    /// Connected devices that said they keep none of it.
    lacking: BTreeSet<ContentPeer>,
    /// Where it has got to, as its subscribers last heard.
    state: ContentState,
    /// The content as it arrives: its length once known, and which bytes
    /// are in.
    total: Option<u64>,
    bytes: Vec<u8>,
    received: u64,
    /// Offsets of the pieces gathered, so a piece two devices both sent
    /// counts once.
    pieces: std::collections::BTreeSet<u64>,
    /// When the want was made, so it stops being asked again on the pass of
    /// [`patience_ms`] after [`WANT_TTL_MS`]: whoever wants it asks afresh
    /// by then.
    wanted_ms: u64,
    /// When it was last asked of the route, and when the last piece arrived:
    /// what says whether a holder that said it keeps none has had time to
    /// keep it, and how long to wait before asking again.
    asked_ms: u64,
    last_piece_ms: u64,
    reasks: u32,
}

/// The host's record-content service: what the broker's `offerBlob`,
/// `wantBlob` and `subscribeBlobs` answer through, and where the network
/// delivers peers' requests and answers.
pub struct RouteContent {
    shelf: Arc<dyn ContentShelf>,
    peers: Arc<dyn ContentPeers>,
    subscribers: Mutex<HashMap<ContentScope, Vec<mpsc::UnboundedSender<ContentNews>>>>,
    wants: Mutex<Wants>,
}

#[derive(Default)]
struct Wants {
    open: HashMap<(ContentScope, String), Want>,
    made: u64,
    /// Content peers asked this device for while it kept none, and the peers
    /// that asked: handed to them once it is kept here ([`RouteContent::owe`]).
    owed: HashMap<(ContentScope, String), Owed>,
}

/// Content this device said it lacks to peers that may read it.
struct Owed {
    route: CrdtRoute,
    /// When it was first owed among the owed ones: the oldest go first.
    order: u64,
    peers: BTreeSet<ContentPeer>,
}

impl Want {
    /// Where a want stands with `sources` connected: fetching while one of
    /// them has not said it lacks the content; past that unavailable when
    /// peers are connected, and waiting for one when none is.
    fn current_state(&self, sources: &ContentSources) -> ContentState {
        if !sources.all().is_subset(&self.lacking) {
            ContentState::Pending
        } else if sources.peers.is_empty() {
            ContentState::Waiting
        } else {
            ContentState::Unavailable
        }
    }

    /// Start gathering afresh, handing back what was gathered.
    fn restart(&mut self) -> Vec<u8> {
        self.total = None;
        self.received = 0;
        self.pieces.clear();
        std::mem::take(&mut self.bytes)
    }

    /// Follow the devices connected now: forget what those that left said
    /// and were asked, and return whether one that connected is to be
    /// asked.
    fn follow(&mut self, sources: &ContentSources) -> bool {
        let sources = sources.all();
        self.asked.retain(|peer| sources.contains(peer));
        self.lacking.retain(|peer| sources.contains(peer));
        let new = !sources.is_subset(&self.asked);
        self.asked.extend(sources.iter().copied());
        new
    }

    /// What the route is told this device holds, so an answer carries only
    /// the pieces missing: the stretches of the content that arrived.
    fn have(&self) -> Vec<Span> {
        let Some(total) = self.total else {
            return Vec::new();
        };
        let mut spans: Vec<Span> = Vec::new();
        for offset in &self.pieces {
            let len = total
                .saturating_sub(*offset)
                .min(u64::try_from(PIECE_BYTES).unwrap_or(u64::MAX));
            match spans.last_mut() {
                Some(last) if last.offset.saturating_add(last.len) == *offset => {
                    last.len = last.len.saturating_add(len);
                }
                _ => spans.push(Span {
                    offset: *offset,
                    len,
                }),
            }
        }
        spans
    }

    /// Whether the want is to be asked again at `now_ms`: nothing arrived
    /// since it was last asked, and the patience of its last answers has
    /// passed. A holder that said it keeps none may keep it a moment later,
    /// and an answer that was lost leaves the content short until the
    /// missing pieces are asked for.
    fn due(&self, now_ms: u64) -> bool {
        now_ms.saturating_sub(self.last_piece_ms.max(self.asked_ms)) >= patience_ms(self.reasks)
    }

    /// Note that the want went out to the route at `now_ms`.
    fn asked_at(&mut self, now_ms: u64) {
        self.asked_ms = now_ms;
        self.reasks = self.reasks.saturating_add(1);
    }
}

impl RouteContent {
    pub fn new(shelf: Arc<dyn ContentShelf>, peers: Arc<dyn ContentPeers>) -> Arc<Self> {
        Arc::new(Self {
            shelf,
            peers,
            subscribers: Mutex::default(),
            wants: Mutex::default(),
        })
    }

    /// An app on `route` stored `bytes` as `hash`: keep them where the
    /// record's readers are served from, and hand them to the other local
    /// apps on the record's scope.
    ///
    /// # Errors
    /// Refuses bytes that are not `hash`, oversized content, or a store
    /// that will not keep them.
    pub fn offer(&self, route: &CrdtRoute, hash: &str, bytes: &[u8]) -> Result<(), String> {
        let hash = hash.to_ascii_lowercase();
        if !valid_hash(&hash) {
            return Err(format!("{hash:?} is not a content address"));
        }
        if bytes.len() as u64 > MAX_CONTENT_BYTES {
            return Err("content is larger than a record may reference".into());
        }
        if hash_bytes(bytes) != hash {
            return Err("content does not match its address".into());
        }
        let scope = ContentScope::of(route);
        self.shelf.keep(&scope, &hash, bytes)?;
        self.settle(&scope, &hash);
        self.tell(&scope, &ContentNews::arrived(&hash, bytes));
        self.repay(&scope, &hash, bytes);
        self.peers.kept(route, &hash, true);
        Ok(())
    }

    /// An app on `route` lacks `hash`, which its document references: hand it
    /// over when this device keeps it, otherwise ask the route's devices.
    /// The answer arrives on [`Self::subscribe`]. An ask that was answered
    /// with word that the devices keep none of it is asked again once the
    /// patience of that answer passed: the content may have reached a holder
    /// since, and an answer may have been lost.
    pub fn want(&self, route: &CrdtRoute, hash: &str) {
        let hash = hash.to_ascii_lowercase();
        if !valid_hash(&hash) {
            return;
        }
        let scope = ContentScope::of(route);
        if let Some(bytes) = self.shelf.read(&scope, &hash) {
            self.tell(&scope, &ContentNews::arrived(&hash, &bytes));
            return;
        }
        let now_ms = tilo_clock::unix_now_ms();
        let sources = self.peers.sources(route);
        let (ask, state, have) = {
            let Ok(mut wants) = self.wants.lock() else {
                return;
            };
            let key = (scope.clone(), hash.clone());
            if let Some(want) = wants.open.get_mut(&key) {
                // Asked before: the asker hears where it stands, and a want
                // nobody answered lately is asked again.
                let ask = want.due(now_ms);
                let have = if ask { want.have() } else { Vec::new() };
                if ask {
                    want.asked_at(now_ms);
                }
                (ask, want.state, have)
            } else {
                if wants.open.len() >= MAX_OPEN_WANTS
                    && let Some(oldest) = wants
                        .open
                        .iter()
                        .min_by_key(|(_, want)| want.order)
                        .map(|(key, _)| key.clone())
                {
                    wants.open.remove(&oldest);
                }
                wants.made = wants.made.wrapping_add(1);
                let mut want = Want {
                    route: route.clone(),
                    order: wants.made,
                    asked: BTreeSet::new(),
                    lacking: BTreeSet::new(),
                    state: ContentState::Waiting,
                    total: None,
                    bytes: Vec::new(),
                    received: 0,
                    pieces: BTreeSet::new(),
                    wanted_ms: now_ms,
                    asked_ms: now_ms,
                    last_piece_ms: now_ms,
                    reasks: 0,
                };
                want.follow(&sources);
                want.state = want.current_state(&sources);
                let state = want.state;
                wants.open.insert(key, want);
                (true, state, Vec::new())
            }
        };
        self.tell(
            &scope,
            &ContentNews {
                hash: hash.clone(),
                state,
                bytes: Vec::new(),
            },
        );
        if ask {
            self.peers.want(route, &hash, &have);
        }
    }

    /// The devices that could hand over content changed: a peer or the
    /// serving host connected or left. Ask those that connected for what is
    /// still wanted, and tell subscribers where each want now stands.
    pub fn sources_changed(&self) {
        let routes: Vec<CrdtRoute> = {
            let Ok(wants) = self.wants.lock() else {
                return;
            };
            let mut routes: Vec<CrdtRoute> =
                wants.open.values().map(|want| want.route.clone()).collect();
            routes.sort_by(|a, b| (&a.cluster_id, &a.app_id).cmp(&(&b.cluster_id, &b.app_id)));
            routes.dedup_by(|a, b| a.cluster_id == b.cluster_id && a.app_id == b.app_id);
            routes
        };
        let sources: HashMap<ContentScope, ContentSources> = routes
            .iter()
            .map(|route| (ContentScope::of(route), self.peers.sources(route)))
            .collect();
        let mut asks = Vec::new();
        let mut news = Vec::new();
        let now_ms = tilo_clock::unix_now_ms();
        {
            let Ok(mut wants) = self.wants.lock() else {
                return;
            };
            for ((scope, hash), want) in &mut wants.open {
                let Some(sources) = sources.get(scope) else {
                    continue;
                };
                if want.follow(sources) {
                    want.asked_at(now_ms);
                    asks.push((want.route.clone(), hash.clone(), want.have()));
                }
                let state = want.current_state(sources);
                if state != want.state {
                    want.state = state;
                    news.push((
                        scope.clone(),
                        ContentNews {
                            hash: hash.clone(),
                            state,
                            bytes: Vec::new(),
                        },
                    ));
                }
            }
        }
        for (scope, news) in news {
            self.tell(&scope, &news);
        }
        for (route, hash, have) in asks {
            self.peers.want(&route, &hash, &have);
        }
    }

    /// Ask the route again for what is still wanted and has not arrived
    /// since its last ask: a device that said it keeps none of the content
    /// may keep it a moment later, and only an ask brings it. A device's
    /// content service calls this on a pass of [`patience_ms`], so content
    /// that arrives at a holder after a first failed ask reaches the device
    /// that wants it without a restart. `now_ms` is the current time.
    pub fn retry(&self, now_ms: u64) {
        let mut asks: Vec<(CrdtRoute, String, Vec<Span>)> = Vec::new();
        {
            let Ok(mut wants) = self.wants.lock() else {
                return;
            };
            for ((_, hash), want) in &mut wants.open {
                if now_ms.saturating_sub(want.wanted_ms) >= WANT_TTL_MS || !want.due(now_ms) {
                    continue;
                }
                let have = want.have();
                want.asked_at(now_ms);
                asks.push((want.route.clone(), hash.clone(), have));
            }
        }
        for (route, hash, have) in asks {
            self.peers.want(&route, &hash, &have);
        }
    }

    /// `peer` said it keeps none of `hash`, asked on `route`. When no
    /// connected device is left that might, the want is unavailable.
    pub fn peer_lacks(&self, route: &CrdtRoute, peer: ContentPeer, hash: &str) {
        self.answered_none(route, peer, hash, None);
    }

    /// `peer` refused `hash`, asked on `route`, because the ask named another
    /// device than the one that sent it ([`neighbour_wire::content::answer`]).
    /// The ask goes no further as addressed: the refusal is logged here, and
    /// the device is counted out of this want as one that keeps none of it,
    /// so the asker waits on the devices that can answer instead of on one
    /// that never will.
    pub fn peer_refused(&self, route: &CrdtRoute, peer: ContentPeer, hash: &str) {
        self.answered_none(
            route,
            peer,
            hash,
            Some("an ask for content named another device"),
        );
    }

    /// `peer` answered the ask for `hash` on `route` with no content, for
    /// `why`: it keeps none of it, or refused an ask misaddressed to it.
    fn answered_none(&self, route: &CrdtRoute, peer: ContentPeer, hash: &str, why: Option<&str>) {
        let hash = hash.to_ascii_lowercase();
        let scope = ContentScope::of(route);
        let sources = self.peers.sources(route);
        let state = {
            let Ok(mut wants) = self.wants.lock() else {
                return;
            };
            let Some(want) = wants.open.get_mut(&(scope.clone(), hash.clone())) else {
                return;
            };
            // Only what a connected device says counts: one that left says
            // nothing about what it may keep when it is back.
            if !sources.all().contains(&peer) || !want.lacking.insert(peer) {
                return;
            }
            want.last_piece_ms = tilo_clock::unix_now_ms();
            let state = want.current_state(&sources);
            if state == want.state {
                return;
            }
            want.state = state;
            state
        };
        if let Some(why) = why {
            tracing::debug!(peer, %hash, why, "a device answered an ask for content with no content");
        }
        self.tell(
            &scope,
            &ContentNews {
                hash,
                state,
                bytes: Vec::new(),
            },
        );
    }

    /// The bytes of `hash` this device keeps on `route`: what a host reads an
    /// offloadable invocation's inputs from, and says of itself when another
    /// device offers it work that reads that content.
    pub fn kept(&self, route: &CrdtRoute, hash: &str) -> Option<Vec<u8>> {
        self.shelf.read(&ContentScope::of(route), hash)
    }

    /// News of content for the record `route` carries: answers to
    /// [`Self::want`] and where each stands until it arrives, and what
    /// another local app offered on its scope.
    pub fn subscribe(&self, route: &CrdtRoute) -> mpsc::UnboundedReceiver<ContentNews> {
        let (sender, receiver) = mpsc::unbounded_channel();
        if let Ok(mut subscribers) = self.subscribers.lock() {
            subscribers
                .entry(ContentScope::of(route))
                .or_default()
                .push(sender);
        }
        receiver
    }

    /// `peer` asked for `hash` on `route`, naming `have`, what it holds of it
    /// already. Answer, in the pieces it lacks, when this device says the peer
    /// may read the route (`may_read`) and keeps the content under the
    /// document's scope; say it keeps none when it may read and this device
    /// does not. A device that may not read hears nothing.
    pub fn peer_wants(
        &self,
        route: &CrdtRoute,
        peer: ContentPeer,
        may_read: bool,
        hash: &str,
        have: &[Span],
    ) {
        let hash = hash.to_ascii_lowercase();
        if !may_read || !valid_hash(&hash) {
            return;
        }
        let scope = ContentScope::of(route);
        let Some(bytes) = self
            .shelf
            .read(&scope, &hash)
            .filter(|bytes| bytes.len() as u64 <= MAX_CONTENT_BYTES)
        else {
            self.owe(route, &scope, peer, &hash);
            self.peers.lacks(route, peer, &hash);
            return;
        };
        self.send(route, peer, &hash, &bytes, have);
    }

    /// Remember that `peer`, which may read `route`, asked for `hash` while
    /// this device kept none of it. A record names content before every
    /// device that holds it has kept it where peers are served from (the
    /// writer publishes a change of its record and keeps the bytes beside
    /// it), and a peer told it is lacking asks no more until a device
    /// connects: so the device that keeps it later hands it over unasked.
    fn owe(&self, route: &CrdtRoute, scope: &ContentScope, peer: ContentPeer, hash: &str) {
        let Ok(mut wants) = self.wants.lock() else {
            return;
        };
        let key = (scope.clone(), hash.to_owned());
        if !wants.owed.contains_key(&key)
            && wants.owed.len() >= MAX_OPEN_WANTS
            && let Some(oldest) = wants
                .owed
                .iter()
                .min_by_key(|(_, owed)| owed.order)
                .map(|(key, _)| key.clone())
        {
            wants.owed.remove(&oldest);
        }
        wants.made = wants.made.wrapping_add(1);
        let order = wants.made;
        wants
            .owed
            .entry(key)
            .or_insert_with(|| Owed {
                route: route.clone(),
                order,
                peers: BTreeSet::new(),
            })
            .peers
            .insert(peer);
    }

    /// `hash` is kept under `scope` now: hand it to the peers it was owed,
    /// the ones that may still read its record.
    fn repay(&self, scope: &ContentScope, hash: &str, bytes: &[u8]) {
        let owed = {
            let Ok(mut wants) = self.wants.lock() else {
                return;
            };
            wants.owed.remove(&(scope.clone(), hash.to_owned()))
        };
        let Some(owed) = owed else {
            return;
        };
        if bytes.len() as u64 > MAX_CONTENT_BYTES {
            return;
        }
        for peer in owed.peers {
            // What a peer may read can change while the content it asked
            // for is on its way here: the workspace may have removed it, or
            // taken the record from it. The bytes go only to a device that
            // may read them now.
            if !self.peers.may_read(&owed.route, peer) {
                tracing::debug!(
                    peer,
                    %hash,
                    "content is not handed to a device that may no longer read its record"
                );
                continue;
            }
            self.send(&owed.route, peer, hash, bytes, &[]);
        }
    }

    /// Send `bytes`, the content `hash`, to `peer`: the pieces it does not
    /// hold yet (`have`), in one bounded window, so the asker asks again for
    /// the next rather than one answer filling its outbound queue.
    fn send(&self, route: &CrdtRoute, peer: ContentPeer, hash: &str, bytes: &[u8], have: &[Span]) {
        let hash = hash.to_owned();
        let total = bytes.len() as u64;
        if bytes.is_empty() {
            self.peers.answer(
                route,
                peer,
                ContentPiece {
                    hash,
                    total: 0,
                    offset: 0,
                    bytes: Vec::new(),
                },
            );
            return;
        }
        for span in answer_pieces(bytes, have) {
            let (Ok(start), Ok(len)) = (usize::try_from(span.offset), usize::try_from(span.len))
            else {
                continue;
            };
            let Some(chunk) = bytes.get(start..start.saturating_add(len)) else {
                continue;
            };
            self.peers.answer(
                route,
                peer,
                ContentPiece {
                    hash: hash.clone(),
                    total,
                    offset: span.offset,
                    bytes: chunk.to_vec(),
                },
            );
        }
    }

    /// Whether this device asked `route` for `hash` and still waits on it:
    /// what a keeper of a host's members' pieces asks before it takes one.
    #[must_use]
    pub fn expecting(&self, route: &CrdtRoute, hash: &str) -> bool {
        let scope = ContentScope::of(route);
        self.wants
            .lock()
            .is_ok_and(|wants| wants.open.contains_key(&(scope, hash.to_ascii_lowercase())))
    }

    /// A piece of content arrived on `route`. Gather it when this device
    /// asked for that content there; once whole and verified, keep it and
    /// hand it to the local apps on the record's scope.
    pub fn peer_piece(&self, route: &CrdtRoute, piece: &ContentPiece) {
        let hash = piece.hash.to_ascii_lowercase();
        let scope = ContentScope::of(route);
        let key = (scope.clone(), hash.clone());
        let now_ms = tilo_clock::unix_now_ms();
        let gathered = {
            let Ok(mut wants) = self.wants.lock() else {
                return;
            };
            let Some(want) = wants.open.get_mut(&key) else {
                return;
            };
            let before = want.pieces.len();
            if Self::gather(want, piece) {
                want.last_piece_ms = now_ms;
                if want.pieces.len() > before {
                    // The transfer is moving: the next ask waits the
                    // patience of a fresh one rather than a failed ask's.
                    want.reasks = 0;
                }
                if want.total.is_some_and(|total| want.received >= total) {
                    Some(want.restart())
                } else {
                    return;
                }
            } else {
                // A piece that contradicts what arrived before spoils what
                // was gathered.
                want.restart();
                None
            }
        };
        match gathered {
            Some(bytes) if hash_bytes(&bytes) == hash => {
                if let Err(error) = self.shelf.keep(&scope, &hash, &bytes) {
                    tracing::warn!(%error, %hash, "record content could not be kept");
                    return;
                }
                self.settle(&scope, &hash);
                self.tell(&scope, &ContentNews::arrived(&hash, &bytes));
                self.repay(&scope, &hash, &bytes);
                self.peers.kept(route, &hash, false);
                return;
            }
            Some(_) => {
                tracing::warn!(%hash, "record content from a peer did not match its address");
            }
            None => {}
        }
        // Spoiled: what is still missing is asked for again.
        let have = {
            let Ok(mut wants) = self.wants.lock() else {
                return;
            };
            let Some(want) = wants.open.get_mut(&key) else {
                return;
            };
            want.asked_at(now_ms);
            want.have()
        };
        self.peers.want(route, &hash, &have);
    }

    /// Fold `piece` into `want`; `false` when it does not fit.
    fn gather(want: &mut Want, piece: &ContentPiece) -> bool {
        if piece.total > MAX_CONTENT_BYTES || piece.bytes.len() > PIECE_BYTES {
            return false;
        }
        match want.total {
            None => {
                let Ok(total) = usize::try_from(piece.total) else {
                    return false;
                };
                want.total = Some(piece.total);
                want.bytes = vec![0; total];
            }
            Some(total) if total != piece.total => return false,
            Some(_) => {}
        }
        let Ok(start) = usize::try_from(piece.offset) else {
            return false;
        };
        let Some(end) = start.checked_add(piece.bytes.len()) else {
            return false;
        };
        let Some(slot) = want.bytes.get_mut(start..end) else {
            return false;
        };
        if !piece.offset.is_multiple_of(PIECE_BYTES as u64) {
            return false;
        }
        // A piece repeated by a second answering device counts once.
        if want.pieces.insert(piece.offset) {
            slot.copy_from_slice(&piece.bytes);
            want.received = want.received.saturating_add(piece.bytes.len() as u64);
        } else if slot != piece.bytes.as_slice() {
            return false;
        }
        true
    }

    /// Content wanted on `scope` is here now: the want is done.
    fn settle(&self, scope: &ContentScope, hash: &str) {
        if let Ok(mut wants) = self.wants.lock() {
            wants.open.remove(&(scope.clone(), hash.to_owned()));
        }
    }

    fn tell(&self, scope: &ContentScope, news: &ContentNews) {
        let Ok(mut subscribers) = self.subscribers.lock() else {
            return;
        };
        if let Some(senders) = subscribers.get_mut(scope) {
            senders.retain(|sender| sender.send(news.clone()).is_ok());
        }
    }
}

/// The record-content service of one workspace service's workspaces, and the keeper of what
/// members answer this device with: both are the embedder's to hold for as long as it serves
/// them ([`routes_content`]).
pub struct RouteContentService {
    /// The content of the workspaces' records, as this device routes it.
    pub content: Arc<RouteContent>,
    /// What the service hands the content its members answer this device with. The service
    /// keeps it weakly: whoever made it holds this.
    pub keeper: Arc<dyn ContentKeeper>,
}

/// Route the content the records of `workspaces`' workspaces reference over `shelf`, the
/// device's content store: a want goes to the device serving a member copy, or, in a workspace
/// this device hosts, to its members; and the answers come
/// back the same way, into the record content an app waiting on the want hears of
/// ([`RouteContent`]).
///
/// The service answers its members' wants from the same store. A want that got no content is
/// asked again by whoever holds the service ([`RouteContent::retry`]), once the patience of its
/// last answer passed.
///
/// A want names this device's own certificate key, the one it is certified as: the device
/// answering refuses an ask that names another
/// ([`neighbour_wire::content::answer`]), and one key serves, answers and asks everywhere.
#[must_use]
pub fn routes_content(
    workspaces: &Workspaces,
    shelf: Arc<dyn ContentShelf>,
) -> RouteContentService {
    let content = RouteContent::new(
        Arc::clone(&shelf),
        Arc::new(MemberPeers {
            workspaces: workspaces.clone(),
            device: workspaces.device_cert().device_public_key,
        }),
    );
    workspaces.keep_shelf(Arc::clone(&shelf));
    workspaces.serve_content(Arc::new(ShelfContent(shelf)));
    let keeper: Arc<dyn ContentKeeper> = Arc::new(MemberContent::new(Arc::clone(&content)));
    // Ahead of what this device keeps of what its members push: an answer to a want this device
    // made and a push to it are the same message, and only this keeper knows which of its wants
    // the answer is for.
    workspaces.keep_content_first(&keeper);
    RouteContentService { content, keeper }
}

/// The workspace's content store, as a host answers its members' wants from it.
struct ShelfContent(Arc<dyn ContentShelf>);

impl WorkspaceContent for ShelfContent {
    fn read(&self, workspace: &str, app: &str, hash: &str) -> Option<Vec<u8>> {
        self.0.read(
            &ContentScope::of(&CrdtRoute::app(app.to_owned(), workspace.to_owned())),
            hash,
        )
    }
}

/// The workspaces' links as the route of their records' content.
struct MemberPeers {
    workspaces: Workspaces,
    /// This device's own certificate key, which a want names, as the one key it serves,
    /// answers and asks with.
    device: [u8; 32],
}

impl MemberPeers {
    fn workspace(route: &CrdtRoute) -> Option<WorkspaceId> {
        WorkspaceId::parse(&route.cluster_id)
    }
}

impl ContentPeers for MemberPeers {
    fn want(&self, route: &CrdtRoute, hash: &str, have: &[Span]) {
        let Some(workspace) = Self::workspace(route) else {
            return;
        };
        let message = NeighbourMsg::DocumentContentWants {
            requester: peer_handle(&self.device),
            cluster_id: route.cluster_id.clone(),
            wants: vec![neighbour_wire::content::Wanted {
                app_id: route.app_id.clone(),
                hash: hash.to_owned(),
                have: have.to_vec(),
            }],
        };
        if self.workspaces.member_link(workspace).is_some() {
            let _ = self.workspaces.send_member(workspace, message);
            return;
        }
        // The workspace this device hosts: it asks its members.
        let Ok(body) = postcard::to_stdvec(&message) else {
            return;
        };
        let copies = self.workspaces.clone();
        let cluster = route.cluster_id.clone();
        tokio::spawn(async move {
            if let Err(error) = copies.ask_members(&cluster, body).await {
                tracing::debug!(%cluster, %error, "a workspace's host did not ask its members for content");
            }
        });
    }

    /// The service answers a want on whichever link it arrived on, from the content it is given
    /// ([`ShelfContent`]).
    fn answer(&self, _route: &CrdtRoute, _peer: ContentPeer, _piece: ContentPiece) {}

    /// The service answers what it keeps none of itself.
    fn lacks(&self, _route: &CrdtRoute, _peer: ContentPeer, _hash: &str) {}

    /// What the service said it lacks is answered by the device that wrote
    /// it ([`Workspaces::content_kept`]); one that fetched it leaves that to
    /// whoever it came from.
    /// Content written here goes to the workspace's host too
    /// ([`Workspaces::push_content`]).
    fn kept(&self, route: &CrdtRoute, hash: &str, written: bool) {
        if written {
            self.workspaces.content_kept(&route.cluster_id, hash);
            self.workspaces
                .push_content(&route.cluster_id, &route.app_id, hash);
        } else {
            self.workspaces.content_fetched(&route.cluster_id, hash);
        }
    }

    fn sources(&self, route: &CrdtRoute) -> ContentSources {
        let Some(workspace) = Self::workspace(route) else {
            return ContentSources::default();
        };
        let hosts = self
            .workspaces
            .member_material(workspace)
            .map(|material| peer_handle(&material.hint.host.device_public_key))
            .into_iter()
            .collect::<BTreeSet<_>>();
        ContentSources {
            peers: BTreeSet::new(),
            hosts,
        }
    }
}

/// The pieces the device serving a member copy answers this device's wants with, taken into the
/// record content.
pub struct MemberContent(Arc<RouteContent>);

impl MemberContent {
    /// Take what members answer this device with into `content`.
    #[must_use]
    pub fn new(content: Arc<RouteContent>) -> Self {
        Self(content)
    }
}

impl ContentKeeper for MemberContent {
    fn take(
        &self,
        _workspace: &str,
        _from: [u8; 32],
        message: NeighbourMsg,
    ) -> Result<(), Box<NeighbourMsg>> {
        let expected = match &message {
            NeighbourMsg::DocumentContentPieces {
                cluster_id, pieces, ..
            } => pieces.iter().any(|piece| {
                self.0.expecting(
                    &CrdtRoute::app(piece.app_id.clone(), cluster_id.clone()),
                    &piece.hash,
                )
            }),
            NeighbourMsg::DocumentContentLacks {
                app_id,
                cluster_id,
                hash,
                ..
            }
            | NeighbourMsg::DocumentContentRefused {
                app_id,
                cluster_id,
                hash,
                ..
            } => self
                .0
                .expecting(&CrdtRoute::app(app_id.clone(), cluster_id.clone()), hash),
            _ => false,
        };
        if !expected {
            return Err(Box::new(message));
        }
        match message {
            NeighbourMsg::DocumentContentPieces {
                cluster_id, pieces, ..
            } => {
                for piece in pieces {
                    self.0.peer_piece(
                        &CrdtRoute::app(piece.app_id, cluster_id.clone()),
                        &ContentPiece {
                            hash: piece.hash,
                            total: piece.total,
                            offset: piece.offset,
                            bytes: piece.bytes,
                        },
                    );
                }
            }
            NeighbourMsg::DocumentContentLacks {
                responder,
                app_id,
                cluster_id,
                hash,
                ..
            } => self
                .0
                .peer_lacks(&CrdtRoute::app(app_id, cluster_id), responder, &hash),
            NeighbourMsg::DocumentContentRefused {
                responder,
                app_id,
                cluster_id,
                hash,
                ..
            } => self
                .0
                .peer_refused(&CrdtRoute::app(app_id, cluster_id), responder, &hash),
            _ => {}
        }
        Ok(())
    }
}

#[cfg(any(test, feature = "test-support"))]
pub mod fixtures;
#[cfg(test)]
mod tests;
