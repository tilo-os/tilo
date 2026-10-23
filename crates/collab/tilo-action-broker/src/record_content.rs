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
//! - its [`ContentPeers`]: the record's route on the network — the room's
//!   topic for a room record, the person's own devices for a person record.
//!
//! The rules:
//!
//! - **Possessing a hash never grants access.** A device answers a peer's
//!   request only when the host says the peer may read the record's route,
//!   and only with content kept under that record's scope on this device.
//! - **Bytes are verified, never trusted.** Content travels in bounded
//!   pieces addressed by hash; the assembled bytes must hash to what was
//!   asked for before they are kept or handed to an app.
//! - **Nothing arrives unasked.** Pieces for content this device did not
//!   ask for on that route are dropped.
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
//! The state follows the host's connections, never a timer: the host says
//! when they change ([`RouteContent::sources_changed`]), and a device that
//! connects is asked for what is still wanted.

use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, Mutex};

pub use tilo_content::ContentState;
use tilo_content::{hash_bytes, valid_hash};
use tokio::sync::mpsc;

use crate::route::CrdtRoute;

/// Largest content one reference names. Bigger content belongs in a file
/// record.
pub const MAX_CONTENT_BYTES: u64 = 64 * 1024 * 1024;
/// Bytes one piece carries on the network.
pub const PIECE_BYTES: usize = 192 * 1024;
/// Requests this device keeps open at once; past it, the oldest go.
const MAX_OPEN_WANTS: usize = 256;

/// Where a record's content lives on a device: the room the record is
/// kept in, and the app whose record it is. A person record's content
/// lives under the person's own room, whichever room the window showing it
/// is in.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ContentScope {
    pub room: String,
    pub app: String,
}

impl ContentScope {
    /// The scope of the record `route` carries.
    pub fn of(route: &CrdtRoute) -> Self {
        Self {
            room: route.cluster_id.clone(),
            app: route.app_id.clone(),
        }
    }
}

/// The device content store, as the host reaches it for records.
pub trait ContentShelf: Send + Sync {
    /// The bytes of `hash`, when this device keeps them under `scope`.
    fn read(&self, scope: &ContentScope, hash: &str) -> Option<Vec<u8>>;
    /// Keep `bytes`, whose hash is `hash`, under `scope`.
    ///
    /// # Errors
    /// Returns why the store would not keep them.
    fn keep(&self, scope: &ContentScope, hash: &str, bytes: &[u8]) -> Result<(), String>;
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
    /// Ask the devices on `route` for `hash`.
    fn want(&self, route: &CrdtRoute, hash: &str);
    /// Send `piece` of content to `peer`, which asked for it on `route`.
    fn answer(&self, route: &CrdtRoute, peer: ContentPeer, piece: ContentPiece);
    /// Tell `peer`, which asked on `route` for `hash`, that this device
    /// keeps none of it.
    fn lacks(&self, route: &CrdtRoute, peer: ContentPeer, hash: &str);
    /// The devices connected now that could hand over `route`'s content,
    /// never this device.
    fn sources(&self, route: &CrdtRoute) -> ContentSources;
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
}

impl Want {
    /// Where a want stands with `sources` connected: fetching while one of
    /// them has not said it lacks the content; past that unavailable when
    /// peers are connected, and waiting for one when none is.
    fn standing(&self, sources: &ContentSources) -> ContentState {
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
        Ok(())
    }

    /// An app on `route` lacks `hash`, which its record references: hand it
    /// over when this device keeps it, otherwise ask the route's devices.
    /// The answer arrives on [`Self::subscribe`].
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
        let sources = self.peers.sources(route);
        let (ask, state) = {
            let Ok(mut wants) = self.wants.lock() else {
                return;
            };
            let key = (scope.clone(), hash.clone());
            if let Some(want) = wants.open.get(&key) {
                // Asked already: the asker hears where it stands.
                (false, want.state)
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
                };
                want.follow(&sources);
                want.state = want.standing(&sources);
                let state = want.state;
                wants.open.insert(key, want);
                (true, state)
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
            self.peers.want(route, &hash);
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
        {
            let Ok(mut wants) = self.wants.lock() else {
                return;
            };
            for ((scope, hash), want) in &mut wants.open {
                let Some(sources) = sources.get(scope) else {
                    continue;
                };
                if want.follow(sources) {
                    asks.push((want.route.clone(), hash.clone()));
                }
                let state = want.standing(sources);
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
        for (route, hash) in asks {
            self.peers.want(&route, &hash);
        }
    }

    /// `peer` said it keeps none of `hash`, asked on `route`. When no
    /// connected device is left that might, the want is unavailable.
    pub fn peer_lacks(&self, route: &CrdtRoute, peer: ContentPeer, hash: &str) {
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
            let state = want.standing(&sources);
            if state == want.state {
                return;
            }
            want.state = state;
            state
        };
        self.tell(
            &scope,
            &ContentNews {
                hash,
                state,
                bytes: Vec::new(),
            },
        );
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

    /// `peer` asked for `hash` on `route`. Answer, in pieces, when the host
    /// says the peer may read the route (`may_read`) and this device keeps
    /// the content under the record's scope; say it keeps none when it may
    /// read and this device does not. A device that may not read hears
    /// nothing.
    pub fn peer_wants(&self, route: &CrdtRoute, peer: ContentPeer, may_read: bool, hash: &str) {
        let hash = hash.to_ascii_lowercase();
        if !may_read || !valid_hash(&hash) {
            return;
        }
        let Some(bytes) = self
            .shelf
            .read(&ContentScope::of(route), &hash)
            .filter(|bytes| bytes.len() as u64 <= MAX_CONTENT_BYTES)
        else {
            self.peers.lacks(route, peer, &hash);
            return;
        };
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
        let mut offset = 0_u64;
        for chunk in bytes.chunks(PIECE_BYTES) {
            self.peers.answer(
                route,
                peer,
                ContentPiece {
                    hash: hash.clone(),
                    total,
                    offset,
                    bytes: chunk.to_vec(),
                },
            );
            offset = offset.saturating_add(chunk.len() as u64);
        }
    }

    /// A piece of content arrived on `route`. Gather it when this device
    /// asked for that content there; once whole and verified, keep it and
    /// hand it to the local apps on the record's scope.
    pub fn peer_piece(&self, route: &CrdtRoute, piece: &ContentPiece) {
        let hash = piece.hash.to_ascii_lowercase();
        let scope = ContentScope::of(route);
        let key = (scope.clone(), hash.clone());
        let gathered = {
            let Ok(mut wants) = self.wants.lock() else {
                return;
            };
            let Some(want) = wants.open.get_mut(&key) else {
                return;
            };
            if !Self::gather(want, piece) {
                // A piece that contradicts what arrived before spoils what
                // was gathered.
                want.restart();
                None
            } else if want.total.is_some_and(|total| want.received >= total) {
                Some(want.restart())
            } else {
                return;
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
                return;
            }
            Some(_) => {
                tracing::warn!(%hash, "record content from a peer did not match its address");
            }
            None => {}
        }
        // Spoiled: it is asked for again.
        self.peers.want(route, &hash);
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

#[cfg(test)]
pub(crate) mod tests;
