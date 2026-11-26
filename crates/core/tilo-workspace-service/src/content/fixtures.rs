//! Record content between hosts joined by an in-process route: the fixtures this crate's
//! tests and its embedders' tests stand on.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "a fixture failing fails its test"
)]

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use super::{
    ContentPeer, ContentPeers, ContentPiece, ContentScope, ContentShelf, ContentSources, CrdtRoute,
    RouteContent, Span,
};

/// A device content store in memory.
#[derive(Default)]
pub struct Shelf(Mutex<BTreeMap<(ContentScope, String), Vec<u8>>>);

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

/// One other end on the route, as this end's wire reaches it.
struct Link {
    /// The host at the other end.
    other: Arc<RouteContent>,
    /// That host's handle on the route.
    peer: ContentPeer,
    /// Whether the two are connected now, shared by both ends.
    connected: Arc<AtomicBool>,
    /// Whether the other end lets this one read its content.
    lets_me: Arc<AtomicBool>,
    /// Whether this end lets the other read its content.
    lets_them: Arc<AtomicBool>,
}

/// One host's routes on the record, wired to the other ends: what it asks,
/// they answer, as the network would carry it, while they are connected.
pub struct Wire {
    /// This host's handle on the route.
    me: ContentPeer,
    links: Mutex<Vec<Link>>,
    /// What this host sent, for the assertions.
    pub sent: Mutex<Vec<String>>,
}

impl Wire {
    pub fn new(me: ContentPeer) -> Arc<Self> {
        Arc::new(Self {
            me,
            links: Mutex::default(),
            sent: Mutex::default(),
        })
    }

    /// Carry this end's messages to `other`, handle `peer` on the route,
    /// while `connected`; `lets_me` and `lets_them` say which of the two
    /// lets the other read its content.
    fn link(
        &self,
        other: Arc<RouteContent>,
        peer: ContentPeer,
        connected: Arc<AtomicBool>,
        lets_me: Arc<AtomicBool>,
        lets_them: Arc<AtomicBool>,
    ) {
        self.links.lock().unwrap().push(Link {
            other,
            peer,
            connected,
            lets_me,
            lets_them,
        });
    }

    /// The ends this host reaches now, each with its handle there and
    /// whether it lets this one read its content.
    fn reach(&self) -> Vec<(Arc<RouteContent>, ContentPeer, Arc<AtomicBool>)> {
        self.links
            .lock()
            .unwrap()
            .iter()
            .filter(|link| link.connected.load(Ordering::Acquire))
            .map(|link| {
                (
                    Arc::clone(&link.other),
                    link.peer,
                    Arc::clone(&link.lets_me),
                )
            })
            .collect()
    }
}

impl ContentPeers for Wire {
    fn want(&self, route: &CrdtRoute, hash: &str, have: &[Span]) {
        self.sent.lock().unwrap().push(format!("want {hash}"));
        for (other, _, lets_me) in self.reach() {
            other.peer_wants(route, self.me, lets_me.load(Ordering::Acquire), hash, have);
        }
    }
    fn answer(&self, route: &CrdtRoute, peer: ContentPeer, piece: ContentPiece) {
        self.sent
            .lock()
            .unwrap()
            .push(format!("piece {} to {peer}", piece.offset));
        if let Some((other, _, _)) = self.reach().into_iter().find(|(_, at, _)| *at == peer) {
            other.peer_piece(route, &piece);
        }
    }
    fn lacks(&self, route: &CrdtRoute, peer: ContentPeer, hash: &str) {
        self.sent.lock().unwrap().push(format!("lacks {hash}"));
        if let Some((other, _, _)) = self.reach().into_iter().find(|(_, at, _)| *at == peer) {
            other.peer_lacks(route, self.me, hash);
        }
    }
    fn sources(&self, _route: &CrdtRoute) -> ContentSources {
        ContentSources {
            peers: self.reach().into_iter().map(|(_, peer, _)| peer).collect(),
            ..ContentSources::default()
        }
    }
    fn may_read(&self, _route: &CrdtRoute, peer: ContentPeer) -> bool {
        self.links
            .lock()
            .unwrap()
            .iter()
            .find(|link| link.peer == peer)
            .is_some_and(|link| link.lets_them.load(Ordering::Acquire))
    }
}

