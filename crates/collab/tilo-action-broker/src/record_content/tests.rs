//! Record content between two hosts joined by an in-process route.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use super::*;

/// A device content store in memory.
#[derive(Default)]
pub(crate) struct Shelf(Mutex<BTreeMap<(ContentScope, String), Vec<u8>>>);

impl ContentShelf for Shelf {
    fn read(&self, scope: &ContentScope, hash: &str) -> Option<Vec<u8>> {
        self.0
            .lock()
            .unwrap()
            .get(&(scope.clone(), hash.to_owned()))
            .cloned()
    }
    fn keep(&self, scope: &ContentScope, hash: &str, bytes: &[u8]) -> Result<(), String> {
        self.0
            .lock()
            .unwrap()
            .insert((scope.clone(), hash.to_owned()), bytes.to_vec());
        Ok(())
    }
}

/// Two hosts' routes, wired to each other: what one asks, the other
/// answers, as the network would carry it, while the two are connected.
pub(crate) struct Wire {
    /// This host's handle on the route.
    me: ContentPeer,
    /// The host at the other end, its handle, and whether it lets this one
    /// read.
    other: OnceLock<(Arc<RouteContent>, ContentPeer, bool)>,
    /// Whether the two are connected, shared by both ends.
    connected: Arc<AtomicBool>,
    /// What this host sent, for the assertions.
    pub(crate) sent: Mutex<Vec<String>>,
}

impl Wire {
    pub(crate) fn new(me: ContentPeer, connected: Arc<AtomicBool>) -> Arc<Self> {
        Arc::new(Self {
            me,
            other: OnceLock::new(),
            connected,
            sent: Mutex::default(),
        })
    }

    /// The other end, while connected.
    fn reach(&self) -> Option<&(Arc<RouteContent>, ContentPeer, bool)> {
        self.other
            .get()
            .filter(|_| self.connected.load(Ordering::Acquire))
    }
}

impl ContentPeers for Wire {
    fn want(&self, route: &CrdtRoute, hash: &str) {
        self.sent.lock().unwrap().push(format!("want {hash}"));
        if let Some((other, _, may_read)) = self.reach() {
            other.peer_wants(route, self.me, *may_read, hash);
        }
    }
    fn answer(&self, route: &CrdtRoute, _peer: ContentPeer, piece: ContentPiece) {
        self.sent
            .lock()
            .unwrap()
            .push(format!("piece {}", piece.offset));
        if let Some((other, _, _)) = self.reach() {
            other.peer_piece(route, &piece);
        }
    }
    fn lacks(&self, route: &CrdtRoute, _peer: ContentPeer, hash: &str) {
        self.sent.lock().unwrap().push(format!("lacks {hash}"));
        if let Some((other, _, _)) = self.reach() {
            other.peer_lacks(route, self.me, hash);
        }
    }
    fn sources(&self, _route: &CrdtRoute) -> ContentSources {
        ContentSources {
            peers: self.reach().map(|(_, peer, _)| *peer).into_iter().collect(),
            hosts: BTreeSet::new(),
        }
    }
}

/// Two hosts on one route, connected or not, and the switch between them.
pub(crate) struct Pair {
    pub(crate) a: Arc<RouteContent>,
    pub(crate) b: Arc<RouteContent>,
    pub(crate) wire_b: Arc<Wire>,
    connected: Arc<AtomicBool>,
}

impl Pair {
    /// Connect or part the two hosts, as their network would, and tell
    /// both.
    pub(crate) fn connect(&self, connected: bool) {
        self.connected.store(connected, Ordering::Release);
        self.a.sources_changed();
        self.b.sources_changed();
    }
}

