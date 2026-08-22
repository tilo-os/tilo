//! Peer pointers, from an app in a room to the compositor drawing it.
//!
//! Every quick-ui app publishes its pointer to the collaboration
//! broker, scoped to the room it was launched into. Until now nothing
//! read those frames except the other apps: the shell hosts the broker
//! but was not a subscriber to it, so `PresenceCtx::pointers` was empty
//! in every native session and the overlay that paints peers had
//! nothing to paint.
//!
//! This module closes both halves of that.
//!
//! **Outbound.** A [`RoomSubscription`] on the broker delivers the
//! presence its local apps publish, addressed by room through the
//! broker's own [`CrdtRoute::delivers_to`]. Each pointer frame is
//! republished to the room's other hosts as *this session's* cursor.
//!
//! Two things follow from republishing per session rather than per
//! connection, and both matter on screen:
//!
//! - **One person is one cursor.** The broker mints a user id per
//!   *connection*, so a person with two windows open is two ids
//!   locally. Those ids never leave the host; what goes on the wire is
//!   the session, which is the person. A cursor that moves from one
//!   app to another is therefore one cursor moving, not a second one
//!   appearing.
//! - **The coordinate is the one on screen.** An app reports its
//!   pointer in *surface-local* coordinates, which say nothing about
//!   where the pointer is on the desktop — the same (40, 30) is a
//!   different place in every window. Only the compositor knows that,
//!   so the compositor supplies it. The app's frame is what says
//!   *someone is pointing inside a shared app in this room*; the
//!   session says where. That is also what makes the crossing work:
//!   as the pointer leaves one app and enters another, the publisher
//!   changes and the position keeps tracking, so peers see one
//!   continuous cursor.
//!
//! **The desktop itself.** The republish above only fires while a
//! local app is publishing a pointer, so it says "someone is pointing
//! inside a shared app in this room" and nothing else. A room whose
//! members have not opened anything yet has no app publishing, so no
//! session ever put its cursor on the wire — which is why two people
//! could join the same room, move their mice, and watch an empty
//! desktop. The pointer is on the desktop long before it is over an
//! app, and it stays on the desktop as it crosses the strip and the
//! chrome between windows.
//!
//! [`PeerPresence::publish_local_cursor`] closes that: it is driven
//! level-triggered from the frame loop, so the session publishes where
//! its own pointer is whether or not anything is running in front of
//! it, addressed to the room whose workspace is on screen.
//! [`should_publish_cursor`] is the gate — a move goes out at most
//! every [`MIN_PUBLISH_GAP`], and a pointer that has stopped is
//! restated every [`CURSOR_KEEPALIVE`] so somebody who joins the room
//! afterwards sees the people already in it rather than an empty
//! desktop until one of them twitches.
//!
//! **Inbound.** [`PeerPresence::pump`] drains what arrived from other
//! hosts and folds it in through
//! [`ui::presence::PresenceCtx::apply_remote_presence`] — the same
//! decode an app runs — so a peer painted by the compositor and a peer
//! painted inside an app are the same peer, arrived by the same route.
//!
//! **Who a peer is.** Two things, kept apart on purpose.
//!
//! The *identity* is `peer@<handle>`, derived from the handle the
//! frame carries — the same spelling the broker gives a peer seen
//! inside an app. One person is one id on both paths, which is what
//! makes them one cursor in one colour rather than two people who
//! happen to move together, and it is why a peer that later gains a
//! name does not change colour in front of the people watching it.
//!
//! The *name* is a governance fact, read from the room document by
//! [`ShellNetwork::peer_labels`]. The room's authority writes it and
//! `apply_room_changes` refuses it from anybody else. A frame's own
//! `user` field — what the sending session says it is called — is
//! never used: a peer that can announce its own name can announce
//! anybody's, beside a caret editing a document whose access that
//! same room governs. A peer nobody has named keeps its id, which
//! `display_name` falls back to, so a pill is never blank and never
//! carries a name nobody chose.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ui::collaboration::{CollabEvent, PresencePayload};
use ui::presence::{PresenceCtx, PresenceKey};

