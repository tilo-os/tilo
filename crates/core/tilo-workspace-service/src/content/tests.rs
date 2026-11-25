//! Record content between two hosts joined by an in-process route.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use super::*;
use neighbour_wire::NeighbourMsg;
use neighbour_wire::peer_handle;

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
pub(crate) struct Wire {
    /// This host's handle on the route.
    me: ContentPeer,
    links: Mutex<Vec<Link>>,
    /// What this host sent, for the assertions.
    pub(crate) sent: Mutex<Vec<String>>,
}

impl Wire {
    pub(crate) fn new(me: ContentPeer) -> Arc<Self> {
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
pub(crate) struct Fan {
    pub(crate) a: Arc<RouteContent>,
    pub(crate) b: Arc<RouteContent>,
    pub(crate) c: Arc<RouteContent>,
    /// What A sent, for the assertions.
    pub(crate) wire_a: Arc<Wire>,
    /// Whether A serves B, for a test to take away while B waits.
    pub(crate) a_serves_b: Arc<AtomicBool>,
}

pub(crate) fn fan() -> Fan {
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
pub(crate) fn pair(b_may_read: bool) -> (Arc<RouteContent>, Arc<RouteContent>, Arc<Wire>) {
    let pair = hosts(true, b_may_read);
    (pair.a, pair.b, pair.wire_b)
}

/// A device serving a workspace, as a member reaches it, as the runtime
/// does: it answers the member's asks with the shared
/// [`neighbour_wire::content::answer`], one window of what the device lacks
/// at a time, from a store that may fill after a first ask.
struct ServingPeers {
    /// What the serving device keeps.
    shelf: Arc<Shelf>,
    /// The serving device, which answers.
    serving: [u8; 32],
    /// The device that asks, as its own requests name it.
    asker: [u8; 32],
    /// The device a misaddressed ask names instead, for a test of the
    /// refusal; the asker itself when `None`.
    named: Option<[u8; 32]>,
    /// The device's content service, where the answers go.
    content: Mutex<Option<std::sync::Weak<RouteContent>>>,
}

impl ContentPeers for ServingPeers {
    fn want(&self, route: &CrdtRoute, hash: &str, have: &[Span]) {
        let request = postcard::to_allocvec(&NeighbourMsg::DocumentContentWants {
            requester: peer_handle(&self.named.unwrap_or(self.asker)),
            cluster_id: route.cluster_id.clone(),
            wants: vec![neighbour_wire::content::Wanted {
                app_id: route.app_id.clone(),
                hash: hash.to_owned(),
                have: have.to_vec(),
            }],
        })
        .unwrap();
        let scope = ContentScope::of(route);
        let frames =
            neighbour_wire::content::answer(&self.serving, &self.asker, &request, |_, _, hash| {
                self.shelf.read(&scope, hash)
            });
        let Some(content) = self
            .content
            .lock()
            .unwrap()
            .as_ref()
            .and_then(std::sync::Weak::upgrade)
        else {
            return;
        };
        for frame in frames {
            match postcard::from_bytes::<NeighbourMsg>(&frame) {
                Ok(NeighbourMsg::DocumentContentPieces { pieces, .. }) => {
                    for piece in pieces {
                        content.peer_piece(
                            &CrdtRoute::app(piece.app_id, route.cluster_id.clone()),
                            &ContentPiece {
                                hash: piece.hash,
                                total: piece.total,
                                offset: piece.offset,
                                bytes: piece.bytes,
                            },
                        );
                    }
                }
                Ok(NeighbourMsg::DocumentContentLacks { hash, .. }) => {
                    content.peer_lacks(route, peer_handle(&self.serving), &hash);
                }
                Ok(NeighbourMsg::DocumentContentRefused { hash, .. }) => {
                    content.peer_refused(route, peer_handle(&self.serving), &hash);
                }
                _ => {}
            }
        }
    }

    fn answer(&self, _route: &CrdtRoute, _peer: ContentPeer, _piece: ContentPiece) {}

    fn lacks(&self, _route: &CrdtRoute, _peer: ContentPeer, _hash: &str) {}

    fn sources(&self, _route: &CrdtRoute) -> ContentSources {
        ContentSources {
            peers: BTreeSet::new(),
            hosts: [peer_handle(&self.serving)].into_iter().collect(),
        }
    }
}

/// A device whose content the workspace's serving device answers, and that
/// device's store: the bytes reach it when content is kept there, as a
/// member's content reaches the device serving the workspace.
fn served() -> (Arc<RouteContent>, Arc<ServingPeers>) {
    served_naming(None)
}

/// [`served`] whose asks name `named` instead of the device that sends them.
fn served_naming(named: Option<[u8; 32]>) -> (Arc<RouteContent>, Arc<ServingPeers>) {
    let peers = Arc::new(ServingPeers {
        shelf: Arc::new(Shelf::default()),
        serving: [9; 32],
        asker: [8; 32],
        named,
        content: Mutex::default(),
    });
    let answering: Arc<dyn ContentPeers> = peers.clone();
    let content = RouteContent::new(Arc::new(Shelf::default()), answering);
    *peers.content.lock().unwrap() = Some(Arc::downgrade(&content));
    (content, peers)
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
    CrdtRoute::record("so.tilo.slides", "workspace-1", "deck-1")
}

fn picture(len: usize) -> Vec<u8> {
    (0..len)
        .map(|index| u8::try_from(index % 251).unwrap())
        .collect()
}

tilo_test_env::scenario! {
    fn content_reaches_only_a_host_that_may_read_its_record_in_verified_pieces;

    phase content_one_host_was_offered_reaches_the_other_in_verified_pieces;
    {
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

    phase a_device_that_may_not_read_the_record_gets_nothing;
    {
        let (a, b, _) = pair(false);
        let bytes = picture(100);
        let hash = hash_bytes(&bytes);
        a.offer(&route(), &hash, &bytes).unwrap();
        let mut arrivals = b.subscribe(&route());
        b.want(&route(), &hash);
        assert!(arrival(&mut arrivals).is_none());
    }

    phase content_of_another_record_scope_is_not_served;
    {
        let (a, b, _) = pair(true);
        let bytes = picture(100);
        let hash = hash_bytes(&bytes);
        a.offer(
            &CrdtRoute::record("so.tilo.slides", "another-workspace", "deck-1"),
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

    phase bytes_that_are_not_the_content_are_refused;
    {
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

    phase pieces_nobody_asked_for_are_dropped;
    {
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

    phase a_person_record_s_content_lives_in_the_person_s_workspace;
    // A person record's content follows the record into the person's workspace,
    // whichever workspace the window that stored it shows: a record of the window's
    // workspace does not reach it.
    {
        let (a, b, _) = pair(true);
        let person = CrdtRoute::record("so.tilo.tube", "personal-cluster", "history");
        let window = CrdtRoute::record("so.tilo.tube", "workspace-1", "");
        let poster = picture(PIECE_BYTES + 3);
        let hash = hash_bytes(&poster);
        a.offer(&person, &hash, &poster).unwrap();

        let mut in_window = b.subscribe(&window);
        b.want(&window, &hash);
        assert!(
            arrival(&mut in_window).is_none(),
            "the window's workspace keeps none of it"
        );

        let mut in_person = b.subscribe(&person);
        b.want(&person, &hash);
        assert_eq!(arrival(&mut in_person).unwrap().bytes, poster);
    }
}

tilo_test_env::scenario! {
    fn a_want_follows_the_devices_that_could_answer_it;

    phase a_want_waits_for_its_holder_and_is_fetched_when_it_connects;
    // A want follows the devices that could answer it, never a timer: it
    // waits while none is connected, is fetched the moment one connects, and
    // is unavailable once every connected one said it keeps none.
    {
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

    phase a_want_nobody_connected_keeps_is_unavailable_until_one_that_might_connects;
    {
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

    phase content_a_device_lacked_when_asked_follows_once_it_keeps_it;
    // A record names content before the device that wrote it keeps the bytes
    // where peers are served from. A peer that asked in between hears that the
    // device lacks it, and asks no more while no device connects: the device
    // hands the content over once it keeps it.
    {
        let (a, b, _) = pair(true);
        let bytes = picture(PIECE_BYTES + 5);
        let hash = hash_bytes(&bytes);
        let mut arrivals = b.subscribe(&route());
        b.want(&route(), &hash);
        assert_eq!(
            states(&mut arrivals),
            [ContentState::Pending, ContentState::Unavailable]
        );

        a.offer(&route(), &hash, &bytes).unwrap();
        let arrived = arrival(&mut arrivals).expect("the content followed");
        assert_eq!(arrived.bytes, bytes);
        assert_eq!(
            b.shelf.read(&ContentScope::of(&route()), &hash),
            Some(bytes)
        );
    }

    phase content_is_not_owed_to_a_device_that_may_not_read;
    // What a device lacked is owed only to peers that may read the record.
    {
        let (a, b, _) = pair(false);
        let bytes = picture(100);
        let hash = hash_bytes(&bytes);
        let mut arrivals = b.subscribe(&route());
        b.want(&route(), &hash);
        a.offer(&route(), &hash, &bytes).unwrap();
        assert!(arrival(&mut arrivals).is_none());
    }

    phase content_a_peer_may_no_longer_read_is_not_handed_over;
    // A peer that asked while the device kept none of the content, and lost
    // the record in the meantime, is not handed the bytes: the peer that may
    // still read it is.
    {
        let fan = fan();
        let bytes = picture(PIECE_BYTES + 5);
        let hash = hash_bytes(&bytes);
        let mut to_b = fan.b.subscribe(&route());
        let mut to_c = fan.c.subscribe(&route());
        fan.b.want(&route(), &hash);
        fan.c.want(&route(), &hash);
        assert_eq!(
            states(&mut to_b),
            [ContentState::Pending, ContentState::Unavailable],
            "the device that keeps none of it said so"
        );
        assert_eq!(
            states(&mut to_c),
            [ContentState::Pending, ContentState::Unavailable]
        );
        fan.wire_a.sent.lock().unwrap().clear();

        // B loses the record while it waits; C keeps it.
        fan.a_serves_b.store(false, Ordering::Release);
        fan.a.offer(&route(), &hash, &bytes).unwrap();

        assert!(
            arrival(&mut to_b).is_none(),
            "the device that may no longer read the record gets nothing"
        );
        assert_eq!(
            fan.b.shelf.read(&ContentScope::of(&route()), &hash),
            None,
            "and keeps none of it"
        );
        assert_eq!(
            arrival(&mut to_c)
                .expect("the device that may still read it gets it")
                .bytes,
            bytes
        );
        assert!(
            fan.wire_a
                .sent
                .lock()
                .unwrap()
                .iter()
                .all(|sent| sent.ends_with("to 3")),
            "the bytes went to the device that may still read them"
        );
    }
}

tilo_test_env::scenario! {
    fn a_want_answered_with_nothing_is_asked_again_until_the_content_arrives;

    phase a_holder_that_keeps_the_bytes_after_saying_it_lacks_is_asked_again;
    // The device serving a workspace answered once that it kept none of the
    // content — the writer had published its document and not yet the bytes.
    // Content that reaches that device later is the asker's once the want is
    // asked again, never only after a restart.
    {
        let (content, serving) = served();
        let bytes = picture(PIECE_BYTES + 17);
        let hash = hash_bytes(&bytes);
        let mut news = content.subscribe(&route());
        content.want(&route(), &hash);
        assert_eq!(
            states(&mut news),
            [ContentState::Pending, ContentState::Waiting],
            "the serving device keeps none of it yet"
        );

        // The bytes reach the serving device after the first ask.
        serving.shelf
            .keep(&ContentScope::of(&route()), &hash, &bytes)
            .unwrap();
        content.retry(
            tilo_clock::unix_now_ms().saturating_add(patience_ms(0).saturating_mul(2)),
        );
        let arrived = arrival(&mut news).expect("the device asked again and got it");
        assert_eq!(arrived.bytes, bytes);
        assert_eq!(
            content.shelf.read(&ContentScope::of(&route()), &hash),
            Some(bytes),
            "and keeps it under the document's scope"
        );
    }

    phase content_larger_than_one_answer_arrives_whole_in_windows;
    // An answer carries at most a window of the pieces an asker lacks; the
    // asker asks again for what is missing until the content is whole.
    {
        let (content, serving) = served();
        let bytes = picture(PIECE_BYTES * (PIECES_PER_ANSWER + 3));
        let hash = hash_bytes(&bytes);
        serving
            .shelf
            .keep(&ContentScope::of(&route()), &hash, &bytes)
            .unwrap();
        let mut news = content.subscribe(&route());
        content.want(&route(), &hash);
        assert!(
            arrival(&mut news).is_none(),
            "one answer cannot carry {} bytes",
            bytes.len()
        );

        let mut now = tilo_clock::unix_now_ms();
        let mut rounds = 0;
        let mut arrived = None;
        while arrived.is_none() {
            assert!(
                rounds < 4,
                "the content crossed in windows rather than never"
            );
            rounds += 1;
            now = now.saturating_add(patience_ms(rounds));
            content.retry(now);
            arrived = arrival(&mut news);
        }
        assert_eq!(arrived.unwrap().bytes, bytes, "every piece arrived once");
    }

    phase a_misaddressed_ask_is_refused_to_the_device_that_asked;
    // An ask that names another device than the one that sent it can never
    // be answered as addressed: the serving device refuses it to the sender,
    // which counts that device out instead of waiting on an answer for ever.
    {
        let (content, serving) = served_naming(Some([7; 32]));
        let bytes = picture(PIECE_BYTES + 5);
        let hash = hash_bytes(&bytes);
        serving
            .shelf
            .keep(&ContentScope::of(&route()), &hash, &bytes)
            .unwrap();

        // What the serving device answers such an ask, word for word.
        let request = postcard::to_allocvec(&NeighbourMsg::DocumentContentWants {
            requester: peer_handle(&[7; 32]),
            cluster_id: route().cluster_id,
            wants: vec![neighbour_wire::content::Wanted {
                app_id: route().app_id,
                hash: hash.clone(),
                have: Vec::new(),
            }],
        })
        .unwrap();
        let frames = neighbour_wire::content::answer(&serving.serving, &serving.asker, &request, |_, _, _| {
            Some(bytes.clone())
        });
        let [frame] = frames.as_slice() else {
            panic!("one refusal answers a misaddressed ask");
        };
        let NeighbourMsg::DocumentContentRefused {
            responder,
            requester,
            hash: refused,
            ..
        } = postcard::from_bytes(frame).unwrap()
        else {
            panic!("the answer is a refusal, not content or silence");
        };
        assert_eq!(
            (responder, requester),
            (peer_handle(&serving.serving), peer_handle(&serving.asker)),
            "the refusal is addressed to the device that sent the ask"
        );
        assert_eq!(refused, hash);

        let mut news = content.subscribe(&route());
        content.want(&route(), &hash);
        assert_eq!(
            states(&mut news),
            [ContentState::Pending, ContentState::Waiting],
            "the device acted on the refusal instead of waiting on the serving device"
        );
    }
}