/// Two hosts on one route, `connected` or not; `b_may_read` says whether A
/// serves B.
pub(crate) fn hosts(connected: bool, b_may_read: bool) -> Pair {
    let link = Arc::new(AtomicBool::new(connected));
    let (wire_a, wire_b) = (
        Wire::new(1, Arc::clone(&link)),
        Wire::new(2, Arc::clone(&link)),
    );
    let a = RouteContent::new(Arc::new(Shelf::default()), wire_a.clone());
    let b = RouteContent::new(Arc::new(Shelf::default()), wire_b.clone());
    assert!(wire_a.other.set((Arc::clone(&b), 2, true)).is_ok());
    assert!(wire_b.other.set((Arc::clone(&a), 1, b_may_read)).is_ok());
    Pair {
        a,
        b,
        wire_b,
        connected: link,
    }
}

/// Two connected hosts on one route; `b_may_read` says whether A serves B.
pub(crate) fn pair(b_may_read: bool) -> (Arc<RouteContent>, Arc<RouteContent>, Arc<Wire>) {
    let pair = hosts(true, b_may_read);
    (pair.a, pair.b, pair.wire_b)
}

/// The next content that arrived on `news`, past where wants stand.
fn arrival(news: &mut mpsc::UnboundedReceiver<ContentNews>) -> Option<ContentNews> {
    while let Ok(next) = news.try_recv() {
        if next.state == ContentState::Resolved {
            return Some(next);
        }
    }
    None
}

/// Where wants stood, as `news` told, in order.
fn states(news: &mut mpsc::UnboundedReceiver<ContentNews>) -> Vec<ContentState> {
    std::iter::from_fn(|| news.try_recv().ok())
        .map(|news| news.state)
        .collect()
}

fn route() -> CrdtRoute {
    CrdtRoute::record("so.tilo.slides", "room-1", "deck-1")
}

fn picture(len: usize) -> Vec<u8> {
    (0..len)
        .map(|index| u8::try_from(index % 251).unwrap())
        .collect()
}

#[test]
fn content_one_host_was_offered_reaches_the_other_in_verified_pieces() {
    let (a, b, wire_b) = pair(true);
    let bytes = picture(PIECE_BYTES * 2 + 17);
    let hash = hash_bytes(&bytes);
    a.offer(&route(), &hash, &bytes).unwrap();

    let mut arrivals = b.subscribe(&route());
    b.want(&route(), &hash);
    let arrived = arrival(&mut arrivals).expect("the content arrived");
    assert_eq!(arrived.hash, hash);
    assert_eq!(arrived.bytes, bytes);
    assert_eq!(
        b.shelf.read(&ContentScope::of(&route()), &hash),
        Some(bytes),
        "and is kept under the record's scope"
    );
    assert_eq!(
        wire_b.sent.lock().unwrap().as_slice(),
        [format!("want {hash}")]
    );

    // Kept now: asking again answers from this device.
    b.want(&route(), &hash);
    assert!(arrival(&mut arrivals).is_some());
    assert_eq!(
        wire_b.sent.lock().unwrap().len(),
        1,
        "nothing more was asked"
    );
}

#[test]
fn a_device_that_may_not_read_the_record_gets_nothing() {
    let (a, b, _) = pair(false);
    let bytes = picture(100);
    let hash = hash_bytes(&bytes);
    a.offer(&route(), &hash, &bytes).unwrap();
    let mut arrivals = b.subscribe(&route());
    b.want(&route(), &hash);
    assert!(arrival(&mut arrivals).is_none());
}

#[test]
fn content_of_another_record_scope_is_not_served() {
    let (a, b, _) = pair(true);
    let bytes = picture(100);
    let hash = hash_bytes(&bytes);
    a.offer(
        &CrdtRoute::record("so.tilo.slides", "another-room", "deck-1"),
        &hash,
        &bytes,
    )
    .unwrap();
    let mut arrivals = b.subscribe(&route());
    b.want(&route(), &hash);
    assert_eq!(
        states(&mut arrivals),
        [ContentState::Pending, ContentState::Unavailable],
        "holding the hash elsewhere does not make it this record's"
    );
}