use crate::shell::collab_broker::{BrokerHandle, RoomSubscription};
use crate::shell::network::ShellNetwork;

/// Bridges the session's collaboration broker and its peers for
/// pointer presence. Held by the compositor for the life of the
/// session.
pub struct PeerPresence {
    /// Where this session's pointer is, in the compositor's own scene
    /// space. Written by the input path on every motion, read by the
    /// broker sink on the reader thread that decoded a frame.
    cursor: Arc<Mutex<(f64, f64)>>,
    /// Inbound presence from other hosts, filled by
    /// [`ShellNetwork::tick`].
    inbound: Arc<Mutex<Vec<crate::shell::network::InboundPresence>>>,
    /// Kept alive for the session, and re-pointed as rooms change.
    subscription: Arc<RoomSubscription>,
    /// The rooms this session is in, mirrored from [`Self::set_rooms`].
    /// The subscription owns its own copy for delivery; this one
    /// scopes the bookkeeping below, so a room this session has left
    /// stops being remembered as something it has spoken about.
    rooms: Mutex<Vec<String>>,
    /// What this session last put on the wire, per room: where the
    /// pointer was and when it was said. Read by
    /// [`should_publish_cursor`] to decide whether to say it again.
    last_published: Mutex<HashMap<String, ((f64, f64), Instant)>>,
    /// Where the desktop pointer goes out. The app-driven republish
    /// captures its own clone in the sink closure; this is the same
    /// network, held so the frame loop can publish without one.
    net: Arc<ShellNetwork>,
}

/// Smallest gap between two pointer publishes for the same room —
/// ~30 Hz, matching the rate the toolkit throttles an app's pointer to
/// so a desktop cursor and an in-app cursor cost the same on the wire.
pub const MIN_PUBLISH_GAP: Duration = Duration::from_millis(33);

/// How long a stationary pointer goes unmentioned before the session
/// restates it. Presence is gossip: a host that joins the room after
/// somebody stopped moving has never heard where they are, and would
/// show an empty desktop until they moved again.
pub const CURSOR_KEEPALIVE: Duration = Duration::from_millis(1000);

/// Should this session put its pointer on the wire again?
///
/// `last` is what it last published for this room and when. Pure so
/// the decision can be asserted on directly: the difference between
/// "throttled" and "never published at all" is invisible on screen,
/// and it is exactly the difference this gate exists to get right.
pub fn should_publish_cursor(
    last: Option<((f64, f64), Instant)>,
    at: (f64, f64),
    now: Instant,
    min_gap: Duration,
    keepalive: Duration,
) -> bool {
    let Some((was, when)) = last else {
        // Never said anything about this room. Say it, so a peer who
        // is already listening sees this session arrive.
        return true;
    };
    let elapsed = now.saturating_duration_since(when);
    if was == at {
        // Standing still: restate it rarely, for whoever joined since.
        return elapsed >= keepalive;
    }
    elapsed >= min_gap
}

impl PeerPresence {
    /// Subscribe to `rooms` on `broker` and republish what the local
    /// apps publish in them to `net`.
    pub fn attach(broker: &BrokerHandle, net: Arc<ShellNetwork>, rooms: Vec<String>) -> Self {
        let cursor = Arc::new(Mutex::new((0.0_f64, 0.0_f64)));
        let inbound = net.inbound_presence_handle();
        let rooms_mirror = rooms.clone();
        let net_for_desktop = Arc::clone(&net);

        let sink_cursor = Arc::clone(&cursor);
        let subscription = broker.subscribe_rooms(
            rooms,
            Box::new(move |event, route| {
                // Only a pointer says where someone is. Carets and
                // selections are positions inside a document, which
                // is the document's business and not the desktop's.
                let CollabEvent::RemotePresenceUpdate {
                    payload: PresencePayload::Pointer { .. },
                    ..
                } = event
                else {
                    return;
                };
                // The room comes from the route the broker delivered
                // on, not from the key the app chose. An app that
                // publishes under a scope of its own (the capnp
                // cursor path tags a bare widget path) is still
                // pointing inside this room, and this is the only
                // place that knows which one.
                if route.cluster_id.is_empty() {
                    return;
                }
                let key = PresenceKey::Workspace {
                    id: route.cluster_id.clone(),
                };
                let Ok(key_str) = serde_json::to_string(&key) else {
                    return;
                };
                let point = *sink_cursor
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                net.publish_presence(key_str, point);
            }),
        );

        Self {
            cursor,
            inbound,
            subscription,
            rooms: Mutex::new(rooms_mirror),
            last_published: Mutex::new(HashMap::new()),
            net: net_for_desktop,
        }
    }

