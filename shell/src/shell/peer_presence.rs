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
    ///
    /// The `Option` alone was not enough, and it is worth saying why.
    /// This type refused to publish a pointer it did not have from the
    /// day it was written, and the pill stayed on screen anyway,
    /// because the frame loop filled it in on every pass from a
    /// `(f64, f64)` that answers the origin for a pointer that has
    /// never moved. A gate is only as good as the value handed to it,
    /// so the value now arrives as
    /// [`crate::compositor::client_input::ClientInput::pointer_position`]
    /// — which can say "nowhere" — through
    /// [`Self::set_cursor_position`]. Anything that goes back to
    /// passing a bare pair here reopens the bug without touching this
    /// file.
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

/// How long a session goes without any input at all before it stops
/// telling the room where its pointer is.
///
/// The keepalive above restates a stationary pointer so a late joiner
/// is not shown an empty desktop. On its own it restates it forever,
/// which turns *where the pointer was last put* into a permanent claim
/// that somebody is pointing there — and a session acquires a pointer
/// position the first time anything presses a button on it, which is
/// how a session joins a room at all. One press on a **Join** button
/// and a cursor with a name pill sat on every peer's desktop for the
/// rest of the session, at a control that had long since been
/// dismissed.
///
/// Measured from the last input of *any* kind, not the last motion:
/// somebody typing has not touched their mouse and is plainly still
/// there, and a desktop cursor's real claim is that somebody is at the
/// machine. Any input revives it on the next frame, so the worst case
/// is a cursor briefly missing for somebody genuinely idle rather than
/// a stale one nobody can clear.
///
/// Long enough that nobody using their machine ever reaches it — every
/// keystroke, wheel notch and twitch resets it, so it is only crossed
/// by a session that has had *nothing* for fifteen seconds.
///
/// Which is also why, on its own, it never fired for the case it was
/// written for: a whole beat of a one-minute film is nowhere near
/// fifteen seconds, so a pointer left on a **Join** button was still
/// inside this horizon when the next beat photographed it. The horizon
/// was not too long for a person; it was being asked a question it
/// cannot answer. See [`POINTER_ACTUATED_IDLE`] for the one that can.
pub const POINTER_IDLE: Duration = Duration::from_secs(15);

/// The same question, asked of a pointer whose last word was a button
/// press rather than a movement.
///
/// Fifteen seconds is the right answer for a pointer somebody *moved*
/// and left: there is a cursor sitting at that spot on their screen and
/// it stays there while they read. It is the wrong answer for a
/// position a press left behind, and the difference is not a margin —
/// it is a different fact. A press is an actuation, and an actuation
/// routinely destroys the control it acted on, so the position is
/// pointing at something that is no longer drawn.
///
/// That is the exact shape of the bug this exists for. Joining a room
/// meant pressing **Join** on an invite card; the card was dismissed on
/// the press; the position stayed; and because *any* input keeps the
/// idle clock warm, a name pill sat on every peer's desktop at a
/// control nobody could see, for the rest of the session.
///
/// Deliberately not a shorter [`POINTER_IDLE`]. A single horizon has to
/// be simultaneously long enough for somebody who moved their mouse and
/// is reading, and short enough to retire a press from the previous
/// beat — and in the run this was measured on those two are three
/// seconds apart, which is not a number, it is a coin toss on a loaded
/// machine. Splitting them by what the pointer last did puts several
/// seconds of margin on both sides instead.
///
/// Fail-open is preserved where it matters: a hand resting on a mouse
/// emits motion at the least contact, so a person who clicked and
/// stayed is back on the long horizon before this could elapse. Only a
/// press with nothing behind it reaches it.
///
/// The value is set by what it has to beat rather than by taste. It is
/// only half the delay: the sender falls silent after this, and the
/// receiver then waits [`POINTER_STALE_AFTER`] before taking the cursor
/// off the screen, so a press disappears about
/// `POINTER_ACTUATED_IDLE + POINTER_STALE_AFTER` after it lands — a bit
/// over five seconds. The soonest anything reads the room's cursors
/// after a join is the following beat, which cannot start in less than
/// the read-hold the card before it is given. Under that sum and the
/// pill survives to be photographed, which is the failure this is for.
///
/// It is not a floor on a person's cursor. A pointer somebody moved
/// keeps [`POINTER_IDLE`] regardless, and a press followed by any
/// motion at all is a moved pointer again.
pub const POINTER_ACTUATED_IDLE: Duration = Duration::from_millis(1200);

