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
    ///
    /// `None` until the pointer has actually been somewhere. That is
    /// not the same as `(0, 0)`, and the difference is the whole
    /// reason this is an `Option`: publishing is level-triggered, so a
    /// session whose pointer has never moved would otherwise announce
    /// the origin as a real position and every peer would paint a
    /// cursor in their top-left corner for somebody who is not there.
    /// An agent session never moves a mouse at all, so for it the
    /// origin pill was permanent.
    cursor: Arc<Mutex<Option<(f64, f64)>>>,
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

/// What this session should put on the wire for a room right now, if
/// anything.
///
/// `at` is `None` until the pointer has actually been somewhere, and
/// that is the case this exists to separate. Publishing is
/// level-triggered and the first pass is deliberately ungated (so a
/// peer already in the room sees this session arrive), which together
/// meant a pointer that had never moved announced the *origin* as a
/// real position — a cursor on every peer's screen for somebody who
/// was not there, and permanently so for a session that never moves a
/// mouse.
///
/// The origin itself is a perfectly good place to point, so it is
/// `None` that is silent here, not `Some((0.0, 0.0))`.
fn cursor_to_publish(
    at: Option<(f64, f64)>,
    last: Option<((f64, f64), Instant)>,
    now: Instant,
    min_gap: Duration,
    keepalive: Duration,
) -> Option<(f64, f64)> {
    let at = at?;
    should_publish_cursor(last, at, now, min_gap, keepalive).then_some(at)
}