    /// Re-point the subscription at the session's current rooms.
    /// Called whenever the workspace set changes — a new workspace,
    /// or one that just joined a peer's room by invite.
    pub fn set_rooms(&self, rooms: Vec<String>) {
        if let Ok(mut g) = self.rooms.lock() {
            // Forget what was published to a room this session has
            // left, so rejoining it re-announces rather than staying
            // silent behind a keepalive from the previous visit.
            if let Ok(mut last) = self.last_published.lock() {
                last.retain(|room, _| rooms.iter().any(|r| r == room));
            }
            *g = rooms.clone();
        }
        self.subscription.set_rooms(rooms);
    }

    /// Put this session's own pointer on the wire, in the room the
    /// user is actually looking at. Returns whether anything went out.
    ///
    /// This is the desktop's publisher, and it is deliberately
    /// level-triggered rather than hooked onto motion: the frame loop
    /// already reads the pointer from the input state on every pass, so
    /// one call there covers every source that can move it — a real
    /// seat, a peer's injected input, the test control socket — and
    /// covers it whether or not an app happens to be running.
    ///
    /// One room, not every room this session is in. A pointer is
    /// somewhere, and the somewhere is the workspace on screen; sending
    /// it to the rest would put a cursor in front of people whose
    /// workspace this user is not even looking at, which is a peer who
    /// is not there.
    pub fn publish_local_cursor(&self, room: &str) -> bool {
        self.publish_local_cursor_at(room, Instant::now())
    }

    /// [`Self::publish_local_cursor`] with the clock supplied, so the
    /// throttle and the keepalive can be exercised without sleeping.
    pub fn publish_local_cursor_at(&self, room: &str, now: Instant) -> bool {
        if room.is_empty() {
            return false;
        }
        let at = *self
            .cursor
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut last = self
            .last_published
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if !should_publish_cursor(
            last.get(room).copied(),
            at,
            now,
            MIN_PUBLISH_GAP,
            CURSOR_KEEPALIVE,
        ) {
            return false;
        }
        let key = PresenceKey::Workspace { id: room.to_string() };
        let Ok(key_str) = serde_json::to_string(&key) else {
            return false;
        };
        self.net.publish_presence(key_str, at);
        last.insert(room.to_string(), (at, now));
        true
    }

    /// Record where this session's pointer is, in scene space.
    pub fn set_cursor(&self, x: f64, y: f64) {
        if let Ok(mut g) = self.cursor.lock() {
            *g = (x, y);
        }
    }

    /// Fold everything that arrived from other hosts into `presence`.
    /// Returns whether anything did, so the frame loop can treat a
    /// peer moving as a reason to repaint.
    pub fn pump(&self, presence: &mut PresenceCtx) -> bool {
        let drained: Vec<_> = {
            let Ok(mut buf) = self.inbound.lock() else {
                return false;
            };
            std::mem::take(&mut *buf)
        };
        let net = Arc::clone(&self.net);
        fold_inbound(drained, presence, &|room| net.peer_labels(room))
    }
}

/// What a room calls the peers in it, keyed by the handle presence
/// frames identify their sender by.
pub type PeerLabels = std::collections::BTreeMap<u64, String>;