/// How long a peer's pointer survives in the local store without being
/// restated, before the overlay stops painting it.
///
/// The receiving half of the same fact. A publisher that has gone quiet
/// — idle, crashed, or gone from the mesh without a word — would
/// otherwise leave its last position painted for the rest of the
/// session, because the store cannot tell a frame that arrived just now
/// from the last one that ever will. Cursors drawn *inside* an app have
/// always been retired this way by `cursor_interp`; the desktop store
/// simply had no notion of it.
///
/// Comfortably longer than [`CURSOR_KEEPALIVE`], so a live peer whose
/// restatement was delayed by a slow frame or a dropped gossip message
/// does not blink.
pub const POINTER_STALE_AFTER: Duration = Duration::from_secs(4);

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
    ///
    /// A caller with a literal point has one by definition, so this is
    /// the `Some` half of [`Self::set_cursor_position`]. The frame loop
    /// does not use it: it reads a pointer that may never have been
    /// anywhere, and only that caller can tell.
    pub fn set_cursor(&self, x: f64, y: f64) {
        self.set_cursor_position(Some((x, y)));
    }

    /// Record where this session's pointer is, or that it has never
    /// been anywhere.
    ///
    /// This is what the frame loop calls, and taking an `Option` is the
    /// point of it. Publishing is level-triggered, so whatever is here
    /// is announced to the room on every pass; the compositor's input
    /// state reports the origin for a pointer that has never moved, and
    /// passing that through as a position put a cursor and a name pill
    /// on every peer's screen for somebody who is not there. A session
    /// that never moves a mouse — an agent participating in a document
    /// — wore one permanently, which says the opposite of what such a
    /// participant is: a pointer is an instrument for pointing at
    /// something you are looking at.
    ///
    /// Suppressing the pointer suppresses only the pointer. Arrival,
    /// the roster, and a caret in the document it edits all travel by
    /// other routes and are untouched by this.
    pub fn set_cursor_position(&self, at: Option<(f64, f64)>) {
        if let Ok(mut g) = self.cursor.lock() {
            *g = at;
        }
    }

    /// Fold everything that arrived from other hosts into `presence`,
    /// returning whether anything landed — a peer moved, which is a
    /// reason to repaint. Who is *in* the room is a separate question,
    /// answered level-triggered by `refresh_roster`, because somebody
    /// who joined and never moved a pointer publishes nothing here.
    pub fn pump(&self, presence: &mut PresenceCtx) -> bool {
        self.pump_at(presence, Instant::now())
    }

    /// [`Self::pump`] with the clock supplied, so retirement can be
    /// exercised without sleeping.
    pub fn pump_at(&self, presence: &mut PresenceCtx, now: Instant) -> bool {
        // Retire whatever nobody has restated, before folding in what
        // just arrived — so a peer heard from this frame is refreshed by
        // the fold rather than aged out by its own arrival.
        //
        // Counted as a landing: a cursor leaving the screen changes the
        // screen exactly as much as one moving across it does, and a
        // caller that repainted for the second and not the first would
        // leave the retired one painted until something else happened.
        let retired = presence.prune_pointers(now, POINTER_STALE_AFTER);
        let drained: Vec<_> = {
            // Still a landing if a cursor came off the screen just now:
            // the retirement already happened, and reporting it as
            // nothing would leave the repaint owed.
            let Ok(mut buf) = self.inbound.lock() else {
                return retired;
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
        let landed = fold_inbound(
            drained,
            presence,
            &|room| net.peer_labels(room),
            &|room| mine.iter().any(|r| r == room),
        );
        landed || retired
    }

    /// Write the active room's people into the tree, where the roster
    /// and the top bar read them.
    ///
    /// A peer's arrival used to be visible only as a cursor: the roster
    /// reads `WorkspaceTree::collaborators`, and nothing in a live
    /// session ever wrote it, so the surface that answers "who is in
    /// this room" answered "No peers connected" after somebody had
    /// demonstrably joined. This is that write.
    ///
    /// Deliberately not folded into [`Self::pump`]. That answers "did a
    /// peer move", which a caller uses to decide whether a cursor needs
    /// repainting; this answers "did who-is-here change", and the two
    /// are different questions with different answers — a peer named
    /// while sitting still changes the second and not the first.
    ///
    /// Level-triggered: it restates the room's people whether or not
    /// anything arrived this frame, so somebody who joined and never
    /// touched their mouse is listed without waiting for a pointer that
    /// may never come. It is also what makes a stale list impossible to
    /// persist — the next frame overwrites whatever a state file held.
    ///
    /// Returns whether the list changed, which is a reason to repaint.
    ///
    /// The *active* workspace's room, not every room the session
    /// listens to. The roster is a statement about the room on screen,
    /// and a session is in every room it has ever joined.
    pub fn refresh_roster(&self, presence: &PresenceCtx) -> bool {
        let local = self.net.local_peer_id_hash();
        let Ok(replica) = self.net.replica.lock() else {
            return false;
        };
        // Computed under a read guard and written only when it differs.
        // This runs on every frame; taking the tree's writer that often
        // would contend with the network thread folding peer changes in
        // for a list that changes when somebody joins.
        let rows = {
            let tree = replica.tree.read();
            let Some(ws) = tree.workspaces.iter().find(|w| w.id == tree.active) else {
                return false;
            };
            if ws.room.is_nil() {
                return false;
            }
            let labels = tree.room_peer_labels(ws.room);
            // Both ways somebody can turn up. Presence is a cursor on
            // the desktop; participation is a peer that asked this
            // room's document for its changes or answered with them,
            // which is what a session that never moves a mouse does and
            // is the signal that survives a peer sitting still.
            let mut arrived = handles_seen_in(presence, &ws.room.to_string());
            arrived.extend(replica.participants_in(ws.room));
            let rows = room_roster(ws, &labels, &arrived, local);
            if tree.collaborators == rows {
                return false;
            }
            rows
        };
        replica.tree.write().collaborators = rows;
        true
    }
}

/// The peer handles this session has had presence from in `room`.
///
/// Read out of the same [`PresenceCtx`] the overlay draws its cursors
/// from, so a peer in the roster and a cursor on the desktop are the
/// same evidence rather than two stores that can disagree — which is
/// what the roster listing somebody the desktop does not draw, or the
/// reverse, would mean.
///
/// Taken from the room's presence *registration* rather than from the
/// live pointer list, because the two answer different questions and
/// only one of them expires. A cursor is a claim about right now and is
/// retired when nobody restates it; having turned up in a room is not
/// undone by going quiet. Reading the pointer list here would have made
/// a person's own row flicker between present and merely invited while
/// they sat reading, which is a worse thing to put on screen than the
/// stale cursor that retirement exists to remove.
///
/// Both are still one arrival, filed by one decode:
/// [`PresenceCtx::set_pointer_at`] registers a workspace-scoped pointer
/// as presence at the moment it stores it, so there is no second path
/// to drift from.
fn handles_seen_in(presence: &PresenceCtx, room: &str) -> std::collections::BTreeSet<u64> {
    let mut out = std::collections::BTreeSet::new();
    let key = PresenceKey::Workspace { id: room.to_string() };
    for user in presence.peers_on(&key) {
        if let Some(handle) = user.0.strip_prefix("peer@")
            && let Ok(handle) = handle.parse::<u64>()
        {
            out.insert(handle);
        }
    }
    out
}

/// What a room says about one of the peers in it.
///
/// A name and a kind together, because they are one statement made by
/// one authority: the room records what it calls a member beside what
/// it admitted them as. Split apart, a reader holding the name has no
/// way to ask the second question, and every path that needs an answer
/// ends up guessing from the string — which is how a person can be
/// taken for an agent by being called one.
///
/// A named struct rather than a widened tuple. The value is read on
/// several paths and `label.1` says nothing about which of a member's
/// facts it is, while `label.is_agent` says it at each use; a further
/// fact about a member lands as a field rather than as a third
/// anonymous slot.
///
/// [`Default`] is the "the room has said nothing" answer — no name, and
/// a person — which is already what a peer nobody named is shown as.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PeerLabel {
    /// What the room calls this peer, set by whoever invited them.
    pub name: String,
    /// True when the room's own capability list admitted this member as
    /// an agent rather than as a person.
    ///
    /// Never inferred from the name or from the handle: it is read off
    /// the membership grant, so it stays false for a person however they
    /// are called — the shipped agent's own name included.
    pub is_agent: bool,
}