#[test]
fn bytes_that_are_not_the_content_are_refused() {
    let (a, b, _) = pair(true);
    let bytes = picture(100);
    let hash = hash_bytes(&bytes);
    assert!(a.offer(&route(), &hash, b"other bytes").is_err());
    let mut arrivals = b.subscribe(&route());
    b.want(&route(), &hash);
    // A forged answer for content asked for is dropped.
    b.peer_piece(
        &route(),
        &ContentPiece {
            hash: hash.clone(),
            total: 3,
            offset: 0,
            bytes: b"bad".to_vec(),
        },
    );
    assert!(arrival(&mut arrivals).is_none());
    assert_eq!(b.shelf.read(&ContentScope::of(&route()), &hash), None);
}

#[test]
fn pieces_nobody_asked_for_are_dropped() {
    let (_, b, _) = pair(true);
    let bytes = picture(10);
    let hash = hash_bytes(&bytes);
    let mut arrivals = b.subscribe(&route());
    b.peer_piece(
        &route(),
        &ContentPiece {
            hash: hash.clone(),
            total: 10,
            offset: 0,
            bytes,
        },
    );
    assert!(arrival(&mut arrivals).is_none());
    assert_eq!(b.shelf.read(&ContentScope::of(&route()), &hash), None);
}

/// A person record's content follows the record into the person's room,
/// whichever room the window that stored it shows: a record of the window's
/// room does not reach it.
#[test]
fn a_person_record_s_content_lives_in_the_person_s_room() {
    let (a, b, _) = pair(true);
    let person = CrdtRoute::record("so.tilo.tube", "person-room", "history");
    let window = CrdtRoute::record("so.tilo.tube", "room-1", "");
    let poster = picture(PIECE_BYTES + 3);
    let hash = hash_bytes(&poster);
    a.offer(&person, &hash, &poster).unwrap();

    let mut in_window = b.subscribe(&window);
    b.want(&window, &hash);
    assert!(
        arrival(&mut in_window).is_none(),
        "the window's room keeps none of it"
    );

    let mut in_person = b.subscribe(&person);
    b.want(&person, &hash);
    assert_eq!(arrival(&mut in_person).unwrap().bytes, poster);
}

/// A want follows the devices that could answer it, never a timer: it
/// waits while none is connected, is fetched the moment one connects, and
/// is unavailable once every connected one said it keeps none.
#[test]
fn a_want_waits_for_its_holder_and_is_fetched_when_it_connects() {
    let pair = hosts(false, true);
    let bytes = picture(PIECE_BYTES + 9);
    let hash = hash_bytes(&bytes);
    pair.a.offer(&route(), &hash, &bytes).unwrap();
    let mut news = pair.b.subscribe(&route());

    pair.b.want(&route(), &hash);
    assert_eq!(
        states(&mut news),
        [ContentState::Waiting],
        "its holder is offline"
    );

    pair.connect(true);
    let told: Vec<ContentNews> = std::iter::from_fn(|| news.try_recv().ok()).collect();
    assert_eq!(
        told.iter().map(|news| news.state).collect::<Vec<_>>(),
        [ContentState::Pending, ContentState::Resolved],
        "fetched from the holder as it connects, then here"
    );
    assert_eq!(told[1].bytes, bytes);
}

#[test]
fn a_want_nobody_connected_keeps_is_unavailable_until_one_that_might_connects() {
    let pair = hosts(true, true);
    let hash = hash_bytes(b"kept nowhere");
    let mut news = pair.b.subscribe(&route());

    pair.b.want(&route(), &hash);
    assert_eq!(
        states(&mut news),
        [ContentState::Pending, ContentState::Unavailable],
        "the one connected device said it keeps none"
    );

    pair.connect(false);
    assert_eq!(
        states(&mut news),
        [ContentState::Waiting],
        "what a device that left said no longer counts"
    );

    // The device came back with the content: asked again, it hands it over.
    pair.a.offer(&route(), &hash, b"kept nowhere").unwrap();
    pair.connect(true);
    assert_eq!(
        states(&mut news),
        [ContentState::Pending, ContentState::Resolved]
    );
}
