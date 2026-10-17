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

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tilo_content::{hash_bytes, valid_hash};
use tokio::sync::mpsc;

use crate::route::CrdtRoute;

/// Largest content one reference names. Bigger content belongs in a file
/// record.
pub const MAX_CONTENT_BYTES: u64 = 64 * 1024 * 1024;
/// Bytes one piece carries on the network.
pub const PIECE_BYTES: usize = 192 * 1024;
/// How long an unanswered request keeps collecting pieces, and how long
/// before the same content may be asked of the route again.
const WANT_TTL: Duration = Duration::from_secs(60);
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
}

/// Content that arrived for a scope, as an app's `subscribeBlobs` reads it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContentArrival {
    pub hash: String,
    pub bytes: Vec<u8>,
}

/// A request this device made and the pieces gathered for it.
struct Want {
    asked: Instant,
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
    subscribers: Mutex<HashMap<ContentScope, Vec<mpsc::UnboundedSender<ContentArrival>>>>,
    wants: Mutex<HashMap<(ContentScope, String), Want>>,
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
        self.arrived(&scope, &hash, bytes);
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
            self.arrived(&scope, &hash, &bytes);
            return;
        }
        let ask = {
            let Ok(mut wants) = self.wants.lock() else {
                return;
            };
            wants.retain(|_, want| want.asked.elapsed() < WANT_TTL);
            let key = (scope, hash.clone());
            if wants.contains_key(&key) {
                false
            } else {
                if wants.len() >= MAX_OPEN_WANTS
                    && let Some(oldest) = wants
                        .iter()
                        .min_by_key(|(_, want)| want.asked)
                        .map(|(key, _)| key.clone())
                {
                    wants.remove(&oldest);
                }
                wants.insert(
                    key,
                    Want {
                        asked: Instant::now(),
                        total: None,
                        bytes: Vec::new(),
                        received: 0,
                        pieces: std::collections::BTreeSet::new(),
                    },
                );
                true
            }
        };
        if ask {
            self.peers.want(route, &hash);
        }
    }

    /// Content arriving for the record `route` carries — answers to
    /// [`Self::want`] and what another local app offered on its scope.
    pub fn subscribe(&self, route: &CrdtRoute) -> mpsc::UnboundedReceiver<ContentArrival> {
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
    /// the content under the record's scope; otherwise say nothing.
    pub fn peer_wants(&self, route: &CrdtRoute, peer: ContentPeer, may_read: bool, hash: &str) {
        let hash = hash.to_ascii_lowercase();
        if !may_read || !valid_hash(&hash) {
            return;
        }
        let Some(bytes) = self.shelf.read(&ContentScope::of(route), &hash) else {
            return;
        };
        let total = bytes.len() as u64;
        if total > MAX_CONTENT_BYTES {
            return;
        }
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
        let complete = {
            let Ok(mut wants) = self.wants.lock() else {
                return;
            };
            let Some(want) = wants.get_mut(&key) else {
                return;
            };
            if !Self::gather(want, piece) {
                // A piece that contradicts what arrived before spoils the
                // request; it may be asked again later.
                wants.remove(&key);
                return;
            }
            if want.total.is_some_and(|total| want.received >= total) {
                wants.remove(&key).map(|want| want.bytes)
            } else {
                None
            }
        };
        let Some(bytes) = complete else {
            return;
        };
        if hash_bytes(&bytes) != hash {
            tracing::warn!(%hash, "record content from a peer did not match its address");
            return;
        }
        if let Err(error) = self.shelf.keep(&scope, &hash, &bytes) {
            tracing::warn!(%error, %hash, "record content could not be kept");
            return;
        }
        self.arrived(&scope, &hash, &bytes);
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

    fn arrived(&self, scope: &ContentScope, hash: &str, bytes: &[u8]) {
        let Ok(mut subscribers) = self.subscribers.lock() else {
            return;
        };
        if let Some(senders) = subscribers.get_mut(scope) {
            senders.retain(|sender| {
                sender
                    .send(ContentArrival {
                        hash: hash.to_owned(),
                        bytes: bytes.to_vec(),
                    })
                    .is_ok()
            });
        }
    }
}

#[cfg(test)]
pub(crate) mod tests;