impl PeerPresence {
    /// Subscribe to `rooms` on `broker` and republish what the local
    /// apps publish in them to `net`.
    pub fn attach(broker: &BrokerHandle, net: Arc<ShellNetwork>, rooms: Vec<String>) -> Self {
        let cursor: Arc<Mutex<Option<(f64, f64)>>> = Arc::new(Mutex::new(None));
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
                // The app said somebody is pointing inside a shared
                // app in this room; this session says where. If this
                // session's own pointer has never been anywhere there
                // is no where to supply, and the origin is not a
                // substitute for one.
                let Some(point) = *sink_cursor
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                else {
                    return;
                };
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
        let Some(at) = cursor_to_publish(
            at,
            last.get(room).copied(),
            now,
            MIN_PUBLISH_GAP,
            CURSOR_KEEPALIVE,
        ) else {
            return false;
        };
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
            *g = Some((x, y));
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
        // The rooms this session is actually in. A frame naming any
        // other room is not this session's to hold — see
        // `fold_inbound`.
        let mine: Vec<String> = self
            .rooms
            .lock()
            .map(|g| g.clone())
            .unwrap_or_default();
        fold_inbound(
            drained,
            presence,
            &|room| net.peer_labels(room),
            &|room| mine.iter().any(|r| r == room),
        )
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
    member_of: &dyn Fn(&str) -> bool,
) -> bool {
    let mut any = false;
    // Entries in one drain are nearly always the same room; resolving
    // per entry would re-read the tree once per pointer sample.
    let mut resolved: Option<(String, PeerLabels)> = None;
    for entry in entries {
        let Ok(key) = serde_json::from_str::<PresenceKey>(&entry.key) else {
            continue;
        };
        // Only rooms this session is in.
        //
        // A session publishes its pointer under whichever room is on
        // its own screen, and that is frequently not this one: a peer
        // is in its own room before it joins yours, and stays in it
        // afterwards. Folding those in anyway filed a pointer under a
        // room this session is not a member of, where nothing ever
        // replaced it and nothing ever cleaned it up — so one person
        // accumulated an entry per room they had ever pointed in, and
        // any reader that spans keys (the state dump, and therefore
        // the recorder) saw one peer as several.
        //
        // The overlay never painted them, because it looks peers up by
        // the active room. That is exactly what made this survive: the
        // ghosts were only visible to something reading the store
        // rather than the screen.
        //
        // It is also the access rule. Presence arrives from whoever is
        // on the wire, so without this a remote host could write
        // entries into this session's presence store under any room id
        // it liked, including rooms it is not in and this session has
        // never heard of.
        if let PresenceKey::Workspace { id: room } = &key {
            if !member_of(room) {
                continue;
            }
        }
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

    /// A session whose pointer has never been anywhere says nothing.
    ///
    /// Publishing is level-triggered, and the first pass is deliberately
    /// ungated so somebody already in the room sees this session arrive.
    /// That combination used to announce the *origin* as a real
    /// position, because a pointer that had never moved still had
    /// coordinates. Every peer then painted a cursor in their top-left
    /// corner for a person who was not there — permanently, for an
    /// agent session that never moves a mouse at all.
    ///
    /// The distinction the fix rests on is between "at (0, 0)" and
    /// "nowhere yet", so both halves are asserted: silence before the
    /// pointer has moved, and the origin published like any other
    /// position once somebody actually puts it there.
    #[test]
    fn a_pointer_that_has_never_moved_is_not_announced_at_the_origin() {
        let now = Instant::now();

        assert_eq!(
            cursor_to_publish(None, None, now, MIN_PUBLISH_GAP, CURSOR_KEEPALIVE),
            None,
            "a session whose pointer has never moved must not announce one"
        );

        // And the origin is not banned — it is a real place. A pointer
        // somebody actually moved there is published like any other, or
        // a peer parked in the corner would vanish.
        assert_eq!(
            cursor_to_publish(
                Some((0.0, 0.0)),
                None,
                now,
                MIN_PUBLISH_GAP,
                CURSOR_KEEPALIVE
            ),
            Some((0.0, 0.0)),
            "a pointer somebody actually moved to the origin is still a pointer"
        );
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

    /// This session is in every room a frame might name. The default
    /// for tests about naming and placement, which are not about
    /// membership; the membership tests state their own.
    fn any_room(_room: &str) -> bool {
        true
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
            &any_room,
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

    /// One person is one pointer, asserted where the desktop pointer
    /// enters the compositor.
    ///
    /// A frame folded here lands in two stores at once, and both are
    /// wanted: `pointers` is what the overlay paints, `remote_cursors`
    /// is what "follow that peer" reads. What must not happen is both
    /// being *painted*, because the toolkit threads this very context
    /// into the app-level overlay as well — the shell is itself a
    /// `ui::App` — so a single arrival came out as two identical dots
    /// on top of each other.
    ///
    /// The duplicate was invisible for months because the two layers
    /// spelled a peer's identity differently and it read as two
    /// people. Naming them consistently is what exposed it, which is
    /// why the rule is pinned to a test rather than to an eye.
    #[test]
    fn a_desktop_pointer_is_painted_by_the_overlay_and_not_again_by_the_window() {
        let room = "3f2b1c00-0000-4000-8000-000000000001";
        let mut presence = PresenceCtx::default();
        fold_inbound(
            vec![arrived("bob", 7, room, (120.0, 240.0))],
            &mut presence,
            &named(&[(7, "bob")]),
            &any_room,
        );

        // The overlay has them.
        let peers = presence
            .pointers
            .get(&PresenceKey::Workspace { id: room.into() })
            .expect("the overlay's store must hold the peer it is to paint");
        assert_eq!(peers.len(), 1, "one arrival, one peer");

        // And exactly one record, which the window layer declines to
        // paint because the overlay above it already is.
        assert_eq!(
            presence.remote_cursors.len(),
            1,
            "one person must not become two cursors"
        );
        let cursor = &presence.remote_cursors[0];
        assert_eq!(cursor.user_id, peers[0].0.0, "and they are the same person");
        assert!(
            !cursor.painted_by_window(),
            "the compositor paints this pointer; drawing it here too is the duplicate",
        );

        // Still followable — declining to paint is not discarding.
        assert_eq!(
            presence.peer_pointer(&cursor.user_id),
            Some([120.0, 240.0]),
            "following a peer must still land where their cursor shows",
        );
    }

    /// One person is one row, however many rooms they have pointed in.
    ///
    /// A peer publishes under whichever room is on its own screen, so a
    /// session that was in its own room before joining this one puts
    /// frames on the wire for both. Folding the foreign one in filed a
    /// second entry for the same person under a room this session is
    /// not in — where nothing replaced it and nothing expired it — and
    /// any reader that spans keys saw one peer as two.
    ///
    /// The overlay never painted the ghost, because it looks peers up
    /// by the active room. That is precisely why this is asserted over
    /// the whole store and not over one room: the failure was only ever
    /// visible to something reading the store rather than the screen.
    #[test]
    fn one_person_is_one_entry_across_every_room_in_the_store() {
        let mine = "3f2b1c00-0000-4000-8000-000000000001";
        let theirs = "3f2b1c00-0000-4000-8000-0000000000ff";
        let mut presence = PresenceCtx::default();

        // The same peer, pointing in this room and in one of its own.
        fold_inbound(
            vec![
                arrived("bob", 7, mine, (10.0, 20.0)),
                arrived("bob", 7, theirs, (30.0, 40.0)),
            ],
            &mut presence,
            &unnamed,
            &|room| room == mine,
        );

        let rows: Vec<_> = presence
            .pointers
            .iter()
            .flat_map(|(k, v)| v.iter().map(move |(u, p)| (k.clone(), u.clone(), *p)))
            .collect();
        assert_eq!(
            rows.len(),
            1,
            "one person must be one entry in the whole store, got {rows:?}"
        );
        assert_eq!(rows[0].1, UserId("peer@7".into()));
        assert_eq!(
            rows[0].2,
            (10.0, 20.0),
            "and it is the room this session is in"
        );
    }

    /// A room this session is not in cannot be written into its
    /// presence store at all. Presence arrives from whoever is on the
    /// wire, so without this a remote host chooses which rooms show up
    /// in somebody else's state.
    #[test]
    fn a_room_this_session_is_not_in_is_refused() {
        let elsewhere = "3f2b1c00-0000-4000-8000-0000000000ff";
        let mut presence = PresenceCtx::default();

        let any = fold_inbound(
            vec![arrived("bob", 7, elsewhere, (10.0, 10.0))],
            &mut presence,
            &unnamed,
            &|_room| false,
        );

        assert!(
            !any,
            "a frame for a room this session is not in changes nothing"
        );
        assert!(
            presence.pointers.is_empty(),
            "and leaves no entry behind: {:?}",
            presence.pointers
        );
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
            &named(&[(7, "bob")]),
            &any_room,
        );
        let peers = presence
            .pointers
            .get(&PresenceKey::Workspace { id: room.into() })
            .expect("a pointer");
        assert_eq!(
            presence.display_name(&peers[0].0),
            "bob",
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
            vec![arrived("ada", 7, room, (10.0, 10.0))],
            &mut presence,
            &unnamed,
            &any_room,
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
            &any_room,
        );
        let mut after = PresenceCtx::default();
        fold_inbound(
            vec![arrived("", 7, room, (10.0, 10.0))],
            &mut after,
            &named(&[(7, "bob")]),
            &any_room,
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
            &named(&[(7, "bob"), (8, "ada")]),
            &any_room,
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
            &any_room,
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
            &any_room,
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
            &any_room,
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

/// Two sessions, and a pointer that has to get from one to the other.
///
/// Everything above this point is asserted against *one* session's
/// store, with the room set and the room's names handed in as
/// closures. That is worth having and it is not enough: a change can
/// leave every one of those green while nothing crosses between two
/// machines, because what a session publishes and what a session
/// accepts are decided in two different places and only agree by
/// construction. The membership gate is exactly that shape — the
/// publisher addresses the room it is *looking at*, the folder accepts
/// the rooms it is *in*, and if those two ever name the room
/// differently every frame is dropped in silence.
///
/// So this drives the whole path end to end: one session's real
/// publisher, the wire message it produces, the other session's real
/// inbound queue and fold, with each session's room set derived from
/// its own workspace tree the way the frame loop derives it.
#[cfg(test)]
mod two_sessions {
    use super::*;
    use crate::network::actor::NetCmd;
    use crate::network::protocol::NeighbourMsg;
    use crate::shell::network::shell_network_for_test;
    use crate::shell::state::{PeerOrigin, RoomId, WorkspaceTree};

    /// The person who administers the room in these tests.
    const OWNER: &str = "ada";

    /// A real base58 peer id, minted from a keypair, so the parse the
    /// room document does is the production one.
    fn peer_id(seed: u8) -> String {
        libp2p::identity::Keypair::ed25519_from_bytes([seed; 32])
            .expect("32 bytes is a keypair")
            .public()
            .to_peer_id()
            .to_base58()
    }

    /// The handle presence frames identify `peer` by.
    fn handle_of(peer: &str) -> u64 {
        crate::network::behaviour::peer_liveness_handle(
            &peer.parse::<libp2p::PeerId>().expect("a peer id"),
        )
    }

    /// The rooms a session is in, read off its tree exactly the way the
    /// frame loop reads them before calling [`PeerPresence::set_rooms`].
    /// Derived rather than passed so a divergence between the tree and
    /// the presence relay is something this test can catch.
    fn rooms_of(tree: &crate::shell::state::WorkspaceTreeHandle) -> Vec<String> {
        tree.read()
            .workspaces
            .iter()
            .map(|w| w.room.to_string())
            .collect()
    }

    /// One session's collaboration broker, on a socket of its own.
    fn broker(dir: &std::path::Path, name: &str) -> crate::shell::collab_broker::BrokerHandle {
        crate::shell::collab_broker::spawn(dir.join(name)).expect("a broker on a temp socket")
    }

    /// A session standing in for the host: it governs a room, owns the
    /// workspace in it, and has named everybody in it.
    ///
    /// Naming is the host's act and only the host's — the machine that
    /// governs a room is the only one whose names count, which is why
    /// the guest below is handed them rather than writing its own.
    fn host_naming(people: &[(&str, &str)]) -> (WorkspaceTree, RoomId) {
        let mut tree = WorkspaceTree::default();
        let ws = tree.append_empty_workspace("Design review");
        let room = tree.room_of(ws).expect("a fresh workspace mints a room");
        tree.adopt_ownerless_workspaces(&OWNER.to_string());
        for (peer, called) in people {
            assert!(
                tree.name_peer(room, &OWNER.to_string(), peer, called),
                "the room's administrator may name a peer in it",
            );
        }
        (tree, room)
    }

    /// A session that has joined `room`, holding what the host's
    /// governance says about it.
    ///
    /// Joining surrenders governance — a guest that could name peers in
    /// somebody else's room could label a caret in a document that
    /// room's capabilities gate — so the names reach it the way they do
    /// in production: replicated out of the room's document, from the
    /// machine whose governance wrote them.
    fn guest_joining(host: &mut WorkspaceTree, room: RoomId) -> WorkspaceTree {
        let mut tree = WorkspaceTree::default();
        let ws = tree.append_empty_workspace("Mine");
        assert!(
            tree.set_room(ws, room),
            "the guest has to end up in the room it joined",
        );
        let changes = host
            .export_room_changes_since(room, &[])
            .expect("the host is in the room it governs");
        tree.apply_room_changes(room, &changes, &PeerOrigin::hub(1))
            .expect("the host's governance merges into the guest");
        tree
    }

    /// Hand `msg` to `to` the way the gossip reader does.
    fn over_the_wire(msg: NeighbourMsg, to: &ShellNetwork) {
        match msg {
            NeighbourMsg::PresenceMoved {
                peer,
                user,
                key,
                point,
                ts,
            } => to.handle_presence_message(peer, user, key, point, ts),
            other => panic!("expected PresenceMoved, got {other:?}"),
        }
    }

    /// Take the one presence frame `cmds` is holding.
    fn published(cmds: &mut tokio::sync::mpsc::Receiver<NetCmd>) -> NeighbourMsg {
        match cmds.try_recv().expect("the session must put its pointer on the wire") {
            NetCmd::Publish { msg, .. } => msg,
            other => panic!("expected a publish, got {other:?}"),
        }
    }

    /// The beat the demo films: two people in one room, nothing open,
    /// and each one's pointer arrives on the other's screen under the
    /// name the room gave it.
    ///
    /// Asserted in both directions on purpose. A host and a guest are
    /// not symmetric — the guest joined the host's room, so the host's
    /// pointer is addressed to a room the guest is certainly in, while
    /// the reverse holds only if joining also moved the guest into that
    /// room as the workspace it is looking at. One direction working is
    /// not evidence about the other, and testing only the easy one is
    /// how a half-broken presence path stays green.
    #[test]
    fn a_pointer_crosses_between_two_sessions_both_ways() {
        let guest_peer = peer_id(9);
        let host_peer = peer_id(11);
        // A named invite names both ends, which is what lets each
        // screen attribute the other's cursor to a person.
        let (mut host_tree, room) =
            host_naming(&[(&guest_peer, "bob"), (&host_peer, "ada")]);

        // The guest, in the host's room — which is what accepting an
        // invite leaves behind, and what the reverse direction needs.
        let guest_tree = guest_joining(&mut host_tree, room);

        let host_handle = host_tree.shared();
        let guest_handle = guest_tree.shared();

        let (host_net, mut host_cmds) =
            shell_network_for_test(host_handle.clone(), handle_of(&host_peer));
        let (guest_net, mut guest_cmds) =
            shell_network_for_test(guest_handle.clone(), handle_of(&guest_peer));
        let host_net = Arc::new(host_net);
        let guest_net = Arc::new(guest_net);

        let dir = std::env::temp_dir().join(format!("qos-two-sessions-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("a temp dir");
        let host_broker = broker(&dir, "h.sock");
        let guest_broker = broker(&dir, "g.sock");

        // Attached knowing no rooms, then told — which is the order the
        // compositor does it in, and the order that matters: a relay
        // whose room set never caught up folds nothing at all.
        let host = PeerPresence::attach(&host_broker, Arc::clone(&host_net), Vec::new());
        let guest = PeerPresence::attach(&guest_broker, Arc::clone(&guest_net), Vec::new());
        host.set_rooms(rooms_of(&host_handle));
        guest.set_rooms(rooms_of(&guest_handle));

        let room_key = PresenceKey::Workspace {
            id: room.to_string(),
        };

        // -- the guest points, and the host has to paint him ---------
        guest.set_cursor(420.0, 300.0);
        assert!(
            guest.publish_local_cursor(&room.to_string()),
            "a session whose pointer has been somewhere must announce it",
        );
        over_the_wire(published(&mut guest_cmds), &host_net);

        let mut on_host = PresenceCtx::default();
        assert!(
            host.pump(&mut on_host),
            "the host's next frame must fold in what the guest published",
        );
        let seen = on_host
            .pointers
            .get(&room_key)
            .expect("the host must hold the guest's pointer, filed under the room");
        assert_eq!(seen.len(), 1, "one person is one pointer");
        assert_eq!(
            seen[0].1,
            (420.0, 300.0),
            "and it has to be where the guest actually put it",
        );
        assert_eq!(
            on_host.display_name(&seen[0].0),
            "bob",
            "the pill over it reads the name the room gave the peer",
        );

        // -- and the host points, and the guest has to paint him -----
        host.set_cursor(880.0, 520.0);
        assert!(host.publish_local_cursor(&room.to_string()));
        over_the_wire(published(&mut host_cmds), &guest_net);

        let mut on_guest = PresenceCtx::default();
        assert!(
            guest.pump(&mut on_guest),
            "and the crossing has to work the other way, which is a \
             separate fact about a separate room set",
        );
        let seen = on_guest
            .pointers
            .get(&room_key)
            .expect("the guest must hold the host's pointer");
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].1, (880.0, 520.0));
        assert_eq!(on_guest.display_name(&seen[0].0), "ada");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A session that has not moved its pointer says nothing, and a
    /// session that then moves it says something — over the real wire.
    ///
    /// The silent case is the origin-ghost fix, and on its own it is
    /// indistinguishable from a publisher that has stopped working
    /// altogether. Both halves are asserted here for that reason: the
    /// first publish is what a peer already in the room sees, and if it
    /// never happens two people watch an empty desktop.
    #[test]
    fn a_session_is_silent_until_its_pointer_has_been_somewhere() {
        let guest_peer = peer_id(21);
        let (mut host_tree, room) = host_naming(&[(&guest_peer, "bob")]);
        let guest_tree = guest_joining(&mut host_tree, room);

        let host_handle = host_tree.shared();
        let guest_handle = guest_tree.shared();
        let (host_net, _host_cmds) = shell_network_for_test(host_handle.clone(), 0xD00Du64);
        let (guest_net, mut guest_cmds) =
            shell_network_for_test(guest_handle.clone(), handle_of(&guest_peer));
        let host_net = Arc::new(host_net);
        let guest_net = Arc::new(guest_net);

        let dir = std::env::temp_dir().join(format!("qos-silent-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("a temp dir");
        let host_broker = broker(&dir, "h.sock");
        let guest_broker = broker(&dir, "g.sock");
        let host = PeerPresence::attach(&host_broker, Arc::clone(&host_net), Vec::new());
        let guest = PeerPresence::attach(&guest_broker, Arc::clone(&guest_net), Vec::new());
        host.set_rooms(rooms_of(&host_handle));
        guest.set_rooms(rooms_of(&guest_handle));

        // Nothing has moved. The frame loop still calls this on every
        // pass, and it must put nothing on the wire.
        assert!(
            !guest.publish_local_cursor(&room.to_string()),
            "a pointer that has never been anywhere is not a pointer at the origin",
        );
        assert!(
            guest_cmds.try_recv().is_err(),
            "and nothing may reach the wire for it",
        );
        let mut on_host = PresenceCtx::default();
        assert!(
            !host.pump(&mut on_host),
            "so the host has nobody to paint",
        );
        assert!(on_host.pointers.is_empty());

        // Now it moves, and the very next pass has to speak — a peer
        // already in the room is waiting to see this session arrive.
        guest.set_cursor(640.0, 610.0);
        assert!(guest.publish_local_cursor(&room.to_string()));
        over_the_wire(published(&mut guest_cmds), &host_net);
        assert!(host.pump(&mut on_host));
        assert_eq!(
            on_host
                .pointers
                .get(&PresenceKey::Workspace {
                    id: room.to_string()
                })
                .map(|p| p[0].1),
            Some((640.0, 610.0)),
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A pointer addressed to a room this session is not in is refused,
    /// asserted across two sessions rather than against a closure.
    ///
    /// This is the access rule as much as the tidiness one: presence
    /// arrives from whoever is on the wire, so a host that folded any
    /// room id it was handed would let a remote machine write entries
    /// into someone else's presence store under rooms it has never
    /// heard of.
    #[test]
    fn a_pointer_from_a_room_this_session_is_not_in_is_refused() {
        let stranger_peer = peer_id(33);
        let (host_tree, _room) = host_naming(&[(&stranger_peer, "Nobody")]);

        // A session in a room of its own, which the host has never
        // joined and never heard of.
        let mut stranger_tree = WorkspaceTree::default();
        let stranger_ws = stranger_tree.append_empty_workspace("Elsewhere");
        let elsewhere = stranger_tree
            .room_of(stranger_ws)
            .expect("a fresh workspace mints a room");

        let host_handle = host_tree.shared();
        let stranger_handle = stranger_tree.shared();
        let (host_net, _host_cmds) = shell_network_for_test(host_handle.clone(), 0xBEEFu64);
        let (stranger_net, mut stranger_cmds) =
            shell_network_for_test(stranger_handle.clone(), handle_of(&stranger_peer));
        let host_net = Arc::new(host_net);
        let stranger_net = Arc::new(stranger_net);

        let dir = std::env::temp_dir().join(format!("qos-stranger-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("a temp dir");
        let host_broker = broker(&dir, "h.sock");
        let stranger_broker = broker(&dir, "s.sock");
        let host = PeerPresence::attach(&host_broker, Arc::clone(&host_net), Vec::new());
        let stranger =
            PeerPresence::attach(&stranger_broker, Arc::clone(&stranger_net), Vec::new());
        host.set_rooms(rooms_of(&host_handle));
        stranger.set_rooms(rooms_of(&stranger_handle));

        stranger.set_cursor(10.0, 10.0);
        assert!(stranger.publish_local_cursor(&elsewhere.to_string()));
        over_the_wire(published(&mut stranger_cmds), &host_net);

        let mut on_host = PresenceCtx::default();
        assert!(
            !host.pump(&mut on_host),
            "a room this session is not in is not this session's to hold",
        );
        assert!(
            on_host.pointers.is_empty(),
            "and nothing may be filed under it",
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
