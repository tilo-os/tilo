//! Record content between two hosts joined by an in-process route.

use std::collections::BTreeMap;
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
/// answers, as the network would carry it.
pub(crate) struct Wire {
    /// The host at the other end, and whether it lets this one read.
    other: OnceLock<(Arc<RouteContent>, bool)>,
    /// What this host sent, for the assertions.
    pub(crate) sent: Mutex<Vec<String>>,
}

impl Wire {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self {
            other: OnceLock::new(),
            sent: Mutex::default(),
        })
    }
}

impl ContentPeers for Wire {
    fn want(&self, route: &CrdtRoute, hash: &str) {
        self.sent.lock().unwrap().push(format!("want {hash}"));
        if let Some((other, may_read)) = self.other.get() {
            other.peer_wants(route, 7, *may_read, hash);
        }
    }
    fn answer(&self, route: &CrdtRoute, _peer: ContentPeer, piece: ContentPiece) {
        self.sent
            .lock()
            .unwrap()
            .push(format!("piece {}", piece.offset));
        if let Some((other, _)) = self.other.get() {
            other.peer_piece(route, &piece);
        }
    }
}

/// Two hosts on one route; `b_may_read` says whether A serves B.
pub(crate) fn pair(b_may_read: bool) -> (Arc<RouteContent>, Arc<RouteContent>, Arc<Wire>) {
    let (wire_a, wire_b) = (Wire::new(), Wire::new());
    let a = RouteContent::new(Arc::new(Shelf::default()), wire_a.clone());
    let b = RouteContent::new(Arc::new(Shelf::default()), wire_b.clone());
    assert!(wire_a.other.set((Arc::clone(&b), true)).is_ok());
    assert!(wire_b.other.set((Arc::clone(&a), b_may_read)).is_ok());
    (a, b, wire_b)
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
    let arrival = arrivals.try_recv().expect("the content arrived");
    assert_eq!(arrival.hash, hash);
    assert_eq!(arrival.bytes, bytes);
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
    assert!(arrivals.try_recv().is_ok());
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
    assert!(arrivals.try_recv().is_err());
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
    assert!(
        arrivals.try_recv().is_err(),
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
    assert!(arrivals.try_recv().is_err());
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
    assert!(arrivals.try_recv().is_err());
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
        in_window.try_recv().is_err(),
        "the window's room keeps none of it"
    );

    let mut in_person = b.subscribe(&person);
    b.want(&person, &hash);
    assert_eq!(in_person.try_recv().unwrap().bytes, poster);
}