impl PeerLabel {
    /// A member the room named and admitted as a person.
    pub fn person(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            is_agent: false,
        }
    }

    /// A member the room named and admitted as an agent.
    pub fn agent(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            is_agent: true,
        }
    }
}

/// What a room calls the peers in it, keyed by the handle presence
/// frames identify their sender by, and what it admitted each of them
/// as.
pub type PeerLabels = std::collections::BTreeMap<u64, PeerLabel>;

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
            if let Some(label) = known.get(&entry.peer) {
                presence.set_display_name(
                    ui::permissions::UserId(user.clone()),
                    label.name.clone(),
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
        // Set here rather than on entry to the loop: every `continue`
        // above is a frame that was rejected — a room this session is
        // not in, a key that did not parse — and reporting those as a
        // landing would repaint on traffic that changed nothing.
        any = true;
    }
    any
}

/// The people in `ws`'s room, as the roster lists them.
///
/// Two sources, because a room knows two different things about who is
/// in it and neither one is sufficient on its own.
///
/// `labels` is the room *document*'s answer: every peer its authority
/// has named, replicated as governance. It carries somebody who was
/// invited and has not touched their mouse yet, which presence cannot —
/// a session whose pointer has never been anywhere publishes nothing,
/// deliberately, so a roster built from presence alone leaves a person
/// who joined and sat still out of the room they joined. An agent
/// session never moves a mouse at all.
///
/// `arrived` is the peer handles that have actually turned up in that
/// room — presence this session received, plus the peers that have
/// taken part in the room's document
/// ([`crate::network::replica::WorkspaceTreeReplica::participants_in`]).
/// It carries somebody the room has *not* named — a peer who redeemed a
/// plain invite, which says nothing about who it is for — and the
/// document cannot, because nothing wrote them into it.
///
/// The two together are what let a row say **which** it is. A name is
/// written the moment an invitation is minted, so a roster that treated
/// naming as arriving would claim somebody was here on the strength of
/// an invitation they may never have opened — and, worse, would make
/// joining a room change nothing on the screens of the people already
/// in it. A named peer that has not arrived is marked
/// [`Collaborator::invited`]; when it does arrive the same row becomes
/// an ordinary participant, which is the moment a viewer can see.
///
/// Arrival is deliberately not taken from the peer's own claim. The
/// document half is keyed off the identity the transport authenticated,
/// so a peer cannot promote its own row by asserting it is here.
///
/// `local` is this session's own handle, and it is dropped. The roster
/// answers "who else is here". A host names *itself* when it mints an
/// invite ([`ShellNetwork::issue_invite`]) so that its guests read a
/// name instead of a number; without this the host would read that name
/// back out of its own room document and list itself as a peer.
///
/// Names are never invented and never taken from the peer's own word
/// for itself. A peer nobody has named keeps its id, so a row is never
/// blank and never carries a name nobody chose.
///
/// The role is the room's own statement too, not a second one: it comes
/// from the capability list [`crate::shell::state::Workspace::role_of`]
/// reads, which reaches a guest by the same replication the names do
/// (`sync_room_doc` projects the owner and the grants). A participant the
/// capability list says nothing about has no role, and shows none.
pub fn room_roster(
    ws: &crate::shell::state::Workspace,
    labels: &PeerLabels,
    arrived: &std::collections::BTreeSet<u64>,
    local: u64,
) -> Vec<crate::shell::state::Collaborator> {
    let mut handles: std::collections::BTreeSet<u64> = labels.keys().copied().collect();
    handles.extend(arrived.iter().copied());
    handles.remove(&local);
    handles
        .into_iter()
        .map(|handle| {
            // The identity is the handle, spelled the way the presence
            // path spells it. One person is therefore one id in the
            // roster and on the desktop, which is what makes a row's
            // avatar the same colour as that person's cursor — and why
            // a peer that is named later does not change colour in
            // front of the people watching it.
            let id = format!("peer@{handle}");
            let label = labels.get(&handle).cloned();
            let name = label
                .as_ref()
                .map(|l| l.name.clone())
                .unwrap_or_else(|| id.clone());
            // What the room admitted them as, carried from the room's
            // capability list rather than decided here. A peer the room
            // has said nothing about is a person, which is the same
            // answer the row has always given.
            let is_agent = label.as_ref().is_some_and(|l| l.is_agent);
            // The role comes off the grant that admitted them, and an
            // agent's grant is a different subject from a person's. Read
            // through the person lookup only, an agent with a session of
            // its own showed no role at all — the one member whose whole
            // authority *is* a capability, with nothing on screen saying
            // what the capability was.
            let role = if is_agent {
                ws.role_of_agent(&name.to_lowercase())
            } else {
                ws.role_of(&name)
            };
            let who = crate::shell::state::Collaborator::new(id, name).as_kind(is_agent);
            let who = match role {
                Some(role) => who.with_role(crate::shell::state::role_label(role)),
                None => who,
            };
            // Anybody in `handles` who has not arrived got there by
            // being named, so this is the whole of the invited case.
            if arrived.contains(&handle) {
                who
            } else {
                who.as_invited()
            }
        })
        .collect()
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

    /// The room's names, as its authority wrote them. People — an agent
    /// is admitted by a capability, which these fixtures do not write.
    fn named(pairs: &[(u64, &str)]) -> impl Fn(&str) -> PeerLabels + use<> {
        let map: PeerLabels = pairs
            .iter()
            .map(|(h, n)| (*h, PeerLabel::person(*n)))
            .collect();
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
            &named(&[(7, "Xaver")]),
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
            &named(&[(7, "Xaver")]),
            &any_room,
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
            &named(&[(7, "Xaver")]),
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
            &named(&[(7, "Xaver"), (8, "Quinten")]),
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

    /// A folded frame files the sender under the room it names, spelled
    /// by the handle the transport authenticated.
    ///
    /// Deliberately not the `"bob"` the frame announced. A sending
    /// session's word for who is at it is a claim, not evidence, and a
    /// peer that can announce its own name can announce anybody's —
    /// beside a caret editing a document whose access that same room
    /// governs. What a room calls somebody is a governance fact its
    /// authority writes into the room document; the fold only ever
    /// records which session was heard from.
    #[test]
    fn a_folded_frame_files_the_sender_under_the_room_it_named() {
        let room = "3f2b1c00-0000-4000-8000-000000000001";
        let mut presence = PresenceCtx::default();
        let any = fold_inbound(
            vec![arrived("bob", 7, room, (1.0, 2.0))],
            &mut presence,
            &unnamed,
            &any_room,
        );
        assert!(any, "a frame in a room this session is in lands");
        let peers = presence
            .pointers
            .get(&PresenceKey::Workspace { id: room.into() })
            .expect("a pointer in the room it named");
        assert_eq!(
            peers.iter().map(|(u, _)| u.clone()).collect::<Vec<_>>(),
            vec![UserId("peer@7".into())],
            "the sender is the session the transport authenticated, \
             not the name that session announced for itself",
        );
    }

    /// A pointer scoped to a tab says which tab someone is over. It does
    /// not name a room, so it must not put anybody on a room's roster —
    /// a roster built from unscoped frames would file peers under
    /// whichever room happened to be active.
    #[test]
    fn a_frame_that_names_no_room_yields_no_sighting() {
        let mut presence = PresenceCtx::default();
        let entry = InboundPresence {
            peer: 9,
            user: "carol".into(),
            key: serde_json::to_string(&PresenceKey::Tab { id: "2a".into() })
                .expect("a tab key serialises"),
            point: (3.0, 4.0),
            ts: 0,
        };
        fold_inbound(vec![entry], &mut presence, &unnamed, &any_room);
        // The fold's `bool` answers "repaint?", and a tab-scoped
        // pointer is a real move, so it may well be true. The property
        // that matters is narrower: a frame that names no room must not
        // file anybody *under* one, or a roster built from these would
        // put peers in whichever room happened to be active.
        assert!(
            !presence
                .pointers
                .keys()
                .any(|k| matches!(k, PresenceKey::Workspace { .. })),
            "a frame naming no room put somebody in one: {:?}",
            presence.pointers.keys().collect::<Vec<_>>(),
        );
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
    const OWNER: &str = "quinten";

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
            host_naming(&[(&guest_peer, "Xaver"), (&host_peer, "Quinten")]);

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
            "Xaver",
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
        assert_eq!(on_guest.display_name(&seen[0].0), "Quinten");

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
        let (mut host_tree, room) = host_naming(&[(&guest_peer, "Xaver")]);
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

    /// The frame loop's own call, fed from the input state a session
    /// that has never been pointed at actually holds.
    ///
    /// The test above proves the gate. This one proves the gate is
    /// *reachable*, and that is the whole difference: `PeerPresence`
    /// has always refused to publish a pointer it does not have, and a
    /// name pill sat on peers' screens anyway, because the compositor
    /// handed it one on every frame. `ClientInput::cursor()` answers
    /// the origin for a pointer that has never moved, the frame loop
    /// passed that straight through, and `Some((0.0, 0.0))` is a
    /// position — so the session announced it, level-triggered, for as
    /// long as it was running. A session that never moves a mouse at
    /// all wore that cursor permanently.
    ///
    /// A test that sets the cursor by hand cannot see any of that,
    /// which is why this one takes the position from `ClientInput`
    /// exactly as `native.rs` does rather than writing `None` itself.
    #[test]
    fn the_frame_loop_publishes_nothing_for_a_pointer_that_has_never_moved() {
        let guest_peer = peer_id(23);
        let (mut host_tree, room) = host_naming(&[(&guest_peer, "Claude")]);
        let guest_tree = guest_joining(&mut host_tree, room);

        let host_handle = host_tree.shared();
        let guest_handle = guest_tree.shared();
        let (host_net, _host_cmds) = shell_network_for_test(host_handle.clone(), 0xD0Cu64);
        let (guest_net, mut guest_cmds) =
            shell_network_for_test(guest_handle.clone(), handle_of(&guest_peer));
        let host_net = Arc::new(host_net);
        let guest_net = Arc::new(guest_net);

        let dir = std::env::temp_dir().join(format!("qos-frameloop-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("a temp dir");
        let host_broker = broker(&dir, "h.sock");
        let guest_broker = broker(&dir, "g.sock");
        let host = PeerPresence::attach(&host_broker, Arc::clone(&host_net), Vec::new());
        let guest = PeerPresence::attach(&guest_broker, Arc::clone(&guest_net), Vec::new());
        host.set_rooms(rooms_of(&host_handle));
        guest.set_rooms(rooms_of(&guest_handle));

        // The compositor's input state for a session nothing has
        // pointed at — an agent's, and anyone's for the seconds before
        // they first touch a mouse.
        let input = crate::compositor::client_input::ClientInput::default();
        assert!(
            input.pointer_position().is_none(),
            "the fixture has to be a session that has not been pointed at, \
             or this proves nothing",
        );

        // Exactly what the frame loop evaluates, every pass.
        guest.set_cursor_position(input.pointer_position());
        assert!(
            !guest.publish_local_cursor(&room.to_string()),
            "a session with no pointer has nothing to say about where one is",
        );
        assert!(
            guest_cmds.try_recv().is_err(),
            "and nothing may reach the wire for it",
        );

        let mut on_host = PresenceCtx::default();
        assert!(!host.pump(&mut on_host), "so the host has nobody to paint");
        assert!(
            on_host
                .pointers
                .get(&PresenceKey::Workspace {
                    id: room.to_string()
                })
                .is_none(),
            "and no pill is drawn for a participant who never pointed at anything",
        );

        // The rule is about pointers, not about participation: once
        // this session's pointer has been somewhere, it publishes.
        guest.set_cursor_position(Some((640.0, 610.0)));
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

    /// A session that was pressed once, minutes ago, wears no cursor on
    /// anybody's desktop — and is still in the room.
    ///
    /// This is the bug as it was actually photographed, which the
    /// previous gate was the wrong shape to catch. That gate refused a
    /// pointer that had never been anywhere; the session in the picture
    /// had been somewhere, exactly once, because pressing **Join** is
    /// how a session enters a room and a press puts the pointer on the
    /// button. The position was real, the gate passed it, and the
    /// keepalive restated it to the room every second for the rest of
    /// the run — a name pill parked on a control that had been
    /// dismissed several beats earlier.
    ///
    /// Driven end to end for that reason: the input state a real press
    /// leaves behind, the value the frame loop reads out of it, the
    /// publisher, the wire, and the store the overlay paints from. Every
    /// previous version of this fix passed a test that stopped short of
    /// one of those and left the pill on screen.
    ///
    /// The second half is the part that must not regress. Retiring a
    /// cursor is not evicting a participant: the room still holds them,
    /// the roster still lists them, and a caret they place in a document
    /// still appears. A participant with no screen is precisely a peer
    /// that is present, silent about pointing, and visible where it
    /// acts.
    #[test]
    fn a_session_pressed_once_and_then_left_alone_stops_wearing_a_cursor() {
        let guest_peer = peer_id(24);
        let (mut host_tree, room) = host_naming(&[(&guest_peer, "Claude")]);
        let guest_tree = guest_joining(&mut host_tree, room);

        let host_handle = host_tree.shared();
        let guest_handle = guest_tree.shared();
        let (host_net, _host_cmds) = shell_network_for_test(host_handle.clone(), 0xD0Cu64);
        let (guest_net, mut guest_cmds) =
            shell_network_for_test(guest_handle.clone(), handle_of(&guest_peer));
        let host_net = Arc::new(host_net);
        let guest_net = Arc::new(guest_net);

        let dir = std::env::temp_dir().join(format!("qos-pressed-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("a temp dir");
        let host_broker = broker(&dir, "ph.sock");
        let guest_broker = broker(&dir, "pg.sock");
        let host = PeerPresence::attach(&host_broker, Arc::clone(&host_net), Vec::new());
        let guest = PeerPresence::attach(&guest_broker, Arc::clone(&guest_net), Vec::new());
        host.set_rooms(rooms_of(&host_handle));
        guest.set_rooms(rooms_of(&guest_handle));

        // The press. `Join` sits here at the size this was filmed at,
        // and the harness clicks it by name — the pointer is moved to
        // the control and the button goes down, which is all a person
        // pressing it does either.
        let joined_at = Instant::now();
        let mut input = crate::compositor::client_input::ClientInput::default();
        input.note_input_for_test(
            Some((1156.0, 588.0)),
            joined_at,
            crate::compositor::client_input::PointerEvidence::Actuated,
        );
        assert_eq!(
            input.pointer_position(),
            Some((1156.0, 588.0)),
            "the fixture has to be a session that really was pointed at, \
             or it proves nothing about the run that was photographed",
        );

        // The frame loop, on the frame the press landed.
        guest.set_cursor_position(input.live_pointer(
            joined_at,
            POINTER_IDLE,
            POINTER_ACTUATED_IDLE,
        ));
        assert!(
            guest.publish_local_cursor_at(&room.to_string(), joined_at),
            "somebody is pressing a button on this session, so it is pointing",
        );
        over_the_wire(published(&mut guest_cmds), &host_net);

        let mut on_host = PresenceCtx::default();
        let ws = PresenceKey::Workspace { id: room.to_string() };
        assert!(host.pump_at(&mut on_host, joined_at));
        assert_eq!(
            on_host.pointers.get(&ws).map(|p| p[0].1),
            Some((1156.0, 588.0)),
            "and the host paints it, which is correct at this instant",
        );

        // Nothing touches the session again. The card is dismissed, the
        // room gets on with itself, and the frame loop keeps running.
        //
        // The gap is the film's, and that is the whole reason this test
        // is worth anything. It used to be `POINTER_IDLE + 1s` — derived
        // from the very constant under test, so it passed for any value
        // of that constant, including the value that was live when a
        // shot came back with a pill reading `Claude` in it. A horizon
        // cannot be checked against itself.
        //
        // Five seconds is a floor on what the run leaves between the
        // beat that joins a room and the beat that reads its cursors —
        // that beat sleeps 2.6s on one card and 0.6s after each of two
        // pointer moves before it looks, and the beat before it waits on
        // a grant. The real gap is longer. It is comfortably inside
        // `POINTER_IDLE`, asserted below so a future attempt to fix this
        // by shortening one horizon has to confront what it trades away:
        // the person whose cursor has to survive the same wait is only a
        // second or two further back.
        let gap_the_film_leaves = Duration::from_secs(5);
        assert!(
            gap_the_film_leaves < POINTER_IDLE,
            "the single idle horizon does not elapse inside this film, so \
             it was never what retires this pointer",
        );
        let later = joined_at + gap_the_film_leaves;
        guest.set_cursor_position(input.live_pointer(
            later,
            POINTER_IDLE,
            POINTER_ACTUATED_IDLE,
        ));
        assert!(
            !guest.publish_local_cursor_at(&room.to_string(), later),
            "nobody is at this session, so it has nothing to say about \
             where it is pointing",
        );
        assert!(
            guest_cmds.try_recv().is_err(),
            "and nothing may reach the wire for it",
        );

        // The half that must not go with it. A person who moved their
        // mouse and is now reading their screen has a cursor genuinely
        // sitting at that spot, and the same gap must leave it there --
        // a missing cursor for somebody who is really present is a
        // worse lie than a stale one is a blemish. Kept on its own
        // input state so the relay above is not disturbed.
        let mut moved_and_left = crate::compositor::client_input::ClientInput::default();
        moved_and_left.note_input_for_test(
            Some((640.0, 610.0)),
            joined_at,
            crate::compositor::client_input::PointerEvidence::Resting,
        );
        assert_eq!(
            moved_and_left.live_pointer(later, POINTER_IDLE, POINTER_ACTUATED_IDLE),
            Some((640.0, 610.0)),
            "somebody who moved their pointer and left it keeps their \
             cursor across exactly the gap that retired the press - which \
             is what a single shorter horizon could not have done",
        );

        // The host stops hearing about it, and stops painting it.
        let host_gives_up = later + POINTER_STALE_AFTER + Duration::from_secs(1);
        host.pump_at(&mut on_host, host_gives_up);
        assert!(
            on_host.pointers.get(&ws).map(|p| p.len()).unwrap_or(0) == 0,
            "so no pill is left parked on a control that was dismissed \
             several beats ago, got {:?}",
            on_host.pointers.get(&ws),
        );

        // But the participant did not leave, and nothing about it may
        // suggest it did.
        assert!(
            on_host.peers_on(&ws).iter().any(|u| u.0 == format!("peer@{}", handle_of(&guest_peer))),
            "a peer that has gone quiet about pointing is still in the room",
        );
        let block = PresenceKey::Widget {
            path: vec![ui::presence::PathSegment::Field("wp_block".to_string())],
        };
        let claude = ui::permissions::UserId(format!("peer@{}", handle_of(&guest_peer)));
        on_host.set_caret(&block, claude.clone(), 12);
        host.pump_at(&mut on_host, host_gives_up + POINTER_STALE_AFTER * 4);
        assert_eq!(
            on_host.caret_for(&block, &claude),
            Some(12),
            "and its caret in the document is untouched by any of this - \
             which is where a participant with no screen is supposed to appear",
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

    /// Put the fixture's workspace on screen.
    ///
    /// `WorkspaceTree::default()` starts with no workspaces and an
    /// `active` that names none, and `append_empty_workspace` does not
    /// move it — which is fine for the tests above, whose sessions
    /// address rooms by name. The roster is different: it is a
    /// statement about the room the user is *looking at*, and a real
    /// session always has one. So the fixture has to say which, the way
    /// a live tree always does.
    fn looking_at_its_room(tree: &mut WorkspaceTree) {
        tree.active = tree
            .workspaces
            .first()
            .expect("the fixture makes a workspace")
            .id;
    }

    /// A session's own view of who is in the room it is looking at,
    /// read the way the roster and the top bar read it.
    fn roster_of(
        tree: &crate::shell::state::WorkspaceTreeHandle,
    ) -> Vec<(String, Option<String>, bool)> {
        tree.read()
            .collaborators
            .iter()
            .map(|c| (c.name.clone(), c.role_id.clone(), c.invited))
            .collect()
    }

    /// Stand a session up around `tree` with `peer` as its identity.
    /// Returns the relay and the command channel, which has to be kept
    /// alive or the actor's sends start failing.
    fn session(
        dir: &std::path::Path,
        sock: &str,
        tree: &crate::shell::state::WorkspaceTreeHandle,
        peer: &str,
    ) -> (
        PeerPresence,
        Arc<ShellNetwork>,
        tokio::sync::mpsc::Receiver<NetCmd>,
    ) {
        let (net, cmds) = shell_network_for_test(tree.clone(), handle_of(peer));
        let net = Arc::new(net);
        let relay = PeerPresence::attach(&broker(dir, sock), Arc::clone(&net), Vec::new());
        relay.set_rooms(rooms_of(tree));
        (relay, net, cmds)
    }

    /// The gap this closes: somebody demonstrably joined, and the
    /// surface that answers "who is in this room" said nobody was.
    ///
    /// The host had every fact it needed — the guest's pointer was on
    /// its screen and the guest's name was in its room document — and
    /// still listed nothing, because `WorkspaceTree::collaborators` is
    /// what the roster reads and nothing in a live session wrote it.
    #[test]
    fn a_peer_who_joined_is_in_the_roster() {
        let guest_peer = peer_id(21);
        let host_peer = peer_id(22);
        let (mut host_tree, room) = host_naming(&[(&guest_peer, "Xaver")]);
        let mut guest_tree = guest_joining(&mut host_tree, room);
        looking_at_its_room(&mut host_tree);
        looking_at_its_room(&mut guest_tree);
        let host_handle = host_tree.shared();
        let guest_handle = guest_tree.shared();

        let dir = std::env::temp_dir().join(format!("qos-roster-join-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("a temp dir");
        let (host, host_net, _host_cmds) = session(&dir, "h.sock", &host_handle, &host_peer);
        let (guest, _guest_net, mut guest_cmds) =
            session(&dir, "g.sock", &guest_handle, &guest_peer);

        assert!(
            roster_of(&host_handle).is_empty(),
            "before anyone arrives the room has nobody in it",
        );

        // The guest arrives the only way a guest can: presence, on the
        // room they joined.
        guest.set_cursor(420.0, 300.0);
        assert!(guest.publish_local_cursor(&room.to_string()));
        over_the_wire(published(&mut guest_cmds), &host_net);

        let mut on_host = PresenceCtx::default();
        assert!(host.pump(&mut on_host), "the frame folds the guest in");
        assert!(
            host.refresh_roster(&on_host),
            "and the room's people changed, which is a reason to repaint",
        );

        assert_eq!(
            roster_of(&host_handle),
            vec![("Xaver".to_string(), None, false)],
            "the host's roster must name the person who joined it, as \
             somebody who is here rather than somebody who was asked",
        );

        // Level-triggered, so a second pass with nothing new reports no
        // change — a frame loop that said "changed" every tick would
        // never let the desktop idle.
        assert!(
            !host.refresh_roster(&on_host),
            "restating the same room's people is not a change",
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A plain invite says nothing about who its token is for, so the
    /// peer who redeems one has no name in the room. They are listed as
    /// their id — never blank, and never a name nobody chose.
    #[test]
    fn a_peer_the_room_has_not_named_is_listed_as_its_id() {
        let guest_peer = peer_id(31);
        let host_peer = peer_id(32);
        let (mut host_tree, room) = host_naming(&[]);
        let mut guest_tree = guest_joining(&mut host_tree, room);
        looking_at_its_room(&mut host_tree);
        looking_at_its_room(&mut guest_tree);
        let host_handle = host_tree.shared();
        let guest_handle = guest_tree.shared();

        let dir = std::env::temp_dir().join(format!("qos-roster-unnamed-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("a temp dir");
        let (host, host_net, _host_cmds) = session(&dir, "h.sock", &host_handle, &host_peer);
        let (guest, _guest_net, mut guest_cmds) =
            session(&dir, "g.sock", &guest_handle, &guest_peer);

        guest.set_cursor(10.0, 20.0);
        assert!(guest.publish_local_cursor(&room.to_string()));
        over_the_wire(published(&mut guest_cmds), &host_net);

        let mut on_host = PresenceCtx::default();
        host.pump(&mut on_host);
        host.refresh_roster(&on_host);

        let expected = format!("peer@{}", handle_of(&guest_peer));
        assert_eq!(
            roster_of(&host_handle),
            vec![(expected, None, false)],
            "an unnamed peer keeps its id as its label",
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A host names itself when it mints an invite, so its guests read
    /// a name rather than a number. Reading that back as a peer of its
    /// own would put the person holding the machine in their own list
    /// of other people.
    #[test]
    fn the_room_does_not_list_the_session_reading_it() {
        let guest_peer = peer_id(41);
        let host_peer = peer_id(42);
        // Both ends named, which is what `issue_invite` leaves behind.
        let (mut host_tree, room) = host_naming(&[(&guest_peer, "Xaver"), (&host_peer, OWNER)]);
        let mut guest_tree = guest_joining(&mut host_tree, room);
        looking_at_its_room(&mut host_tree);
        looking_at_its_room(&mut guest_tree);
        let host_handle = host_tree.shared();
        let guest_handle = guest_tree.shared();

        let dir = std::env::temp_dir().join(format!("qos-roster-self-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("a temp dir");
        let (host, host_net, _host_cmds) = session(&dir, "h.sock", &host_handle, &host_peer);
        let (guest, _guest_net, mut guest_cmds) =
            session(&dir, "g.sock", &guest_handle, &guest_peer);

        guest.set_cursor(1.0, 2.0);
        assert!(guest.publish_local_cursor(&room.to_string()));
        over_the_wire(published(&mut guest_cmds), &host_net);
        let mut on_host = PresenceCtx::default();
        host.pump(&mut on_host);
        host.refresh_roster(&on_host);

        let listed: Vec<String> =
            roster_of(&host_handle).into_iter().map(|(n, ..)| n).collect();
        assert_eq!(
            listed,
            vec!["Xaver".to_string()],
            "the roster is who else is here, and the host is not else",
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Somebody the room has named and who has not turned up reads as
    /// invited — listed, because the room's authority really did record
    /// them, but never as a person who is here.
    ///
    /// Nothing has crossed a wire in this test: no pointer, no sync
    /// round. So the only thing this session knows about that peer is
    /// that the room document names it, which is an invitation and not
    /// an arrival. Drawing the two the same way would claim somebody
    /// was present on the strength of a token they may never have
    /// opened.
    #[test]
    fn a_named_peer_who_has_not_arrived_reads_as_invited() {
        let host_peer = peer_id(51);
        let guest_peer = peer_id(52);
        let (mut host_tree, room) = host_naming(&[(&host_peer, OWNER)]);
        let mut guest_tree = guest_joining(&mut host_tree, room);
        looking_at_its_room(&mut guest_tree);
        let guest_handle = guest_tree.shared();

        let dir = std::env::temp_dir().join(format!("qos-roster-guest-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("a temp dir");
        let (guest, _guest_net, _guest_cmds) =
            session(&dir, "g.sock", &guest_handle, &guest_peer);

        // Nothing has arrived and nothing has moved.
        let empty = PresenceCtx::default();
        assert!(
            guest.refresh_roster(&empty),
            "a room whose authority has named somebody has a row to show",
        );
        assert_eq!(
            roster_of(&guest_handle),
            vec![(OWNER.to_string(), Some("owner".to_string()), true)],
            "the row carries the name and the role the room replicated, \
             and says that peer has not arrived",
        );

        // And the surfaces that claim people are *present* do not count
        // them. An avatar in the top bar's cluster is that claim, and
        // the slot it would replace is the offer to invite the first
        // person — so filling it with somebody just invited would have
        // the bar answering its own offer.
        assert_eq!(
            crate::shell::state::active_workspace_peer_count(&guest_handle.read()),
            0,
            "an invitation nobody has redeemed is not a peer in the room",
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The beat: an invitation becomes a participant, and the screen
    /// changes at the moment it does.
    ///
    /// Deliberately without a pointer. Joining is not moving a mouse — a
    /// session whose pointer has never been anywhere publishes nothing,
    /// so a roster that waited for one would leave the person who just
    /// joined looking exactly like the person who was merely asked.
    ///
    /// What the host observes instead is the guest asking this room's
    /// document for its changes, which is what taking part in a room
    /// *is*, and which arrives under the identity the transport
    /// authenticated rather than one the guest asserted about itself.
    #[test]
    fn arriving_turns_an_invitation_into_a_participant() {
        let guest_peer = peer_id(61);
        let host_peer = peer_id(62);
        let (mut host_tree, room) = host_naming(&[(&guest_peer, "Xaver")]);
        looking_at_its_room(&mut host_tree);
        let host_handle = host_tree.shared();

        let dir = std::env::temp_dir().join(format!("qos-roster-beat-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("a temp dir");
        let (host, host_net, _host_cmds) = session(&dir, "h.sock", &host_handle, &host_peer);

        let quiet = PresenceCtx::default();
        assert!(host.refresh_roster(&quiet));
        assert_eq!(
            roster_of(&host_handle),
            vec![("Xaver".to_string(), None, true)],
            "before he redeems anything, quinten has invited him and no more",
        );
        assert_eq!(
            crate::shell::state::active_workspace_peer_count(&host_handle.read()),
            0,
            "so the top bar still offers to invite the first person",
        );

        // He redeems it. What reaches the host is a request for this
        // room's changes under his authenticated peer id — the same
        // thing `accept_invite` puts on the wire.
        {
            let mut replica = host_net.replica.lock().expect("the replica lock");
            replica.handle_from(
                &crate::shell::state::PeerOrigin::peer_base58(&guest_peer),
                &NeighbourMsg::SyncReq {
                    since: serde_json::to_vec(&Vec::<automerge::ChangeHash>::new())
                        .expect("empty heads serialise"),
                    room: room.to_string(),
                    invite: None,
                },
            );
        }

        assert!(
            host.refresh_roster(&quiet),
            "the room's people changed, which is what makes the frame repaint",
        );
        assert_eq!(
            roster_of(&host_handle),
            vec![("Xaver".to_string(), None, false)],
            "the same row, now a participant — this is the change a \
             viewer sees at the moment somebody joins",
        );
        assert_eq!(
            crate::shell::state::active_workspace_peer_count(&host_handle.read()),
            1,
            "and the bar shows him instead of offering to invite him",
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The frame itself, because a fix here has already passed its test
    /// while a screenshot still had the pill in it.
    ///
    /// Renders the desktop through `FrameBuilder::paint` — the same call
    /// the live compositor and `shots --mode offscreen` make — twice:
    /// once on the frame a press lands, and once after the room has got
    /// on with itself. The first must draw more than the second, and the
    /// person must survive both.
    ///
    /// Real elapsed time rather than an injected clock, because the half
    /// that was never exercised is the receiving side: `pump` reads
    /// `Instant::now()`, and if the retirement it performs were not
    /// wired into the paint path, every clock-controlled assertion above
    /// would still pass with the pill on screen.
    #[test]
    fn the_frame_a_peer_is_painted_on_loses_the_pill_and_keeps_the_person() {
        use crate::compositor::client_input::{ClientInput, PointerEvidence};

        let guest_peer = peer_id(41);

        // The host's own frame, built the way the compositor builds it,
        // and governed through its own tree rather than one swapped in
        // underneath it — the chrome hands its handle to the network,
        // and a tree assigned wholesale afterwards is a different room
        // from the one the relay is subscribed to.
        let mut fb = crate::compositor::frame::FrameBuilder::new((1920, 1014));
        let (room, host_handle, guest_tree) = {
            let chrome = fb.chrome();
            let mut tree = chrome.tree_mut();
            let active = tree.active;
            tree.adopt_ownerless_workspaces(&OWNER.to_string());
            let room = tree.room_of(active).expect("the active workspace has a room");
            assert!(
                tree.name_peer(room, &OWNER.to_string(), &guest_peer, "Claude"),
                "the room's administrator may name a peer in it",
            );
            let guest_tree = guest_joining(&mut tree, room);
            (room, chrome.tree_handle(), guest_tree)
        };
        let guest_handle = guest_tree.shared();

        let (host_net, _host_cmds) = shell_network_for_test(host_handle.clone(), 0xF8Au64);
        let (guest_net, mut guest_cmds) =
            shell_network_for_test(guest_handle.clone(), handle_of(&guest_peer));
        let host_net = Arc::new(host_net);
        let guest_net = Arc::new(guest_net);

        let dir = std::env::temp_dir().join(format!("qos-frame-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("a temp dir");
        let host_broker = broker(&dir, "fh.sock");
        let guest_broker = broker(&dir, "fg.sock");
        let guest = PeerPresence::attach(&guest_broker, Arc::clone(&guest_net), Vec::new());
        guest.set_rooms(rooms_of(&guest_handle));

        fb.set_peer_presence(PeerPresence::attach(
            &host_broker,
            Arc::clone(&host_net),
            vec![room.to_string()],
        ));

        // The press: the pointer travels to Join and the button goes
        // down. Nothing touches this session again.
        let joined_at = Instant::now();
        let mut input = ClientInput::default();
        input.note_input_for_test(Some((1156.0, 588.0)), joined_at, PointerEvidence::Actuated);
        guest.set_cursor_position(input.live_pointer(
            joined_at,
            POINTER_IDLE,
            POINTER_ACTUATED_IDLE,
        ));
        assert!(
            guest.publish_local_cursor_at(&room.to_string(), joined_at),
            "somebody is pressing a button on this session",
        );
        over_the_wire(published(&mut guest_cmds), &host_net);
        fb.pump_peer_presence();

        let ws = PresenceKey::Workspace { id: room.to_string() };
        assert_eq!(
            fb.presence().pointers.get(&ws).map(|p| p.len()).unwrap_or(0),
            1,
            "the frame has a cursor to draw",
        );
        let painted_with = fb.paint();
        let with_pill = painted_with.encoding().draw_tags.len();

        // The room gets on with itself. The frame loop keeps running and
        // keeps asking; the session keeps having nothing new to say.
        //
        // The keepalive is *expected* to restate the press for a moment
        // — that is what stops a peer who joins in the same second from
        // seeing an empty desktop — so what is counted is restatements
        // after the horizon, not restatements at all.
        let stops_speaking_at = joined_at + POINTER_ACTUATED_IDLE;
        let mut restated_past_the_horizon = 0usize;
        let deadline = stops_speaking_at + POINTER_STALE_AFTER + Duration::from_millis(1500);
        while Instant::now() < deadline {
            let now = Instant::now();
            guest.set_cursor_position(input.live_pointer(
                now,
                POINTER_IDLE,
                POINTER_ACTUATED_IDLE,
            ));
            if guest.publish_local_cursor_at(&room.to_string(), now) {
                if now >= stops_speaking_at {
                    restated_past_the_horizon += 1;
                }
                over_the_wire(published(&mut guest_cmds), &host_net);
            }
            fb.pump_peer_presence();
            std::thread::sleep(Duration::from_millis(120));
        }
        assert_eq!(
            restated_past_the_horizon, 0,
            "the keepalive went on restating a position nobody has \
             touched, which is the whole defect",
        );

        assert_eq!(
            fb.presence().pointers.get(&ws).map(|p| p.len()).unwrap_or(0),
            0,
            "and the frame the compositor is about to paint has no cursor \
             left in it",
        );
        let painted_without = fb.paint();
        let without_pill = painted_without.encoding().draw_tags.len();
        assert!(
            without_pill < with_pill,
            "the second frame has to be visibly smaller than the first, \
             or nothing was drawn for the peer in either and this proves \
             nothing: {with_pill} -> {without_pill}",
        );

        // The person did not leave, and the frame still says so.
        assert!(
            fb.presence()
                .peers_on(&ws)
                .iter()
                .any(|u| u.0 == format!("peer@{}", handle_of(&guest_peer))),
            "retiring a cursor is not evicting a participant",
        );

        // And the two frames themselves, when asked for. The counts
        // above are the assertion; these are the pictures it is about,
        // put through the same offscreen renderer `shots` uses so they
        // are what the compositor would have shown.
        if let Ok(out) = std::env::var("QOS_PRESENCE_FRAMES") {
            let dest = std::path::Path::new(&out);
            let _ = std::fs::create_dir_all(dest);
            let mut r = ui::offscreen::OffscreenRenderer::new(1920, 1014);
            let bg = vello::peniko::Color::BLACK;
            r.render_png(&painted_with, bg, dest.join("pressed.png"));
            r.render_png(&painted_without, bg, dest.join("left-alone.png"));
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

}