/// Two hosts on one route, connected or not, and the switch between them.
pub struct Pair {
    pub a: Arc<RouteContent>,
    pub b: Arc<RouteContent>,
    pub wire_b: Arc<Wire>,
    connected: Arc<AtomicBool>,
}

impl Pair {
    /// Connect or part the two hosts, as their network would, and tell
    /// both.
    pub fn connect(&self, connected: bool) {
        self.connected.store(connected, Ordering::Release);
        self.a.sources_changed();
        self.b.sources_changed();
    }
}

/// Two hosts on one route, `connected` or not; `b_may_read` says whether A
/// serves B.
pub fn hosts(connected: bool, b_may_read: bool) -> Pair {
    let up = Arc::new(AtomicBool::new(connected));
    // Which of the two lets the other read its content.
    let a_serves_b = Arc::new(AtomicBool::new(b_may_read));
    let b_serves_a = Arc::new(AtomicBool::new(true));
    let (wire_a, wire_b) = (Wire::new(1), Wire::new(2));
    let a = RouteContent::new(Arc::new(Shelf::default()), wire_a.clone());
    let b = RouteContent::new(Arc::new(Shelf::default()), wire_b.clone());
    wire_a.link(
        Arc::clone(&b),
        2,
        Arc::clone(&up),
        Arc::clone(&b_serves_a),
        Arc::clone(&a_serves_b),
    );
    wire_b.link(
        Arc::clone(&a),
        1,
        Arc::clone(&up),
        Arc::clone(&a_serves_b),
        Arc::clone(&b_serves_a),
    );
    Pair {
        a,
        b,
        wire_b,
        connected: up,
    }
}

/// Three connected hosts on one route: A keeps the content, and B and C ask
/// it for what they lack.
pub struct Fan {
    pub a: Arc<RouteContent>,
    pub b: Arc<RouteContent>,
    pub c: Arc<RouteContent>,
    /// What A sent, for the assertions.
    pub wire_a: Arc<Wire>,
    /// Whether A serves B, for a test to take away while B waits.
    pub a_serves_b: Arc<AtomicBool>,
}

pub fn fan() -> Fan {
    let up = Arc::new(AtomicBool::new(true));
    let serves = Arc::new(AtomicBool::new(true));
    let a_serves_b = Arc::new(AtomicBool::new(true));
    let a_serves_c = Arc::new(AtomicBool::new(true));
    let (wire_a, wire_b, wire_c) = (Wire::new(1), Wire::new(2), Wire::new(3));
    let a = RouteContent::new(Arc::new(Shelf::default()), wire_a.clone());
    let b = RouteContent::new(Arc::new(Shelf::default()), wire_b.clone());
    let c = RouteContent::new(Arc::new(Shelf::default()), wire_c.clone());
    wire_a.link(
        Arc::clone(&b),
        2,
        Arc::clone(&up),
        Arc::clone(&serves),
        Arc::clone(&a_serves_b),
    );
    wire_a.link(
        Arc::clone(&c),
        3,
        Arc::clone(&up),
        Arc::clone(&serves),
        Arc::clone(&a_serves_c),
    );
    wire_b.link(
        Arc::clone(&a),
        1,
        Arc::clone(&up),
        Arc::clone(&a_serves_b),
        Arc::clone(&serves),
    );
    wire_c.link(
        Arc::clone(&a),
        1,
        Arc::clone(&up),
        Arc::clone(&a_serves_c),
        Arc::clone(&serves),
    );
    Fan {
        a,
        b,
        c,
        wire_a,
        a_serves_b,
    }
}

/// Two connected hosts on one route; `b_may_read` says whether A serves B.
pub fn pair(b_may_read: bool) -> (Arc<RouteContent>, Arc<RouteContent>, Arc<Wire>) {
    let pair = hosts(true, b_may_read);
    (pair.a, pair.b, pair.wire_b)
}