/// Fold inbound presence into `presence`, returning whether anything
/// landed. Split out from [`PeerPresence::pump`] so the fold can be
/// driven without standing a network up; `pump` does nothing else.
///
/// `labels` answers "what does this room call that peer". It is a
/// parameter rather than a lookup done here so the fold stays testable
/// without a network, and so there is exactly one implementation of it
/// in production: the room document, via
/// [`ShellNetwork::peer_labels`].
pub fn fold_inbound(
    entries: Vec<crate::shell::network::InboundPresence>,
    presence: &mut PresenceCtx,
    labels: &dyn Fn(&str) -> PeerLabels,
) -> bool {
    let mut any = false;
    // Entries in one drain are nearly always the same room; resolving
    // per entry would re-read the tree once per pointer sample.
    let mut resolved: Option<(String, PeerLabels)> = None;
    for entry in entries {
        let Ok(key) = serde_json::from_str::<PresenceKey>(&entry.key) else {
            continue;
        };
        // The id is the sending *session*, spelled from the handle its
        // frames carry — the same spelling the broker gives a peer
        // seen inside an app. One person is therefore one id on both
        // paths, which is what makes them one cursor in one colour
        // instead of two people who happen to move together.
        //
        // Deliberately not `entry.user`. That is the sending session's
        // word for who is at it, and a peer that can announce its own
        // name can announce anybody's — beside a caret editing a
        // document whose access that same room governs.
        let user = format!("peer@{}", entry.peer);

        // What the room calls them. Names are a governance fact: the
        // room's authority writes them into the room document and
        // `apply_room_changes` refuses them from anyone else, so this
        // is the one place a label can come from.
        //
        // A peer nobody has named has no entry here and keeps the id
        // above. `PresenceCtx::display_name` falls back to it, so the
        // pill is never blank and never shows a name nobody chose.
        if let PresenceKey::Workspace { id: room } = &key {
            let known = match &resolved {
                Some((seen, map)) if seen == room => map,
                _ => {
                    resolved = Some((room.clone(), labels(room)));
                    &resolved.as_ref().expect("just set").1
                }
            };
            if let Some(name) = known.get(&entry.peer) {
                presence.set_display_name(
                    ui::permissions::UserId(user.clone()),
                    name.clone(),
                );
            }
        }

        presence.apply_remote_presence(
            user,
            key,
            PresencePayload::Pointer {
                x: entry.point.0 as f32,
                y: entry.point.1 as f32,
            },
        );
        any = true;
    }
    any
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::network::InboundPresence;
    use ui::permissions::UserId;

    #[test]
    fn a_session_that_has_said_nothing_speaks() {
        // The empty desktop this exists for. Nothing has published,
        // so the first pass must put the pointer on the wire — a
        // throttle that swallowed it would leave a peer looking at
        // nothing until the mouse happened to move again.
        assert!(should_publish_cursor(
            None,
            (10.0, 10.0),
            Instant::now(),
            MIN_PUBLISH_GAP,
            CURSOR_KEEPALIVE,
        ));
    }

    #[test]
    fn a_move_inside_the_gap_is_held() {
        let t0 = Instant::now();
        assert!(
            !should_publish_cursor(
                Some(((10.0, 10.0), t0)),
                (11.0, 10.0),
                t0 + Duration::from_millis(5),
                MIN_PUBLISH_GAP,
                CURSOR_KEEPALIVE,
            ),
            "a pointer moving at frame rate must not put every sample on the wire"
        );
    }

    #[test]
    fn a_move_past_the_gap_goes_out() {
        let t0 = Instant::now();
        assert!(should_publish_cursor(
            Some(((10.0, 10.0), t0)),
            (400.0, 300.0),
            t0 + MIN_PUBLISH_GAP,
            MIN_PUBLISH_GAP,
            CURSOR_KEEPALIVE,
        ));
    }

    #[test]
    fn a_pointer_that_stopped_is_restated_for_whoever_joined_since() {
        // Presence is gossip, so a host that joins after somebody
        // stopped moving never heard where they are. Standing still
        // must not mean going silent, or the newcomer sees an empty
        // desktop until an existing member twitches.
        let t0 = Instant::now();
        let at = (10.0, 10.0);
        assert!(
            !should_publish_cursor(
                Some((at, t0)),
                at,
                t0 + Duration::from_millis(200),
                MIN_PUBLISH_GAP,
                CURSOR_KEEPALIVE,
            ),
            "a still pointer must not republish at frame rate"
        );
        assert!(
            should_publish_cursor(
                Some((at, t0)),
                at,
                t0 + CURSOR_KEEPALIVE,
                MIN_PUBLISH_GAP,
                CURSOR_KEEPALIVE,
            ),
            "but it must be restated once the keepalive is due"
        );
    }

    /// A frame as it arrives off the wire. `user` is what the *sender*
    /// claims to be called, and is deliberately never used as a label —
    /// the tests below pass a wrong one on purpose.
    fn arrived(user: &str, peer: u64, room: &str, at: (f64, f64)) -> InboundPresence {
        InboundPresence {
            peer,
            user: user.into(),
            key: serde_json::to_string(&PresenceKey::Workspace { id: room.into() })
                .expect("a workspace key serialises"),
            point: at,
            ts: 0,
        }
    }

    /// No room has named anybody.
    fn unnamed(_room: &str) -> PeerLabels {
        PeerLabels::new()
    }

    /// The room's names, as its authority wrote them.
    fn named(pairs: &[(u64, &str)]) -> impl Fn(&str) -> PeerLabels + use<> {
        let map: PeerLabels = pairs.iter().map(|(h, n)| (*h, (*n).to_string())).collect();
        move |_room: &str| map.clone()
    }

    #[test]
    fn a_peer_lands_on_the_room_the_overlay_reads() {
        // The overlay looks pointers up by room, because the room is
        // what an app publishes under. A frame that landed anywhere
        // else would be held and never drawn.
        let room = "3f2b1c00-0000-4000-8000-000000000001";
        let mut presence = PresenceCtx::default();
        assert!(fold_inbound(
            vec![arrived("bob", 7, room, (120.0, 240.0))],
            &mut presence,
            &unnamed,
        ));

        let key = PresenceKey::Workspace { id: room.into() };
        let peers = presence
            .pointers
            .get(&key)
            .expect("the pointer should be filed under the room");
        assert_eq!(peers.len(), 1);
        assert_eq!(peers[0].0, UserId("peer@7".into()));
        assert_eq!(peers[0].1, (120.0, 240.0));
    }

    #[test]
    fn a_peer_is_called_what_the_room_calls_it() {
        // Names are governance: the room's authority writes them into
        // the room document and nobody else may. So the label comes
        // from there, and the sender's own word for itself — "bob"
        // below — is not consulted.
        let room = "3f2b1c00-0000-4000-8000-000000000001";
        let mut presence = PresenceCtx::default();
        fold_inbound(
            vec![arrived("bob", 7, room, (10.0, 10.0))],
            &mut presence,
            &named(&[(7, "Xaver")]),
        );
        let peers = presence
            .pointers
            .get(&PresenceKey::Workspace { id: room.into() })
            .expect("a pointer");
        assert_eq!(
            presence.display_name(&peers[0].0),
            "Xaver",
            "the pill must read what the room says, not what the sender claimed"
        );
    }

    #[test]
    fn a_sender_cannot_name_itself() {
        // The spoofable half. A session that says it is somebody must
        // not be drawn as that somebody; an unnamed peer keeps its id.
        let room = "3f2b1c00-0000-4000-8000-000000000001";
        let mut presence = PresenceCtx::default();
        fold_inbound(
            vec![arrived("Quinten", 7, room, (10.0, 10.0))],
            &mut presence,
            &unnamed,
        );
        let peers = presence
            .pointers
            .get(&PresenceKey::Workspace { id: room.into() })
            .expect("a pointer");
        assert_eq!(
            presence.display_name(&peers[0].0),
            "peer@7",
            "a name the sender chose for itself must not reach the screen"
        );
    }

    #[test]
    fn a_name_does_not_move_a_peer_to_a_new_colour() {
        // A peer that gains a name must not change colour in front of
        // the people watching it, so the name is a label on a stable
        // id rather than a new identity.
        let room = "3f2b1c00-0000-4000-8000-000000000001";
        let mut before = PresenceCtx::default();
        fold_inbound(
            vec![arrived("", 7, room, (10.0, 10.0))],
            &mut before,
            &unnamed,
        );
        let mut after = PresenceCtx::default();
        fold_inbound(
            vec![arrived("", 7, room, (10.0, 10.0))],
            &mut after,
            &named(&[(7, "Xaver")]),
        );
        let key = PresenceKey::Workspace { id: room.into() };
        let a = &before.pointers.get(&key).expect("a pointer")[0].0;
        let b = &after.pointers.get(&key).expect("a pointer")[0].0;
        assert_eq!(a, b, "the identity must be the same before and after a name");
        assert_eq!(
            before.color_for(a),
            after.color_for(b),
            "and so must the colour"
        );
    }

    #[test]
    fn naming_a_peer_does_not_conjure_a_cursor() {
        // The roster says who is here, not where they are. A room that
        // has named somebody who has not pointed must paint nothing.
        let room = "3f2b1c00-0000-4000-8000-000000000001";
        let mut presence = PresenceCtx::default();
        assert!(!fold_inbound(
            Vec::new(),
            &mut presence,
            &named(&[(7, "Xaver"), (8, "Quinten")]),
        ));
        assert!(
            presence.pointers.is_empty(),
            "a name is not a position: {:?}",
            presence.pointers
        );
    }

    #[test]
    fn one_session_stays_one_peer_across_two_apps() {
        // A person with two windows open publishes from two processes.
        // What reaches another host is the session, so the two moves
        // are one cursor travelling — not two cursors sitting where
        // each app last saw it.
        let room = "3f2b1c00-0000-4000-8000-000000000001";
        let mut presence = PresenceCtx::default();
        fold_inbound(
            vec![
                arrived("bob", 7, room, (100.0, 100.0)),
                arrived("bob", 7, room, (600.0, 300.0)),
            ],
            &mut presence,
            &unnamed,
        );

        let key = PresenceKey::Workspace { id: room.into() };
        let peers = presence.pointers.get(&key).expect("a pointer");
        assert_eq!(
            peers.len(),
            1,
            "one person driving two apps must be one peer, got {peers:?}"
        );
        assert_eq!(
            peers[0].1,
            (600.0, 300.0),
            "and their cursor must be where they last pointed"
        );
    }

    #[test]
    fn two_rooms_do_not_share_peers() {
        let here = "3f2b1c00-0000-4000-8000-000000000001";
        let there = "3f2b1c00-0000-4000-8000-0000000000ff";
        let mut presence = PresenceCtx::default();
        fold_inbound(
            vec![
                arrived("bob", 7, here, (10.0, 10.0)),
                arrived("carol", 8, there, (20.0, 20.0)),
            ],
            &mut presence,
            &unnamed,
        );
        let peers = presence
            .pointers
            .get(&PresenceKey::Workspace { id: here.into() })
            .expect("a pointer in this room");
        assert_eq!(peers.len(), 1);
        assert_eq!(peers[0].0, UserId("peer@7".into()));
    }

    #[test]
    fn a_session_that_names_nobody_is_labelled_with_its_handle() {
        // Falling back to the session handle keeps the caret labelled
        // without inventing a person who did not announce themselves.
        let room = "3f2b1c00-0000-4000-8000-000000000001";
        let mut presence = PresenceCtx::default();
        fold_inbound(
            vec![arrived("", 0x2a, room, (5.0, 5.0))],
            &mut presence,
            &unnamed,
        );
        let peers = presence
            .pointers
            .get(&PresenceKey::Workspace { id: room.into() })
            .expect("a pointer");
        assert_eq!(peers[0].0, UserId("peer@42".into()));
        assert_eq!(
            presence.display_name(&peers[0].0),
            "peer@42",
            "never blank"
        );
    }
}
