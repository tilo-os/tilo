//! Peer pointers, from an app in a room to the compositor drawing it.
//!
//! The one pointer pipeline for both ways the shell runs: the native
//! compositor attaches it to its frame loop, and the hosted shell (the shell
//! as a window) to its frame tick. Both publish the same tile-anchored key,
//! accept the same keys, and hand the overlay the same store, timing and
//! glide ([`ui::presence::PresenceCtx::pointer_position_at`]).
//!
//! Outbound: a [`RoomSubscription`] republishes each local app's
//! surface-local pointer as this session's cursor — per session, not per
//! connection, so one person stays one cursor across windows.
//! [`should_publish_cursor`] throttles to [`MIN_PUBLISH_GAP`]. A still
//! pointer is not restated on a timer, which would repaint every peer for
//! nothing: it is said once more only when a peer joins, so a late joiner
//! learns where it rests ([`ShellNetwork::peers_joined_epoch`]).
//!
//! A cursor never travels in desktop coordinates: those are this device's
//! monitors, and a collaborator's desktop is another shape. It travels as
//! a [`PointerAnchor`] — a tab, which the room shares, and an offset from
//! the top-left of the tile showing it — and each receiver places it over
//! its own tile for that tab, again every frame, so it stays on the window
//! however the receiver's monitors, lanes and scrolling lay it out.
//!
//! Inbound: [`PeerPresence::pump`] folds arrivals through the same
//! decode an app uses. A peer's identity (`peer@<handle>`) comes from
//! the frame; its name is a governance fact from
//! [`ShellNetwork::peer_labels`] — a frame's own `user` field is never
//! trusted, since a peer naming itself could name anybody.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use ui::num::f32_of_f64;

use ui::collaboration::{CollabEvent, PresencePayload};
use ui::presence::{PresenceCtx, PresenceKey};

use crate::shell::collab_broker::{BrokerHandle, RoomSubscription};
use crate::shell::network::ShellNetwork;

/// A room's last-published cursor and when it went out, keyed by room —
/// what [`should_publish_cursor`] rate-limits against.
type CursorPublishHistory = HashMap<String, (PointerAnchor, Instant)>;
type CertifiedPeopleInbound = Arc<Mutex<Vec<(String, Vec<ui::collaboration::Collaborator>)>>>;
type DepartureKey = (u64, String, std::time::SystemTime);

/// Where a person's pointer is, in terms every device in the room shares:
/// logical pixels from the top-left corner of the tile showing tab `tab`.
/// The offset can fall outside the tile — a pointer on a tab bar or in the
/// gap beside a window is anchored to the nearest tile — so it lands in the
/// same place relative to that window on every desk.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PointerAnchor {
    pub tab: u64,
    pub x: f64,
    pub y: f64,
}

/// The tiles this device shows, each with the tab it shows, in scene
/// coordinates.
pub type TabTiles = [(u64, vello::kurbo::Rect)];

/// Anchor a pointer at scene point `at` to the tile it is over, or else the
/// nearest one. `None` when nothing is shown: an empty desktop has nothing
/// the room shares to place a pointer against.
pub fn anchor_pointer(tiles: &TabTiles, at: (f64, f64)) -> Option<PointerAnchor> {
    let point = vello::kurbo::Point::new(at.0, at.1);
    let gap = |rect: &vello::kurbo::Rect| {
        let dx = (rect.x0 - point.x).max(point.x - rect.x1).max(0.0);
        let dy = (rect.y0 - point.y).max(point.y - rect.y1).max(0.0);
        dx * dx + dy * dy
    };
    tiles
        .iter()
        .min_by(|(_, a), (_, b)| gap(a).total_cmp(&gap(b)))
        .map(|(tab, rect)| PointerAnchor {
            tab: *tab,
            x: point.x - rect.x0,
            y: point.y - rect.y0,
        })
}

/// Where `anchor` falls on this device, or `None` when this device shows
/// no tile for its tab (another workspace, a tab behind another in its
/// group).
pub fn place_pointer(tiles: &TabTiles, anchor: PointerAnchor) -> Option<(f64, f64)> {
    tiles
        .iter()
        .find(|(tab, _)| *tab == anchor.tab)
        .map(|(_, rect)| (rect.x0 + anchor.x, rect.y0 + anchor.y))
}

/// What a window-hosted shell shows and where its own pointer is, in the
/// host's scene coordinates — the two things the native frame loop reads
/// off its host and seat each frame ([`crate::shell::frame::FrameBuilder::note_local_pointer`]).
/// The hosted shell's frame tick is handed only the presence store, not the
/// host, so the host keeps this current instead: its tiles after every
/// layout, its pointer on every move
/// ([`crate::shell::overlay_host::ShellOverlayHost::share_desk`]).
#[derive(Clone, Default)]
pub struct SharedDesk(Arc<Mutex<Desk>>);

/// A [`SharedDesk`] as of one moment.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Desk {
    /// Each tile shown, with the tab it shows.
    pub tiles: Vec<(u64, vello::kurbo::Rect)>,
    /// The pointer, or `None` before it has moved or after it left the
    /// window.
    pub pointer: Option<(f64, f64)>,
}

impl SharedDesk {
    fn with(&self, edit: impl FnOnce(&mut Desk)) {
        edit(&mut self.0.lock_recover());
    }

    /// Replace the tiles shown with `tiles`.
    pub fn set_tiles(&self, tiles: Vec<(u64, vello::kurbo::Rect)>) {
        self.with(|desk| desk.tiles = tiles);
    }

    /// Record where the pointer is, or that it is in no tile's window.
    pub fn set_pointer(&self, at: Option<(f64, f64)>) {
        self.with(|desk| desk.pointer = at);
    }

    /// The desk as it is now, for one frame to read without holding the lock.
    pub fn snapshot(&self) -> Desk {
        self.0.lock_recover().clone()
    }
}

/// Every room this session is in, and the one the user is looking at — what
/// a frame hands [`PeerPresence::set_rooms`] and
/// [`PeerPresence::publish_local_cursor`]. One reading for both shells, so
/// they listen to and speak in the same rooms.
pub fn rooms_and_active(
    tree: &crate::shell::state::WorkspaceTree,
) -> (Vec<String>, Option<String>) {
    let rooms = tree
        .desktop
        .workspaces
        .iter()
        .map(|w| w.room.to_string())
        .collect();
    let active = tree
        .desktop
        .workspaces
        .iter()
        .find(|w| w.id == tree.desktop.active)
        .map(|w| w.room.to_string());
    (rooms, active)
}

/// Bridges the session's collaboration broker and its peers for
/// pointer presence. Held by the compositor for the life of the session.
pub struct PeerPresence {
    /// This session's pointer, anchored to the tile under it. `None` until
    /// it has actually been somewhere — not the origin, since a session
    /// that never moves would otherwise announce it as a real position —
    /// and while nothing is shown to anchor it to.
    cursor: Arc<Mutex<Option<PointerAnchor>>>,
    /// Every peer pointer received, as sent: which room, which tab, what
    /// offset, and when it was last heard. Placed again on every
    /// [`Self::pump`], because where a tab's tile is on this device changes
    /// as it scrolls and lays out.
    anchored: Mutex<AnchoredPointers>,
    /// Inbound presence from other hosts, filled by [`ShellNetwork::tick`].
    inbound: Arc<Mutex<Vec<crate::shell::network::InboundPresence>>>,
    /// Tab location, follow cues, short statuses, and semantic work arrive through the
    /// authenticated typed-presence route rather than the legacy desktop
    /// cursor queue. They are drained into the same render-only
    /// [`PresenceCtx`] projection in [`Self::pump`].
    typed_inbound: Arc<Mutex<Vec<(String, PresenceKey, PresencePayload)>>>,
    /// The peer sessions that said, per room, that they are not here: a
    /// person who shows nothing there ("Nothing") says so on the room's
    /// typed route, and is then off this roster even while their machine
    /// keeps taking part in the room's documents. Anything else they say
    /// in the room (being available again, a pointer) brings them back.
    absent: Mutex<HashMap<String, std::collections::BTreeSet<u64>>>,
    /// Complete owner-backed People contexts. Ordinary app-authored Context
    /// fanout is deliberately not an input: it cannot establish membership.
    room_people: Arc<Mutex<HashMap<String, Vec<ui::collaboration::Collaborator>>>>,
    certified_people_inbound: CertifiedPeopleInbound,
    _certified_people_subscription:
        Option<Arc<tilo_action_broker::collab_broker::CertifiedPeopleSubscription>>,
    projected_people: Mutex<(Option<String>, HashSet<ui::permissions::UserId>)>,
    departures: Arc<crate::shell::network::PeerDepartureHistory>,
    seen_departures: Mutex<HashSet<DepartureKey>>,
    /// The departure history's generation [`Self::pump`] last read it at.
    departures_applied: AtomicU64,
    /// What the session's apps publish in its rooms. `None` for a session
    /// with no collaboration broker (a hosted shell started without one):
    /// it still publishes its own pointer and places its peers'.
    subscription: Option<Arc<RoomSubscription>>,
    /// The rooms this session is in, mirrored from [`Self::set_rooms`]
    /// so a room this session has left stops being remembered below.
    rooms: Mutex<Vec<String>>,
    /// The workspace tree generation [`Self::follow_tree`] last read the
    /// rooms at, and the room on screen then.
    followed: Mutex<Option<(u64, Option<String>)>>,
    /// What this session last put on the wire, per room: position and
    /// when. Read by [`should_publish_cursor`].
    last_published: Mutex<CursorPublishHistory>,
    /// The [`ShellNetwork::peers_joined_epoch`] the pointer was last said
    /// at: a peer that joined since has not heard where a still one rests.
    joins_welcomed: AtomicU64,
    /// The [`ShellNetwork::peer_colours_epoch`] whose colours [`Self::pump`]
    /// last applied; `u64::MAX` until the first pump applies them.
    colours_applied: AtomicU64,
    /// Advanced by [`Self::pump`] whenever it changed the presence the
    /// roster reads (a pointer heard, placed or retired, typed presence, a
    /// departure), so a pump that folded nothing costs the roster nothing.
    presence_epoch: AtomicU64,
    /// The replica's tree, whose write generation tells [`Self::refresh_roster`]
    /// whether the room's naming, governance or the active workspace can
    /// have changed, without taking the replica's lock to ask.
    roster_tree: crate::shell::state::WorkspaceTreeHandle,
    /// [`crate::network::replica::WorkspaceTreeReplica::participants_epoch`].
    participants_epoch: Arc<AtomicU64>,
    /// What the roster was last derived from; `None` before the first refresh.
    roster_seen: Mutex<Option<RosterSeen>>,
    /// The record each window shows, by tab, as learned from the local app
    /// pointing in it ([`Self::window_record`]).
    window_records: WindowRecords,
    /// The window cursor chat last went to: a message keeps going there
    /// while the pointer crosses a gap between windows.
    last_chat_target: Mutex<Option<ChatTarget>>,
    net: Arc<ShellNetwork>,
}

/// The record a window shows: what its app announced when it pointed there.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WindowRecord {
    pub room: String,
    pub app: String,
    pub record: String,
}

type WindowRecords = Arc<Mutex<HashMap<u64, WindowRecord>>>;

/// Where this person's cursor chat is: the window it is over, the record
/// that window shows, and the pointer from the window's top-left corner.
#[derive(Clone, Debug, PartialEq)]
pub struct ChatTarget {
    pub record: WindowRecord,
    pub at: (f64, f64),
}

/// The change signals the roster was last derived at.
#[derive(Clone, Copy, PartialEq, Eq)]
struct RosterInputs {
    /// [`crate::shell::state::WorkspaceTreeHandle::generation`].
    tree: u64,
    /// [`crate::network::replica::WorkspaceTreeReplica::participants_epoch`].
    participants: u64,
    /// [`PeerPresence::presence_epoch`].
    presence: u64,
}

/// What [`PeerPresence::refresh_roster`] last derived the roster from.
struct RosterSeen {
    inputs: RosterInputs,
    /// The active workspace's room then, empty when it had none.
    room: String,
    /// [`roster_presence_print`] of the presence then, in that room.
    presence: u64,
    /// When the roster was last derived, for [`ROSTER_RECHECK`].
    at: Instant,
}

/// The longest the roster goes without being derived again while none of
/// its change signals moved. It also depends on the clock (a device
/// certificate expiring takes its role chip away) and on presence written
/// outside [`PeerPresence::pump`]; a missed signal makes the roster this
/// late, never wrong for good.
pub const ROSTER_RECHECK: Duration = Duration::from_secs(1);

/// Smallest gap between two pointer publishes for the same room — ~30
/// Hz, matching the rate the toolkit throttles an app's pointer to so a
/// desktop cursor and an in-app cursor cost the same on the wire. Both wires
/// then pace a slot to 20 Hz (`tilo_relay_session::pointer_pacing`: the
/// relay session writer, and the network actor for gossip), keeping the
/// freshest of these samples.
pub const MIN_PUBLISH_GAP: Duration = Duration::from_millis(33);

fn typed_presence_authorized(key: &PresenceKey, payload: &PresencePayload, room: &str) -> bool {
    match payload {
        PresencePayload::Following { .. }
        | PresencePayload::Status { .. }
        | PresencePayload::Availability { .. }
        | PresencePayload::Wave { .. } => {
            matches!(key, PresenceKey::Workspace { id } if id == room)
        }
        // The room's people told that this person is not here: someone who
        // chose to show nothing in it.
        PresencePayload::Left if matches!(key, PresenceKey::Workspace { .. }) => {
            !room.is_empty() && matches!(key, PresenceKey::Workspace { id } if id == room)
        }
        PresencePayload::Arrived | PresencePayload::Left => {
            !room.is_empty()
                && matches!(
                    key,
                    PresenceKey::Tab { id } if u64::from_str_radix(id, 16).is_ok()
                )
        }
        PresencePayload::Interaction(update) => {
            !room.is_empty()
                && matches!(
                    key,
                    PresenceKey::Resource { app, id, kind }
                        if update.resource.id == *id
                            && update.resource.kind == *kind
                            && update
                                .action
                                .app
                                .as_ref()
                                .is_none_or(|owner| owner.id == *app)
                )
        }
        _ => false,
    }
}

fn fold_typed_presence(
    updates: Vec<(String, PresenceKey, PresencePayload)>,
    presence: &mut PresenceCtx,
) -> bool {
    let changed = !updates.is_empty();
    for (user, key, payload) in updates {
        if let PresencePayload::Interaction(update) = payload {
            // The broker and the network's ingress deliver an actor they
            // vouch for under its own id; anything else is its sender.
            let id = ui::permissions::UserId(user.clone());
            let kind = match presence.participant_kind(&id) {
                ui::presence::ParticipantKind::Person => tilo_actions::ActorKind::Person,
                ui::presence::ParticipantKind::Agent => tilo_actions::ActorKind::Agent,
                ui::presence::ParticipantKind::Shortcut => tilo_actions::ActorKind::Shortcut,
                ui::presence::ParticipantKind::App => tilo_actions::ActorKind::App,
                ui::presence::ParticipantKind::Bot => tilo_actions::ActorKind::Bot,
            };
            let actor = if update.actor.id == user && update.actor.kind == kind {
                update.actor.clone()
            } else {
                tilo_actions::InteractionActor::new(user, kind)
            };
            let sequence = update.sequence;
            let update = (*update).normalized_by_broker(actor, sequence);
            let _ = presence.observe_interaction(update);
        } else if matches!(payload, PresencePayload::Left) && matches!(key, PresenceKey::Tab { .. })
        {
            // Here Tab/Left is app lifecycle, not the legacy desktop
            // pointer leaving a viewport. Clear only this location.
            presence.remove_presence(&key, &ui::permissions::UserId(user));
        } else {
            presence.apply_remote_presence(user, key, payload);
        }
    }
    changed
}

fn departure_presence_ids(
    session_handle: u64,
    participant_name: Option<&str>,
    active_labels: &std::collections::BTreeMap<u64, String>,
) -> Vec<String> {
    let mut ids = vec![format!("peer@{session_handle}")];
    if let Some(name) = participant_name
        && !active_labels
            .values()
            .any(|active| active.eq_ignore_ascii_case(name))
    {
        ids.push(name.to_string());
    }
    ids
}

/// Should this session put its pointer on the wire again? `last` is
/// what it last published for this room and when; `welcome` is whether a
/// peer joined since, who has not heard where a still pointer rests. Pure
/// so the decision can be asserted on directly.
pub fn should_publish_cursor<P: PartialEq + Copy>(
    last: Option<(P, Instant)>,
    at: P,
    now: Instant,
    min_gap: Duration,
    welcome: bool,
) -> bool {
    let Some((was, when)) = last else {
        return true; // Never said anything about this room — say it.
    };
    if was == at {
        return welcome;
    }
    now.saturating_duration_since(when) >= min_gap
}

/// What to put on the wire for a room right now, if anything. `at` is
/// `None` until the pointer has moved; `None` here means silence, not
/// the origin.
fn cursor_to_publish<P: PartialEq + Copy>(
    at: Option<P>,
    last: Option<(P, Instant)>,
    now: Instant,
    min_gap: Duration,
    welcome: bool,
) -> Option<P> {
    let at = at?;
    should_publish_cursor(last, at, now, min_gap, welcome).then_some(at)
}

impl PeerPresence {
    /// Subscribe to `rooms` on `broker` and republish what the local
    /// apps publish in them to `net`.
    pub fn attach(broker: &BrokerHandle, net: &Arc<ShellNetwork>, rooms: Vec<String>) -> Self {
        let cursor: Arc<Mutex<Option<PointerAnchor>>> = Arc::new(Mutex::new(None));
        let net_for_subscription = Arc::clone(net);

        let sink_cursor = Arc::clone(&cursor);
        let window_records: WindowRecords = Arc::default();
        let sink_records = Arc::clone(&window_records);
        let typed_inbound: Arc<Mutex<Vec<(String, PresenceKey, PresencePayload)>>> =
            Arc::new(Mutex::new(Vec::new()));
        let sink_typed = Arc::clone(&typed_inbound);
        let room_people = Arc::new(Mutex::new(HashMap::new()));
        let certified_people_inbound = Arc::new(Mutex::new(Vec::new()));
        let certified_sink = Arc::clone(&certified_people_inbound);
        let certified_people_subscription =
            broker.observe_certified_people(Arc::new(move |room, people| {
                if let Ok(mut inbound) = certified_sink.lock() {
                    inbound.push((room, people));
                }
            }));
        let subscription = broker.subscribe_rooms(
            rooms.clone(),
            Box::new(move |event, route| {
                if let CollabEvent::RemotePresenceUpdate {
                    user_id,
                    key,
                    payload,
                } = event
                    && matches!(
                        payload,
                        PresencePayload::Following { .. }
                            | PresencePayload::Status { .. }
                            | PresencePayload::Availability { .. }
                            | PresencePayload::Wave { .. }
                            | PresencePayload::Interaction(_)
                            | PresencePayload::Arrived
                            | PresencePayload::Left
                    )
                {
                    // Room-scoped values must name the delivery room. A
                    // resource-scoped interaction is authorized by the room
                    // route itself, then retained only as roster activity.
                    let authorized = typed_presence_authorized(key, payload, &route.cluster_id);
                    if authorized && let Ok(mut queue) = sink_typed.lock() {
                        queue.push((user_id.clone(), key.clone(), payload.clone()));
                    }
                    return;
                }
                // Only a pointer says where someone is; carets/selections
                // are the document's business, not the desktop's.
                let CollabEvent::RemotePresenceUpdate {
                    payload: PresencePayload::Pointer { .. },
                    ..
                } = event
                else {
                    return;
                };
                // Room comes from the delivery route, not the app's own
                // key — only the route knows which room this app is in.
                if route.cluster_id.is_empty() {
                    return;
                }
                // Nothing to say if the pointer has never moved, or is
                // over nothing the room shares.
                let Some(anchor) = *sink_cursor.lock_recover() else {
                    return;
                };
                // Only where its people may see where this person is.
                if !net_for_subscription
                    .presence_visibility(&route.cluster_id)
                    .shows_where()
                {
                    return;
                }
                // The app pointing is the one under the pointer: the record
                // its window shows, which is what anything said over that
                // window is sealed for and placed in.
                if !route.app_id.is_empty()
                    && let Ok(mut records) = sink_records.lock()
                {
                    records.insert(
                        anchor.tab,
                        WindowRecord {
                            room: route.cluster_id.clone(),
                            app: route.app_id.clone(),
                            record: route.record_id.clone(),
                        },
                    );
                }
                let key = PresenceKey::tile_pointer(route.cluster_id.clone(), anchor.tab);
                let Ok(key_str) = serde_json::to_string(&key) else {
                    return;
                };
                net_for_subscription.publish_presence(key_str, (anchor.x, anchor.y));
            }),
        );
        let mut presence = Self::assemble(
            cursor,
            typed_inbound,
            room_people,
            certified_people_inbound,
            Some(certified_people_subscription),
            Some(subscription),
            net,
            rooms,
        );
        presence.window_records = window_records;
        presence
    }

    /// [`Self::attach`] for a session with no collaboration broker: it
    /// publishes its own pointer and places its peers', but has no local
    /// apps' presence to republish or fold.
    pub fn for_network(net: &Arc<ShellNetwork>, rooms: Vec<String>) -> Self {
        Self::assemble(
            Arc::new(Mutex::new(None)),
            Arc::new(Mutex::new(Vec::new())),
            Arc::new(Mutex::new(HashMap::new())),
            Arc::new(Mutex::new(Vec::new())),
            None,
            None,
            net,
            rooms,
        )
    }

    fn assemble(
        cursor: Arc<Mutex<Option<PointerAnchor>>>,
        typed_inbound: Arc<Mutex<Vec<(String, PresenceKey, PresencePayload)>>>,
        room_people: Arc<Mutex<HashMap<String, Vec<ui::collaboration::Collaborator>>>>,
        certified_people_inbound: CertifiedPeopleInbound,
        certified_people_subscription: Option<
            Arc<tilo_action_broker::collab_broker::CertifiedPeopleSubscription>,
        >,
        subscription: Option<Arc<RoomSubscription>>,
        net: &Arc<ShellNetwork>,
        rooms: Vec<String>,
    ) -> Self {
        let departures = net.peer_departure_history();
        // Read before the snapshot: a departure recorded in between is in
        // both, and the next pump reads it again and finds it seen.
        let departures_applied = AtomicU64::new(departures.generation());
        let seen_departures = departures
            .snapshot()
            .into_iter()
            .map(|departure| {
                (
                    departure.session_handle,
                    departure.route.cluster_id,
                    departure.observed_at,
                )
            })
            .collect();
        // Cloned once, so the roster can read their change signals without
        // locking the replica on every frame.
        let (roster_tree, participants_epoch) = {
            let replica = net.replica.lock_recover();
            (replica.tree.clone(), replica.participants_epoch())
        };
        Self {
            cursor,
            anchored: Mutex::new(HashMap::new()),
            inbound: net.inbound_presence_handle(),
            typed_inbound,
            absent: Mutex::new(HashMap::new()),
            room_people,
            certified_people_inbound,
            _certified_people_subscription: certified_people_subscription,
            projected_people: Mutex::new((None, HashSet::new())),
            departures,
            seen_departures: Mutex::new(seen_departures),
            departures_applied,
            subscription,
            rooms: Mutex::new(rooms),
            followed: Mutex::new(None),
            last_published: Mutex::new(HashMap::new()),
            joins_welcomed: AtomicU64::new(net.peers_joined_epoch()),
            colours_applied: AtomicU64::new(u64::MAX),
            presence_epoch: AtomicU64::new(0),
            roster_tree,
            participants_epoch,
            roster_seen: Mutex::new(None),
            window_records: Arc::default(),
            last_chat_target: Mutex::new(None),
            net: Arc::clone(net),
        }
    }

    /// Re-point the subscription at the session's current rooms. Called
    /// whenever the workspace set changes.
    pub fn set_rooms(&self, rooms: Vec<String>) {
        if let Ok(mut g) = self.rooms.lock() {
            if *g == rooms {
                return;
            }
            // Forget publishes to a left room, so rejoining re-announces
            // rather than staying silent behind a stale keepalive.
            if let Ok(mut last) = self.last_published.lock() {
                last.retain(|room, _| rooms.iter().any(|r| r == room));
            }
            g.clone_from(&rooms);
        }
        if let Ok(mut people) = self.room_people.lock() {
            people.retain(|room, _| rooms.contains(room));
        }
        if let Some(subscription) = &self.subscription {
            subscription.set_rooms(rooms);
        }
    }

    /// Replace a room's complete roster from the existing owner-backed
    /// certified People observer. The shell caller must use the same admitted
    /// context as native apps, never an app's ordinary Context fanout.
    pub(crate) fn set_certified_people(
        &self,
        room: &str,
        people: Vec<ui::collaboration::Collaborator>,
    ) {
        let mine = self.rooms.lock_recover();
        if !mine.iter().any(|current| current == room) {
            return;
        }
        if let Ok(mut current) = self.room_people.lock() {
            current.insert(room.to_owned(), people);
        }
        drop(mine);
    }

    /// Follow the session's workspaces: after any write to `tree` since the
    /// last call, re-point the subscription at its rooms; then put the
    /// pointer on the wire in the room on screen. A wakeup with no tree
    /// write reads nothing from the tree. Returns whether anything went out.
    pub fn follow_tree(&self, tree: &crate::shell::state::WorkspaceTreeHandle) -> bool {
        let mut followed = self.followed.lock_recover();
        // Loaded before the tree is read: a write racing the read below is
        // read again on the next call.
        let generation = tree.generation();
        if followed.as_ref().map(|(seen, _)| *seen) != Some(generation) {
            // Every room the session listens to, and the one the user is
            // looking at (where their pointer is). The read guard is a
            // temporary, released before the subscription is re-pointed.
            let (rooms, here) = rooms_and_active(&tree.read());
            self.set_rooms(rooms);
            *followed = Some((generation, here));
        }
        let here = followed.as_ref().and_then(|(_, here)| here.clone());
        drop(followed);
        // A room no longer on screen no longer sees this pointer.
        let withdrawn = self.withdraw_pointer_except(here.as_deref());
        // Say where this session's pointer is whether or not an app is
        // publishing — the app-driven republish alone stays silent over a
        // window whose app draws no cursor.
        here.is_some_and(|here| self.publish_local_cursor(&here)) || withdrawn
    }

    /// Take this session's pointer away from every room it was last shown
    /// in but `keep`: its people stop showing it. Returns whether any went.
    fn withdraw_pointer_except(&self, keep: Option<&str>) -> bool {
        let gone: Vec<(String, PointerAnchor)> = {
            let mut last = self.last_published.lock_recover();
            let gone = last
                .iter()
                .filter(|(room, _)| Some(room.as_str()) != keep)
                .map(|(room, (at, _))| (room.clone(), *at))
                .collect::<Vec<_>>();
            for (room, _) in &gone {
                last.remove(room);
            }
            gone
        };
        for (room, at) in &gone {
            if let Ok(key) = serde_json::to_string(&PresenceKey::tile_pointer(room.clone(), at.tab))
            {
                self.net.publish_presence_cleared(key);
            }
        }
        !gone.is_empty()
    }

    /// Put this session's own pointer on the wire, in the room the user
    /// is looking at. Returns whether anything went out. Only the room
    /// on screen, or every room this session is in would paint a cursor
    /// for people not looking at it.
    pub fn publish_local_cursor(&self, room: &str) -> bool {
        self.publish_local_cursor_at(room, Instant::now())
    }

    /// [`Self::publish_local_cursor`] with the clock supplied, so the
    /// throttle and the keepalive can be exercised without sleeping.
    pub fn publish_local_cursor_at(&self, room: &str, now: Instant) -> bool {
        if room.is_empty() || !self.net.presence_visibility(room).shows_where() {
            return false;
        }
        let at = *self.cursor.lock_recover();
        if at.is_none() {
            // Off the room's windows: shown there no longer.
            return self.withdraw_pointer_except(None);
        }
        let mut last = self.last_published.lock_recover();
        // Read before deciding: a peer joining while this runs is welcomed
        // on the next frame.
        let joined = self.net.peers_joined_epoch();
        let welcome = self.joins_welcomed.load(Ordering::Acquire) != joined;
        let Some(at) =
            cursor_to_publish(at, last.get(room).copied(), now, MIN_PUBLISH_GAP, welcome)
        else {
            return false;
        };
        self.joins_welcomed.store(joined, Ordering::Release);
        let key = PresenceKey::tile_pointer(room, at.tab);
        let Ok(key_str) = serde_json::to_string(&key) else {
            return false;
        };
        self.net.publish_presence(key_str, (at.x, at.y));
        last.insert(room.to_string(), (at, now));
        true
    }

    /// The id the people in a room know this session by: what they wave
    /// at and follow.
    pub fn own_presence_id(&self) -> String {
        format!("peer@{}", self.net.local_peer_id_hash())
    }

    /// The record window `tab` shows, once its app has pointed in it here.
    /// A window whose app never says where its person points (a legacy app,
    /// one that is not collaborative) has none: nobody shares it.
    pub fn window_record(&self, tab: u64) -> Option<WindowRecord> {
        self.window_records.lock_recover().get(&tab).cloned()
    }

    /// Where cursor chat goes from here: the shared window under this
    /// session's pointer, in `room`, with the pointer's place in it.
    pub fn chat_target(&self, room: &str) -> Option<ChatTarget> {
        let anchor = (*self.cursor.lock_recover())?;
        let record = self.window_record(anchor.tab)?;
        let target = (record.room == room).then_some(ChatTarget {
            record,
            at: (anchor.x, anchor.y),
        })?;
        *self.last_chat_target.lock_recover() = Some(target.clone());
        Some(target)
    }

    /// Where what is being said goes now: the shared window under the
    /// pointer, else the one it last went to in `room`.
    pub fn chat_destination(&self, room: &str) -> Option<ChatTarget> {
        self.chat_target(room).or_else(|| {
            self.last_chat_target
                .lock_recover()
                .clone()
                .filter(|target| target.record.room == room)
        })
    }

    /// Say `said` (text, sent) beside this person's pointer over `target`,
    /// or stop with `None`. Returns whether it went out.
    pub fn say(&self, target: &ChatTarget, said: Option<(&str, bool)>) -> bool {
        self.net.publish_cursor_chat(
            (
                target.record.room.as_str(),
                target.record.app.as_str(),
                target.record.record.as_str(),
            ),
            target.at,
            said.map(|(text, sent)| tilo_relay_protocol::live::CursorChat {
                text: text.to_owned(),
                sent,
            }),
        )
    }

    /// React with `emoji` from this person's pointer over `target`.
    pub fn react(&self, target: &ChatTarget, emoji: &str) -> bool {
        self.net.publish_reaction(
            (
                target.record.room.as_str(),
                target.record.app.as_str(),
                target.record.record.as_str(),
            ),
            target.at,
            emoji.to_owned(),
        )
    }

    /// Record where this session's pointer is, anchored to the tile it is
    /// over ([`anchor_pointer`]), or that it is nowhere a peer could be
    /// shown — an `Option` because a never-moved pointer must stay silent
    /// rather than publish the origin as a real position.
    pub fn set_pointer(&self, at: Option<PointerAnchor>) {
        if let Ok(mut g) = self.cursor.lock() {
            *g = at;
        }
    }

    /// One frame of a session whose host and pointer are read through a
    /// [`SharedDesk`] (the hosted shell): anchor its pointer to the tile
    /// under it, follow its rooms, say where it points in the room on
    /// screen, fold in what peers sent, and restate who is here — the same
    /// steps, in the same order, as the native frame loop. Returns whether
    /// anything a paint shows changed or is still moving.
    pub fn frame(
        &self,
        presence: &mut PresenceCtx,
        desk: &Desk,
        tree: &crate::shell::state::WorkspaceTreeHandle,
    ) -> bool {
        self.set_pointer(desk.pointer.and_then(|at| anchor_pointer(&desk.tiles, at)));
        self.follow_tree(tree);
        let moved = self.pump(presence, &desk.tiles);
        let roster_changed = self.refresh_roster(presence);
        moved || roster_changed
    }

    /// Fold everything that arrived from other hosts into `presence`,
    /// returning whether anything landed or moved. Every peer pointer is
    /// placed over this device's own tile for its tab — `tiles` is what this
    /// device shows right now — and hidden while no such tile is shown. Who
    /// is *in* the room is answered separately by `refresh_roster`.
    pub fn pump(&self, presence: &mut PresenceCtx, tiles: &TabTiles) -> bool {
        self.pump_at(presence, tiles, Instant::now())
    }

    /// [`Self::pump`] with the clock supplied, so a peer going quiet can be
    /// exercised without sleeping. A pointer nobody has restated for
    /// [`ui::presence::POINTER_IDLE`] fades out and is retired; while one is
    /// fading this returns `true`, so the fade is painted frame by frame.
    pub fn pump_at(&self, presence: &mut PresenceCtx, tiles: &TabTiles, now: Instant) -> bool {
        let certified = self
            .certified_people_inbound
            .lock()
            .map(|mut inbound| std::mem::take(&mut *inbound))
            .unwrap_or_default();
        for (room, people) in certified {
            self.set_certified_people(&room, people);
        }
        let drained: Vec<_> = {
            let Ok(mut buf) = self.inbound.lock() else {
                return false;
            };
            std::mem::take(&mut *buf)
        };
        // Each person in the colour their own machine says they wear, not
        // one derived here from their session's handle. Applied after one
        // changed rather than restated every frame; the epoch is read before
        // the colours, so a change racing this read is applied next time.
        let colours = self.net.peer_colours_epoch();
        if self.colours_applied.swap(colours, Ordering::AcqRel) != colours {
            for (peer, hue) in self.net.peer_colours() {
                presence.set_presence_hue(ui::permissions::UserId(format!("peer@{peer}")), hue);
            }
        }
        let folded = !drained.is_empty();
        let said = self.net.take_live_chat();
        let net = Arc::clone(&self.net);
        // Rooms whose pointers this person sees: theirs, unless they show
        // nothing there.
        let mine: Vec<String> = self
            .rooms
            .lock_recover()
            .iter()
            .filter(|room| net.presence_visibility(room).sees_pointers())
            .cloned()
            .collect();
        let mut chat_changed = false;
        let mut hidden = Vec::new();
        let cursor_changed = self.anchored.lock().is_ok_and(|mut anchored| {
            anchored.retain(|user, (room, _, _)| {
                let kept = mine.iter().any(|r| r == room);
                if !kept {
                    hidden.push(user.clone());
                }
                kept
            });
            let mut fresh = fold_inbound(
                drained,
                &mut anchored,
                presence,
                &|room| net.peer_labels(room),
                &|room| mine.iter().any(|r| r == room),
                now,
            );
            chat_changed = fold_live_chat(
                said,
                &mut anchored,
                &mut fresh,
                presence,
                &|room| mine.iter().any(|r| r == room),
                now,
            );
            place_pointers(&anchored, &fresh, presence, tiles, now)
        });
        // A room left, or one where this person now shows nothing: its
        // pointers go at once.
        let mut cursor_changed = cursor_changed;
        for user in hidden {
            cursor_changed |= presence.clear_pointer(&user);
            cursor_changed |= presence.chats.forget(&ui::permissions::UserId(user));
        }
        drop(mine);
        let typed = self
            .typed_inbound
            .lock()
            .map(|mut queue| std::mem::take(&mut *queue))
            .unwrap_or_default();
        let folded = folded || !typed.is_empty();
        // Attribution is normalized to the authenticated envelope identity
        // inside the fold; JSON supplied by an app cannot rename its actor.
        let people_changed = self.project_people(presence);
        let absent_changed = self.note_absences(&typed);
        let typed_changed = fold_typed_presence(typed, presence);
        let departure_changed = self.fold_departures(presence);
        if folded || cursor_changed || departure_changed || people_changed || absent_changed {
            self.presence_epoch.fetch_add(1, Ordering::Release);
        }
        cursor_changed
            || typed_changed
            || departure_changed
            || people_changed
            || chat_changed
            || absent_changed
    }

    /// Note who said, in which room, that they are not here (a
    /// room-keyed `Left` on the typed route), and who said anything else
    /// there since, which brings them back. Returns whether it changed.
    fn note_absences(&self, typed: &[(String, PresenceKey, PresencePayload)]) -> bool {
        let mut absent = self.absent.lock_recover();
        let mut changed = false;
        for (user, key, payload) in typed {
            let PresenceKey::Workspace { id: room } = key else {
                continue;
            };
            let Some(handle) = user
                .strip_prefix("peer@")
                .and_then(|handle| handle.parse::<u64>().ok())
            else {
                continue;
            };
            if matches!(payload, PresencePayload::Left) {
                changed |= absent.entry(room.clone()).or_default().insert(handle);
            } else if let Some(gone) = absent.get_mut(room) {
                changed |= gone.remove(&handle);
            }
        }
        absent.retain(|_, gone| !gone.is_empty());
        changed
    }

    fn project_people(&self, presence: &mut PresenceCtx) -> bool {
        let tree = self.roster_tree.read();
        let room = tree
            .desktop
            .active_workspace()
            .map(|workspace| workspace.room.to_string());
        drop(tree);
        let Ok(people) = self.room_people.lock() else {
            return false;
        };
        let rows = room.as_ref().and_then(|room| people.get(room));
        let current: HashSet<_> = rows
            .into_iter()
            .flatten()
            .map(|participant| ui::permissions::UserId(participant.id.clone()))
            .collect();
        let Ok(mut projected) = self.projected_people.lock() else {
            return false;
        };
        let mut changed = projected.0 != room || projected.1 != current;
        for retired in projected.1.difference(&current) {
            presence.unregister_peer(&retired.0);
        }
        for participant in rows.into_iter().flatten() {
            let id = ui::permissions::UserId(participant.id.clone());
            let kind = ui::presence::ParticipantKind::from(participant.is_agent);
            changed |= presence.participant_kind(&id) != kind
                || presence.names.get(&id) != Some(&participant.name);
            presence.set_participant_kind(&id, kind);
            presence.set_display_name(id.clone(), participant.name.clone());
            if let Some(colour) = participant.colour {
                presence.set_presence_hue(id, colour);
            }
        }
        *projected = (room, current);
        changed
    }

    /// Take everyone who left off `presence`, once each. Reads the departure
    /// history only after it recorded somebody since the last call. Returns
    /// whether anybody was taken off.
    fn fold_departures(&self, presence: &mut PresenceCtx) -> bool {
        // Read before the snapshot, so a departure racing it is read again.
        let generation = self.departures.generation();
        if self.departures_applied.swap(generation, Ordering::AcqRel) == generation {
            return false;
        }
        let departures = self.departures.snapshot();
        let active_keys = departures
            .iter()
            .map(|departure| {
                (
                    departure.session_handle,
                    departure.route.cluster_id.clone(),
                    departure.observed_at,
                )
            })
            .collect::<HashSet<_>>();
        self.seen_departures.lock().is_ok_and(|mut seen| {
            seen.retain(|key| active_keys.contains(key));
            let mut changed = false;
            for departure in departures {
                let key = (
                    departure.session_handle,
                    departure.route.cluster_id.clone(),
                    departure.observed_at,
                );
                if !seen.insert(key) {
                    continue;
                }
                let active_labels = self.net.active_session_labels(&departure.route.cluster_id);
                for id in departure_presence_ids(
                    departure.session_handle,
                    departure.participant_name.as_deref(),
                    &active_labels,
                ) {
                    if let Ok(mut people) = self.room_people.lock()
                        && let Some(rows) = people.get_mut(&departure.route.cluster_id)
                    {
                        rows.retain(|row| row.id != id);
                    }
                    presence.unregister_peer(&id);
                }
                changed = true;
            }
            changed
        })
    }

    /// Write the active workspace's room's people into the tree, where
    /// the roster and top bar read them. Separate from [`Self::pump`]:
    /// answers "did who-is-here change", not "did a peer move" — a peer
    /// named while sitting still changes the former, not the latter.
    /// Returns whether the list changed, a reason to repaint.
    ///
    /// Derived again only once one of its inputs moved: the tree was written
    /// (the active workspace, its room's naming and governance), a peer first
    /// took part in a room's document, or [`Self::pump`] changed who is seen
    /// in the active room. A frame where none did locks nothing but this
    /// relay's own bookkeeping, and [`ROSTER_RECHECK`] bounds how stale a
    /// missed signal can leave it.
    pub fn refresh_roster(&self, presence: &PresenceCtx) -> bool {
        self.refresh_roster_at(presence, Instant::now())
    }

    /// [`Self::refresh_roster`] with the clock supplied, so the recheck can
    /// be exercised without sleeping.
    pub fn refresh_roster_at(&self, presence: &PresenceCtx, now: Instant) -> bool {
        // Loaded before anything is read: a change racing the reads below
        // leaves a signal this refresh did not record, and is read next time.
        let inputs = RosterInputs {
            tree: self.roster_tree.generation(),
            participants: self.participants_epoch.load(Ordering::Acquire),
            presence: self.presence_epoch.load(Ordering::Acquire),
        };
        let mut seen = self.roster_seen.lock_recover();
        if let Some(last) = seen.as_mut()
            && now.saturating_duration_since(last.at) < ROSTER_RECHECK
        {
            if last.inputs == inputs {
                return false;
            }
            // Only presence moved. A peer's pointer moving says nothing new
            // about who is here, and is told apart without the replica.
            if last.inputs.tree == inputs.tree
                && last.inputs.participants == inputs.participants
                && roster_presence_print(presence, &self.anchored, &self.absent, &last.room)
                    == last.presence
            {
                last.inputs = inputs;
                return false;
            }
        }
        let Some((changed, room, tree)) = self.derive_roster(presence, inputs.tree) else {
            return false;
        };
        *seen = Some(RosterSeen {
            inputs: RosterInputs { tree, ..inputs },
            presence: roster_presence_print(presence, &self.anchored, &self.absent, &room),
            room,
            at: now,
        });
        changed
    }

    /// Derive the active room's roster and write it into the tree if it
    /// changed. Returns whether it did, the room it was derived for, and the
    /// tree generation the result stands for: `generation` (loaded before
    /// the tree was read), or the one this refresh's own write leaves when
    /// nobody else wrote in between. `None` when the replica cannot be read.
    fn derive_roster(
        &self,
        presence: &PresenceCtx,
        generation: u64,
    ) -> Option<(bool, String, u64)> {
        let local = self.net.local_peer_id_hash();
        // Written only on change — a lock every frame would contend
        // with the network thread. `replica` is locked once to compute
        // `rows` and dropped before re-locking to write it, rather than
        // held across both — the network thread's own use of the lock
        // is short, and holding it through an unrelated write extends
        // every contending wait by however long that write takes.
        let (active, rows, room) = {
            let Ok(replica) = self.net.replica.lock() else {
                return None;
            };
            let tree = replica.tree.read();
            let Some(ws) = tree
                .desktop
                .workspaces
                .iter()
                .find(|w| w.id == tree.desktop.active)
            else {
                return Some((false, String::new(), generation));
            };
            if ws.room.is_nil() {
                return Some((false, String::new(), generation));
            }
            let labels = tree.room_peer_labels(ws.room);
            // Two ways to be present: moving cursor, or CRDT participation.
            let room = ws.room.to_string();
            let mut arrived = handles_seen_in(presence, &room);
            // A pointer this device can't place (its tab isn't shown here)
            // is still someone pointing at the room's work.
            if let Ok(anchored) = self.anchored.lock() {
                arrived.extend(
                    anchored
                        .iter()
                        .filter(|(_, (r, _, _))| *r == room)
                        .filter_map(|(user, _)| user.strip_prefix("peer@")?.parse::<u64>().ok()),
                );
            }
            // Taking part in the room's documents is being here, unless the
            // person said they are not: someone who shows nothing there.
            // Their pointer, if they show one again, brings them back above.
            let absent = self
                .absent
                .lock_recover()
                .get(&room)
                .cloned()
                .unwrap_or_default();
            arrived.extend(
                replica
                    .participants_in(ws.room)
                    .into_iter()
                    .filter(|handle| !absent.contains(handle)),
            );
            let principals = tree.room_peer_principals(ws.room);
            let mut rows = room_roster(ws, &labels, &arrived, local, &principals);
            for row in &mut rows {
                // A row named by the id its presence carries is that
                // presence; only a row named otherwise is found by its name,
                // which two people's devices can share.
                let own = ui::permissions::UserId(row.id.clone());
                let presence_id = if presence.names.contains_key(&own)
                    || presence.here.values().any(|users| users.contains(&own))
                {
                    own
                } else {
                    presence
                        .names
                        .iter()
                        .find_map(|(id, name)| name.eq_ignore_ascii_case(&row.name).then_some(id))
                        .cloned()
                        .unwrap_or(own)
                };
                row.active_tab = presence.here.iter().find_map(|(key, users)| {
                    let PresenceKey::Tab { id } = key else {
                        return None;
                    };
                    users
                        .contains(&presence_id)
                        .then(|| u64::from_str_radix(id, 16).ok())
                        .flatten()
                });
            }
            let unchanged = ws.collaborators == rows && tree.desktop.collaborators == rows;
            let active = ws.id;
            // Drop the read guard as soon as it's no longer needed, rather
            // than holding it to the end of the block below.
            drop(tree);
            if unchanged {
                return Some((false, room, generation));
            }
            (active, rows, room)
        };
        let Ok(replica) = self.net.replica.lock() else {
            return None;
        };
        let mut tree = replica.tree.write();
        // The write lock is held, so nobody else writes before this guard's
        // release advances the generation by one.
        let written = if self.roster_tree.generation() == generation {
            generation.wrapping_add(1)
        } else {
            generation
        };
        if let Some(workspace) = tree
            .desktop
            .workspaces
            .iter_mut()
            .find(|ws| ws.id == active)
        {
            workspace.collaborators.clone_from(&rows);
        }
        // Keep the migration-era cache in sync for older readers. New shell
        // chrome always reads the active workspace directly, so this cache
        // cannot leak another workspace's presence into the top bar.
        tree.desktop.collaborators = rows;
        drop(tree);
        Some((true, room, written))
    }
}

/// An order-insensitive fingerprint of what the roster reads from presence
/// in `room`: whose pointer is seen there (placed, or anchored to a tab this
/// device does not show), every name, and who is on which tab. Equal prints
/// mean a re-derived roster would read the same presence.
fn roster_presence_print(
    presence: &PresenceCtx,
    anchored: &Mutex<AnchoredPointers>,
    absent: &Mutex<HashMap<String, std::collections::BTreeSet<u64>>>,
    room: &str,
) -> u64 {
    use std::hash::{Hash, Hasher};
    fn hash_of(value: impl Hash) -> u64 {
        let mut hasher = std::hash::DefaultHasher::new();
        value.hash(&mut hasher);
        hasher.finish()
    }
    // A sum, not XOR, so two equal entries do not cancel out.
    let mut print = 0u64;
    let mut add = |hash: u64| print = print.wrapping_add(hash);
    for (key, peers) in &presence.pointers {
        if matches!(key, PresenceKey::Workspace { id } if id.as_str() == room) {
            for (user, _) in peers {
                add(hash_of((0u8, &user.0)));
            }
        }
    }
    for (user, name) in &presence.names {
        add(hash_of((1u8, &user.0, name)));
    }
    for (key, users) in &presence.here {
        if let PresenceKey::Tab { id } = key {
            for user in users {
                add(hash_of((2u8, id, &user.0)));
            }
        }
    }
    if let Ok(anchored) = anchored.lock() {
        for (user, (anchored_in, _, _)) in anchored.iter() {
            if anchored_in.as_str() == room {
                add(hash_of((3u8, user)));
            }
        }
    }
    if let Ok(absent) = absent.lock()
        && let Some(gone) = absent.get(room)
    {
        for handle in gone {
            add(hash_of((4u8, handle)));
        }
    }
    print
}

/// The peer handles this session has had presence from in `room`.
///
/// Reads the same [`PresenceCtx`] the overlay paints cursors from, so the
/// roster and the desktop can never disagree about who is present.
fn handles_seen_in(presence: &PresenceCtx, room: &str) -> std::collections::BTreeSet<u64> {
    let mut out = std::collections::BTreeSet::new();
    for (key, peers) in &presence.pointers {
        let PresenceKey::Workspace { id } = key else {
            continue;
        };
        if id.as_str() != room {
            continue;
        }
        for (user, _) in peers {
            if let Some(handle) = user.0.strip_prefix("peer@")
                && let Ok(handle) = handle.parse::<u64>()
            {
                out.insert(handle);
            }
        }
    }
    out
}

/// What a room calls the peers in it, keyed by the handle presence
/// frames identify their sender by.
pub type PeerLabels = std::collections::BTreeMap<u64, String>;

/// Every peer pointer received and not yet superseded, by presence id
/// (`peer@<handle>`): the room it was sent in, where it is anchored, and
/// when it was last heard. Kept after it goes quiet — somebody who pointed
/// at the room's work is still in the room — but no longer placed.
pub type AnchoredPointers = HashMap<String, (String, PointerAnchor, Instant)>;

/// Fold inbound presence into `anchored`, naming each sender in
/// `presence`, and return the ids of the peers whose pointer arrived.
/// Split from [`PeerPresence::pump`] so it can be tested without a
/// network; `labels` is a parameter for the same reason (production wires
/// [`ShellNetwork::peer_labels`]).
///
/// Only a pointer anchored to a room's tab is taken. A position in the
/// sender's desktop coordinates is refused: it names a place on monitors
/// this device doesn't have.
pub fn fold_inbound(
    entries: Vec<crate::shell::network::InboundPresence>,
    anchored: &mut AnchoredPointers,
    presence: &mut PresenceCtx,
    labels: &dyn Fn(&str) -> PeerLabels,
    member_of: &dyn Fn(&str) -> bool,
    now: Instant,
) -> Vec<String> {
    let mut arrived = Vec::new();
    // Entries in one drain are nearly always the same room.
    let mut resolved: Option<(String, PeerLabels)> = None;
    for entry in entries {
        let Ok(PresenceKey::Pointer {
            space: ui::presence::PointerSpace::Tile { room, tab },
        }) = serde_json::from_str::<PresenceKey>(&entry.key)
        else {
            continue;
        };
        // Only rooms this session is in — an access rule.
        if !member_of(&room) {
            continue;
        }
        // Spelled from the handle, never `entry.user`'s unverified claim.
        let user = format!("peer@{}", entry.peer);
        if entry.cleared {
            // Taken away from this room's windows: gone at once, here too.
            if anchored.get(&user).is_some_and(|(at, _, _)| *at == room) {
                anchored.remove(&user);
                presence.clear_pointer(&user);
            }
            continue;
        }
        if !matches!(&resolved, Some((seen, _)) if *seen == room) {
            resolved = Some((room.clone(), labels(&room)));
        }
        if let Some((_, known)) = &resolved
            && let Some(name) = known.get(&entry.peer)
        {
            presence.set_display_name(ui::permissions::UserId(user.clone()), name.clone());
        }
        let anchor = PointerAnchor {
            tab,
            x: entry.point.0,
            y: entry.point.1,
        };
        anchored.insert(user.clone(), (room, anchor, now));
        arrived.push(user);
    }
    arrived
}

/// Fold what live members said beside their pointers and reacted with into
/// `presence` ([`ui::presence::CursorChats`]), for rooms this session is in.
/// Someone talking keeps their pointer: a person typing holds still, and a
/// still pointer is otherwise retired once it goes quiet, so each frame that
/// carries a message counts as hearing the pointer again (`fresh`), where it
/// was. Returns whether anything shown changed or is still moving.
pub fn fold_live_chat(
    said: Vec<crate::shell::network::LiveChat>,
    anchored: &mut AnchoredPointers,
    fresh: &mut Vec<String>,
    presence: &mut PresenceCtx,
    member_of: &dyn Fn(&str) -> bool,
    now: Instant,
) -> bool {
    let mut changed = false;
    for entry in said {
        if !member_of(&entry.room) {
            continue;
        }
        let user = format!("peer@{}", entry.peer);
        let id = ui::permissions::UserId(user.clone());
        changed |= presence.chats.say(
            &id,
            entry
                .chat
                .as_ref()
                .map(|chat| (chat.text.as_str(), chat.sent)),
            now,
        );
        if let Some(reaction) = &entry.reaction {
            changed |= presence
                .chats
                .react(&id, reaction.seq, &reaction.emoji, now);
        }
        if presence.chats.talking(&id, now)
            && let Some((room, _, heard)) = anchored.get_mut(&user)
            && *room == entry.room
        {
            *heard = now;
            if !fresh.contains(&user) {
                fresh.push(user);
            }
        }
    }
    changed |= presence.chats.prune(now);
    changed || presence.chats.animating(now)
}

/// Put every pointer in `anchored` where its tab's tile is on this device
/// (`tiles`), filed under its room the way the overlay paints them, and
/// take off screen any whose tab this device doesn't show. `fresh` peers
/// are restated even when they haven't moved, so a keepalive keeps them
/// current. Each is stamped with when it was last *heard*, not placed, so
/// a pointer riding a scrolling tile still goes quiet on time: one not
/// heard for [`ui::presence::POINTER_IDLE`] is retired
/// ([`PresenceCtx::prune_pointers`]) and not placed again until it is.
/// The same stamp is what tells the presence store a tile carrying the
/// pointer from the peer moving it: only a newly heard position glides
/// ([`PresenceCtx::pointer_position_at`]).
/// Returns whether anything landed, moved, went, or is fading or gliding.
/// How long a peer's pointer stays painted after it was last heard: while
/// it is in the room's windows, however still (presence.md: only its name
/// steps aside). It goes when its person takes it away (`PresenceCleared`),
/// leaves the room, or stops showing where they are.
pub const POINTER_STAYS: Duration = Duration::MAX;

pub fn place_pointers(
    anchored: &AnchoredPointers,
    fresh: &[String],
    presence: &mut PresenceCtx,
    tiles: &TabTiles,
    now: Instant,
) -> bool {
    use ui::presence::POINTER_FADE;
    let mut changed = false;
    for (user, (room, anchor, heard)) in anchored {
        let Some((x, y)) = place_pointer(tiles, *anchor) else {
            changed |= presence.clear_pointer(user);
            continue;
        };
        let key = PresenceKey::Workspace { id: room.clone() };
        let at = (f32_of_f64(x), f32_of_f64(y));
        let placed = presence
            .pointers
            .get(&key)
            .and_then(|list| list.iter().find(|(u, _)| u.0 == *user))
            .map(|(_, at)| *at);
        // Restated where it already is (a late joiner's welcome, or an older
        // build's keepalive): heard again, so it does not fade, but nothing
        // on screen moves unless it had started fading.
        let restated = placed == Some(at);
        if restated && !fresh.contains(user) {
            continue;
        }
        let fading = restated
            && presence.pointer_opacity(
                &key,
                &ui::permissions::UserId(user.clone()),
                now,
                POINTER_STAYS,
                POINTER_FADE,
            ) < 1.0;
        // Stamped with when it was heard rather than now: moving with a
        // tile is not the peer saying anything.
        presence.apply_remote_presence_at(
            user.clone(),
            key,
            PresencePayload::Pointer { x: at.0, y: at.1 },
            *heard,
        );
        changed |= !restated || fading;
    }
    changed |= presence.prune_pointers(now, POINTER_STAYS);
    changed
        || presence.pointers_fading(now, POINTER_STAYS, POINTER_FADE)
        || presence.pointers_easing(now)
        || presence.pointer_names_fading(now, POINTER_FADE)
}

/// The people in `ws`'s room. Two sources, neither sufficient alone:
/// `labels` is the room document's answer (everyone its authority has
/// named, including an invitee who never moved their mouse), `arrived`
/// is who actually turned up (presence plus CRDT participants).
///
/// A named peer that hasn't arrived is marked `Collaborator::invited`,
/// keyed off the transport-authenticated identity so a peer cannot
/// promote its own row. `local` is dropped — the roster is who *else*
/// is here.
///
/// The role chip comes from the policy, asked about the person a handle's
/// certified device belongs to (`principals`, from
/// [`crate::shell::state::WorkspaceTree::room_peer_principals`]) — the key
/// membership capabilities are written under. A label is what a person is
/// *called*, not who they are; it is asked only for a handle with no
/// certified person, which covers the owner's own endpoint, labelled with
/// its user id.
pub fn room_roster(
    ws: &crate::shell::state::Workspace,
    labels: &PeerLabels,
    arrived: &std::collections::BTreeSet<u64>,
    local: u64,
    principals: &std::collections::BTreeMap<u64, crate::shell::state::UserId>,
) -> Vec<crate::shell::state::Collaborator> {
    let mut handles: std::collections::BTreeSet<u64> = labels.keys().copied().collect();
    handles.extend(arrived.iter().copied());
    handles.remove(&local);
    handles
        .into_iter()
        .map(|handle| {
            // Same spelling as the presence path, so avatar and cursor share an id.
            let id = format!("peer@{handle}");
            let name = labels.get(&handle).cloned().unwrap_or_else(|| id.clone());
            let role = match principals.get(&handle) {
                Some(person) => ws.role_of(person),
                None => ws.role_of(&name),
            };
            let who = crate::shell::state::Collaborator::new(id, name).with_session(handle);
            let who = match role {
                Some(role) => who.with_role(crate::shell::state::role_id(role)),
                None => who,
            };
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
    use std::time::SystemTime;
    use ui::permissions::UserId;

    fn interaction(actor: &str) -> PresencePayload {
        PresencePayload::Interaction(Box::new(tilo_actions::SemanticInteraction {
            version: tilo_actions::INTERACTION_VERSION,
            invocation: tilo_actions::InvocationId::new(),
            sequence: 1,
            actor: tilo_actions::InteractionActor {
                id: actor.to_string(),
                kind: tilo_actions::ActorKind::Person,
                for_person: None,
            },
            phase: tilo_actions::InteractionPhase::Applying,
            action: tilo_actions::InteractionAction {
                id: "edit".into(),
                label: "Editing".into(),
                app: None,
            },
            resource: tilo_actions::ActionResource {
                id: "report:q3".into(),
                kind: "document".into(),
                label: "Q3 report".into(),
            },
            anchor: tilo_actions::InteractionAnchor::Resource,
            group: None,
        }))
    }

    /// An actor the broker or the network's ingress vouched for arrives
    /// under its own id and keeps whom it acts for; one that names someone
    /// other than its sender does not.
    #[test]
    fn a_vouched_agent_keeps_whom_it_acts_for() {
        let resource = PresenceKey::Resource {
            app: "org.tilo.Writer".into(),
            id: "report:q3".into(),
            kind: "document".into(),
        };
        let agent = |for_person: &str| {
            let PresencePayload::Interaction(mut update) = interaction("agent:claude") else {
                unreachable!("interaction() builds an interaction")
            };
            update.actor =
                tilo_actions::InteractionActor::new("agent:claude", tilo_actions::ActorKind::Agent)
                    .acting_for(for_person);
            PresencePayload::Interaction(update)
        };
        let mut presence = PresenceCtx::default();
        presence.set_participant_kind(UserId("agent:claude".into()), true);
        assert!(fold_typed_presence(
            vec![
                ("agent:claude".into(), resource.clone(), agent("ada")),
                ("peer@9".into(), resource, agent("bob")),
            ],
            &mut presence,
        ));
        let visible = presence.visible_interactions(Instant::now());
        let claude = visible
            .iter()
            .find(|update| update.actor.id == "agent:claude")
            .expect("the vouched agent");
        assert_eq!(claude.actor.for_person.as_deref(), Some("ada"));
        let forged = visible
            .iter()
            .find(|update| update.actor.id == "peer@9")
            .expect("attributed to its sender");
        assert_eq!(forged.actor.for_person, None);
        assert_eq!(forged.actor.kind, tilo_actions::ActorKind::Person);
        assert!(!presence.is_agent(&UserId("peer@9".into())));
    }

    #[test]
    fn a_payload_cannot_promote_an_unrostered_person_to_an_agent() {
        let mut presence = PresenceCtx::default();
        let PresencePayload::Interaction(mut update) = interaction("agent:invented") else {
            unreachable!();
        };
        update.actor.kind = tilo_actions::ActorKind::Agent;
        fold_typed_presence(
            vec![(
                "agent:invented".into(),
                PresenceKey::Resource {
                    app: "org.tilo.Writer".into(),
                    id: "report:q3".into(),
                    kind: "document".into(),
                },
                PresencePayload::Interaction(update),
            )],
            &mut presence,
        );
        assert!(!presence.is_agent(&UserId("agent:invented".into())));
        assert!(
            presence
                .agent_marker_interactions(Instant::now())
                .is_empty()
        );
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn remote_actions_are_attributed_and_departure_clears_only_dead_presence() {
        fn shell_observes_remote_action_and_intent_without_spoofed_attribution() {
            let room = PresenceKey::Workspace {
                id: "room-7".into(),
            };
            let tab = PresenceKey::Tab { id: "2a".into() };
            let resource = PresenceKey::Resource {
                app: "org.tilo.Writer".into(),
                id: "report:q3".into(),
                kind: "document".into(),
            };
            let status = PresencePayload::Status {
                text: "Preparing the board review".into(),
                automatic: true,
            };
            assert!(typed_presence_authorized(&room, &status, "room-7"));
            assert!(!typed_presence_authorized(&room, &status, "another-room"));
            assert!(typed_presence_authorized(
                &resource,
                &interaction("forged-id"),
                "room-7"
            ));
            assert!(!typed_presence_authorized(
                &PresenceKey::Resource {
                    app: "org.tilo.Writer".into(),
                    id: "another-report".into(),
                    kind: "document".into(),
                },
                &interaction("forged-id"),
                "room-7"
            ));
            assert!(typed_presence_authorized(
                &tab,
                &PresencePayload::Arrived,
                "room-7"
            ));

            let mut presence = PresenceCtx::default();
            assert!(fold_typed_presence(
                vec![
                    ("ada".into(), room, status),
                    ("ada".into(), resource, interaction("forged-id")),
                    ("ada".into(), tab.clone(), PresencePayload::Arrived),
                ],
                &mut presence,
            ));
            assert_eq!(
                presence
                    .status_for(&UserId("ada".into()), SystemTime::now())
                    .map(|value| value.text.as_str()),
                Some("Preparing the board review")
            );
            let visible = presence.visible_interactions(Instant::now());
            assert_eq!(visible.len(), 1);
            assert_eq!(visible[0].actor.id, "ada");
            assert_eq!(visible[0].action.label, "Editing");
            assert_eq!(
                presence.here.get(&tab),
                Some(&vec![UserId("ada".into())]),
                "the shell retains the app's active tab for the People location line"
            );
            let next_tab = PresenceKey::Tab { id: "2b".into() };
            assert!(fold_typed_presence(
                vec![("ada".into(), next_tab.clone(), PresencePayload::Arrived,)],
                &mut presence,
            ));
            assert!(presence.here.get(&tab).is_none_or(std::vec::Vec::is_empty));
            assert_eq!(
                presence.here.get(&next_tab),
                Some(&vec![UserId("ada".into())])
            );
            assert!(fold_typed_presence(
                vec![("ada".into(), next_tab.clone(), PresencePayload::Left)],
                &mut presence,
            ));
            assert!(
                presence
                    .here
                    .get(&next_tab)
                    .is_none_or(std::vec::Vec::is_empty),
                "closing the tab clears its roster location without disconnecting Ada"
            );
            assert!(
                presence
                    .status_for(&UserId("ada".into()), SystemTime::now())
                    .is_some(),
                "tab lifecycle does not erase room-scoped intent"
            );
        }

        fn abrupt_departure_clears_named_presence_unless_another_device_is_live() {
            let mut presence = PresenceCtx::default();
            let ada = UserId("ada".into());
            let old_transport = UserId("peer@41".into());
            let tab = PresenceKey::Tab { id: "2a".into() };
            presence.register_presence(&tab, ada.clone());
            presence.register_presence(&tab, old_transport);
            presence.apply_remote_presence(
                "ada".into(),
                PresenceKey::Workspace {
                    id: "room-7".into(),
                },
                PresencePayload::Status {
                    text: "Preparing the board review".into(),
                    automatic: false,
                },
            );
            let PresencePayload::Interaction(update) = interaction("ada") else {
                unreachable!();
            };
            assert!(presence.observe_interaction(*update).unwrap());

            let ids =
                departure_presence_ids(41, Some("ada"), &std::collections::BTreeMap::default());
            assert_eq!(ids, vec!["peer@41", "ada"]);
            for id in ids {
                presence.unregister_peer(&id);
            }
            assert!(presence.here.values().all(std::vec::Vec::is_empty));
            assert!(presence.status_for(&ada, SystemTime::now()).is_none());
            assert!(presence.visible_interactions(Instant::now()).is_empty());

            let still_live = std::collections::BTreeMap::from([(73, "ada".to_string())]);
            assert_eq!(
                departure_presence_ids(41, Some("ada"), &still_live),
                vec!["peer@41"],
                "one device leaving must not erase the person's other live session"
            );
        }

        shell_observes_remote_action_and_intent_without_spoofed_attribution();
        abrupt_departure_clears_named_presence_unless_another_device_is_live();
    }

    #[test]
    fn local_pointer_is_announced_only_after_moving_and_throttled_by_the_gap() {
        fn a_session_that_has_said_nothing_speaks() {
            assert!(should_publish_cursor(
                None,
                (10.0, 10.0),
                Instant::now(),
                MIN_PUBLISH_GAP,
                false,
            ));
        }

        fn a_pointer_that_has_never_moved_is_not_announced_at_the_origin() {
            let now = Instant::now();

            assert_eq!(
                cursor_to_publish::<(f64, f64)>(None, None, now, MIN_PUBLISH_GAP, false),
                None,
                "a session whose pointer has never moved must not announce one"
            );

            // The origin is not banned once actually moved to.
            assert_eq!(
                cursor_to_publish(Some((0.0, 0.0)), None, now, MIN_PUBLISH_GAP, false),
                Some((0.0, 0.0)),
                "a pointer somebody actually moved to the origin is still a pointer"
            );
        }

        fn a_move_inside_the_gap_is_held() {
            let t0 = Instant::now();
            assert!(
                !should_publish_cursor(
                    Some(((10.0, 10.0), t0)),
                    (11.0, 10.0),
                    t0 + Duration::from_millis(5),
                    MIN_PUBLISH_GAP,
                    false,
                ),
                "a pointer moving at frame rate must not put every sample on the wire"
            );
        }

        fn a_move_past_the_gap_goes_out() {
            let t0 = Instant::now();
            assert!(should_publish_cursor(
                Some(((10.0, 10.0), t0)),
                (400.0, 300.0),
                t0 + MIN_PUBLISH_GAP,
                MIN_PUBLISH_GAP,
                false,
            ));
        }

        fn a_pointer_that_stopped_is_restated_for_whoever_joined_since() {
            let t0 = Instant::now();
            let at = (10.0, 10.0);
            assert!(
                !should_publish_cursor(
                    Some((at, t0)),
                    at,
                    t0 + Duration::from_millis(200),
                    MIN_PUBLISH_GAP,
                    false,
                ),
                "a still pointer must not republish at frame rate"
            );
            assert!(
                !should_publish_cursor(
                    Some((at, t0)),
                    at,
                    t0 + Duration::from_secs(3600),
                    MIN_PUBLISH_GAP,
                    false,
                ),
                "nor on a timer: everyone in the room already knows where it rests"
            );
            assert!(
                should_publish_cursor(
                    Some((at, t0)),
                    at,
                    t0 + Duration::from_millis(200),
                    MIN_PUBLISH_GAP,
                    true,
                ),
                "but a peer that joined since has not heard, so it is said again"
            );
        }

        a_session_that_has_said_nothing_speaks();
        a_pointer_that_has_never_moved_is_not_announced_at_the_origin();
        a_move_inside_the_gap_is_held();
        a_move_past_the_gap_goes_out();
        a_pointer_that_stopped_is_restated_for_whoever_joined_since();
    }

    /// The tab the frames below point at, and the tile this desk shows it
    /// in: at the origin, so a placed pointer reads the offset it was sent.
    const TAB: u64 = 0x5eed;
    const DESK_TILES: [(u64, vello::kurbo::Rect); 1] =
        [(TAB, vello::kurbo::Rect::new(0.0, 0.0, 2000.0, 2000.0))];

    /// A frame as it arrives off the wire. `user` is the sender's own
    /// (untrusted) claimed name — tests below pass a wrong one on purpose.
    fn arrived(user: &str, peer: u64, room: &str, at: (f64, f64)) -> InboundPresence {
        InboundPresence {
            peer,
            user: user.into(),
            key: serde_json::to_string(&PresenceKey::tile_pointer(room, TAB))
                .expect("a tile pointer key serialises"),
            point: at,
            ts: 0,
            cleared: false,
        }
    }

    /// What [`PeerPresence::pump`] does with a drain, over [`DESK_TILES`]:
    /// fold it, then place it. Returns whether anything landed.
    fn fold(
        entries: Vec<InboundPresence>,
        presence: &mut PresenceCtx,
        labels: &dyn Fn(&str) -> PeerLabels,
        member_of: &dyn Fn(&str) -> bool,
    ) -> bool {
        let mut anchored = AnchoredPointers::new();
        let now = Instant::now();
        let fresh = fold_inbound(entries, &mut anchored, presence, labels, member_of, now);
        place_pointers(&anchored, &fresh, presence, &DESK_TILES, now)
    }

    #[test]
    fn a_peer_pointer_is_anchored_to_a_tab_and_placed_over_this_desks_tile_for_it() {
        use vello::kurbo::Rect;
        /// Over a tile, the offset is from that tile's corner; in the gap
        /// beside one or on its tab bar, from the nearest tile's; with
        /// nothing shown there is nothing the room shares to anchor to.
        fn a_pointer_anchors_to_the_tile_under_it_or_the_nearest() {
            let tiles = [
                (1, Rect::new(10.0, 60.0, 410.0, 460.0)),
                (2, Rect::new(430.0, 60.0, 830.0, 460.0)),
            ];
            assert_eq!(
                anchor_pointer(&tiles, (500.0, 100.0)),
                Some(PointerAnchor {
                    tab: 2,
                    x: 70.0,
                    y: 40.0
                }),
            );
            assert_eq!(
                anchor_pointer(&tiles, (425.0, 40.0)),
                Some(PointerAnchor {
                    tab: 2,
                    x: -5.0,
                    y: -20.0
                }),
                "a pointer in the gap and above the tiles belongs to the nearer one",
            );
            assert_eq!(anchor_pointer(&[], (500.0, 100.0)), None);
        }

        /// A position in the sender's desktop coordinates names a place on
        /// monitors this device doesn't have, so it is not drawn at all.
        fn a_desktop_coordinate_pointer_is_refused() {
            let room = "3f2b1c00-0000-4000-8000-000000000001";
            let mut presence = PresenceCtx::default();
            let entry = InboundPresence {
                peer: 7,
                user: "bob".into(),
                key: serde_json::to_string(&PresenceKey::Workspace { id: room.into() })
                    .expect("a workspace key serialises"),
                point: (120.0, 240.0),
                ts: 0,
                cleared: false,
            };
            assert!(!fold(vec![entry], &mut presence, &unnamed, &any_room));
            assert!(presence.pointers.is_empty(), "{:?}", presence.pointers);
        }

        /// The pointer follows this desk's tile as it moves, disappears
        /// while the tab isn't shown here, and comes back when it is.
        fn a_placed_pointer_follows_the_tile_and_hides_with_it() {
            let room = "3f2b1c00-0000-4000-8000-000000000001";
            let key = PresenceKey::Workspace { id: room.into() };
            let mut presence = PresenceCtx::default();
            let mut anchored = AnchoredPointers::new();
            let now = Instant::now();
            let fresh = fold_inbound(
                vec![arrived("bob", 7, room, (30.0, 20.0))],
                &mut anchored,
                &mut presence,
                &unnamed,
                &any_room,
                now,
            );
            let at = |presence: &PresenceCtx| {
                presence
                    .pointers
                    .get(&key)
                    .and_then(|p| p.first())
                    .map(|p| p.1)
            };
            let here = [(TAB, Rect::new(1300.0, 50.0, 1900.0, 700.0))];
            assert!(place_pointers(&anchored, &fresh, &mut presence, &here, now));
            assert_eq!(at(&presence), Some((1330.0, 70.0)));

            let scrolled = [(TAB, Rect::new(900.0, 50.0, 1500.0, 700.0))];
            assert!(place_pointers(
                &anchored,
                &[],
                &mut presence,
                &scrolled,
                now
            ));
            assert_eq!(at(&presence), Some((930.0, 70.0)), "it rides the tile");
            assert!(
                !place_pointers(&anchored, &[], &mut presence, &scrolled, now),
                "and a frame where nothing moved changes nothing",
            );

            assert!(place_pointers(&anchored, &[], &mut presence, &[], now));
            assert_eq!(
                at(&presence),
                None,
                "a tab not shown here hides the pointer"
            );
            assert!(presence.remote_cursors.is_empty());

            assert!(place_pointers(&anchored, &[], &mut presence, &here, now));
            assert_eq!(at(&presence), Some((1330.0, 70.0)));
        }

        a_pointer_anchors_to_the_tile_under_it_or_the_nearest();
        a_desktop_coordinate_pointer_is_refused();
        a_placed_pointer_follows_the_tile_and_hides_with_it();
    }

    #[path = "still_pointer_tests.rs"]
    mod still_pointer;

    /// A resting pointer said again where it already is (an older build's
    /// keepalive, or the welcome a late joiner gets) moves nothing on
    /// screen: no repaint.
    #[test]
    fn identical_restated_pointer_does_not_repaint() {
        use vello::kurbo::Rect;
        let room = "3f2b1c00-0000-4000-8000-000000000002";
        let key = PresenceKey::Workspace { id: room.into() };
        let bob = ui::permissions::UserId("peer@7".into());
        let mut presence = PresenceCtx::default();
        let mut anchored = AnchoredPointers::new();
        let here = [(TAB, Rect::new(100.0, 50.0, 700.0, 700.0))];
        let heard = |anchored: &mut AnchoredPointers, presence: &mut PresenceCtx, at| {
            fold_inbound(
                vec![arrived("bob", 7, room, (30.0, 20.0))],
                anchored,
                presence,
                &unnamed,
                &any_room,
                at,
            )
        };
        let t0 = Instant::now();
        let fresh = heard(&mut anchored, &mut presence, t0);
        assert!(
            place_pointers(&anchored, &fresh, &mut presence, &here, t0),
            "a pointer arriving is painted"
        );

        let again = t0 + Duration::from_secs(1);
        let fresh = heard(&mut anchored, &mut presence, again);
        assert!(
            !place_pointers(&anchored, &fresh, &mut presence, &here, again),
            "the same place heard again paints nothing"
        );
        // Still fully drawn long after: a resting pointer does not fade.
        let later = again + Duration::from_secs(60);
        assert!(!place_pointers(&anchored, &[], &mut presence, &here, later));
        assert_eq!(presence.pointer_for(&key, &bob), Some((130.0, 70.0)));
    }

    fn unnamed(_room: &str) -> PeerLabels {
        PeerLabels::new()
    }

    /// This session is in every room a frame might name — default for
    /// tests not about membership.
    fn any_room(_room: &str) -> bool {
        true
    }

    fn named(pairs: &[(u64, &str)]) -> impl Fn(&str) -> PeerLabels + use<> {
        let map: PeerLabels = pairs.iter().map(|(h, n)| (*h, (*n).to_string())).collect();
        move |_room: &str| map.clone()
    }

    #[test]
    fn peers_land_on_the_overlays_room_and_are_painted_once() {
        fn a_peer_lands_on_the_room_the_overlay_reads() {
            let room = "3f2b1c00-0000-4000-8000-000000000001";
            let mut presence = PresenceCtx::default();
            assert!(fold(
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

        /// A folded frame lands in two stores — `pointers` (overlay paint)
        /// and `remote_cursors` (follow-that-peer) — and both being
        /// *painted* would draw one person twice.
        fn a_desktop_pointer_is_painted_by_the_overlay_and_not_again_by_the_window() {
            let room = "3f2b1c00-0000-4000-8000-000000000001";
            let mut presence = PresenceCtx::default();
            fold(
                vec![arrived("bob", 7, room, (120.0, 240.0))],
                &mut presence,
                &named(&[(7, "bob")]),
                &any_room,
            );

            let peers = presence
                .pointers
                .get(&PresenceKey::Workspace { id: room.into() })
                .expect("the overlay's store must hold the peer it is to paint");
            assert_eq!(peers.len(), 1, "one arrival, one peer");

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

            assert_eq!(
                presence.peer_pointer(&cursor.user_id),
                Some([120.0, 240.0]),
                "following a peer must still land where their cursor shows",
            );
        }

        a_peer_lands_on_the_room_the_overlay_reads();
        a_desktop_pointer_is_painted_by_the_overlay_and_not_again_by_the_window();
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn peer_sightings_are_one_entry_per_person_filed_under_their_named_room() {
        fn one_person_is_one_entry_across_every_room_in_the_store() {
            let mine = "3f2b1c00-0000-4000-8000-000000000001";
            let theirs = "3f2b1c00-0000-4000-8000-0000000000ff";
            let mut presence = PresenceCtx::default();

            fold(
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

        fn a_room_this_session_is_not_in_is_refused() {
            let elsewhere = "3f2b1c00-0000-4000-8000-0000000000ff";
            let mut presence = PresenceCtx::default();

            let any = fold(
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

        fn one_session_stays_one_peer_across_two_apps() {
            let room = "3f2b1c00-0000-4000-8000-000000000001";
            let mut presence = PresenceCtx::default();
            fold(
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

        fn two_rooms_do_not_share_peers() {
            let here = "3f2b1c00-0000-4000-8000-000000000001";
            let there = "3f2b1c00-0000-4000-8000-0000000000ff";
            let mut presence = PresenceCtx::default();
            fold(
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

        fn a_folded_frame_files_the_sender_under_the_room_it_named() {
            let room = "3f2b1c00-0000-4000-8000-000000000001";
            let mut presence = PresenceCtx::default();
            let any = fold(
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

        fn a_frame_that_names_no_room_yields_no_sighting() {
            let mut presence = PresenceCtx::default();
            let entry = InboundPresence {
                peer: 9,
                user: "carol".into(),
                key: serde_json::to_string(&PresenceKey::Tab { id: "2a".into() })
                    .expect("a tab key serialises"),
                point: (3.0, 4.0),
                ts: 0,
                cleared: false,
            };
            fold(vec![entry], &mut presence, &unnamed, &any_room);
            assert!(
                !presence
                    .pointers
                    .keys()
                    .any(|k| matches!(k, PresenceKey::Workspace { .. })),
                "a frame naming no room put somebody in one: {:?}",
                presence.pointers.keys().collect::<Vec<_>>(),
            );
        }

        one_person_is_one_entry_across_every_room_in_the_store();
        a_room_this_session_is_not_in_is_refused();
        one_session_stays_one_peer_across_two_apps();
        two_rooms_do_not_share_peers();
        a_folded_frame_files_the_sender_under_the_room_it_named();
        a_frame_that_names_no_room_yields_no_sighting();
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn peer_names_come_from_the_room_without_moving_colour_or_conjuring_cursors() {
        fn a_peer_is_called_what_the_room_calls_it() {
            let room = "3f2b1c00-0000-4000-8000-000000000001";
            let mut presence = PresenceCtx::default();
            fold(
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

        fn a_sender_cannot_name_itself() {
            let room = "3f2b1c00-0000-4000-8000-000000000001";
            let mut presence = PresenceCtx::default();
            fold(
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

        fn a_name_does_not_move_a_peer_to_a_new_colour() {
            let room = "3f2b1c00-0000-4000-8000-000000000001";
            let mut before = PresenceCtx::default();
            fold(
                vec![arrived("", 7, room, (10.0, 10.0))],
                &mut before,
                &unnamed,
                &any_room,
            );
            let mut after = PresenceCtx::default();
            fold(
                vec![arrived("", 7, room, (10.0, 10.0))],
                &mut after,
                &named(&[(7, "bob")]),
                &any_room,
            );
            let key = PresenceKey::Workspace { id: room.into() };
            let a = &before.pointers.get(&key).expect("a pointer")[0].0;
            let b = &after.pointers.get(&key).expect("a pointer")[0].0;
            assert_eq!(
                a, b,
                "the identity must be the same before and after a name"
            );
            assert_eq!(
                before.color_for(a),
                after.color_for(b),
                "and so must the colour"
            );
        }

        fn naming_a_peer_does_not_conjure_a_cursor() {
            let mut presence = PresenceCtx::default();
            assert!(!fold(
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

        fn a_session_that_names_nobody_is_labelled_with_its_handle() {
            let room = "3f2b1c00-0000-4000-8000-000000000001";
            let mut presence = PresenceCtx::default();
            fold(
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
            assert_eq!(presence.display_name(&peers[0].0), "peer@42", "never blank");
        }

        a_peer_is_called_what_the_room_calls_it();
        a_sender_cannot_name_itself();
        a_name_does_not_move_a_peer_to_a_new_colour();
        naming_a_peer_does_not_conjure_a_cursor();
        a_session_that_names_nobody_is_labelled_with_its_handle();
    }

    /// A workspace `owner` owns, whose room admits `member` as `role` —
    /// the membership invite redemption writes, keyed by the person's key
    /// hex.
    fn room_admitting(
        owner: &str,
        member: &str,
        role: crate::shell::state::CapRole,
    ) -> crate::shell::state::Workspace {
        use crate::shell::state::{Workspace, WorkspaceId};
        let mut ws = Workspace::new(WorkspaceId(1), "Design review");
        ws.set_room(crate::shell::state::RoomId::new());
        ws.set_owner(owner.to_string());
        ws.govern_as_at(
            owner,
            tilo_action_policy::Change::AddMember {
                person: member.to_owned(),
                role,
            },
            tilo_clock::unix_now(),
        )
        .expect("the owner admits the member");
        ws
    }

    #[test]
    fn room_roster_takes_roles_from_the_certified_principal_not_the_label() {
        use crate::shell::state::{CapRole, role_id};
        let bob = crate::shell::state::person_id_of([0xb0; 32]).expect("a person key");
        let handle = 7_u64;
        let arrived = std::collections::BTreeSet::from([handle]);

        // Phase `an_invited_member_shows_their_policy_role`. Bob was
        // invited as an editor; the room calls him "Bob" and his
        // certified device belongs to his key. The chip is the policy's.
        {
            let ws = room_admitting("ada", &bob, CapRole::Editor);
            let labels = PeerLabels::from([(handle, "Bob".to_string())]);
            let principals = std::collections::BTreeMap::from([(handle, bob.clone())]);
            let rows = room_roster(&ws, &labels, &arrived, 1, &principals);
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].name, "Bob", "the label is still what he is called");
            assert_eq!(
                rows[0].role_id.as_deref(),
                Some(role_id(CapRole::Editor)),
                "an invited editor's row carries the role the Members tab shows",
            );
        }

        // Phase `a_label_spelling_somebody_else_s_id_does_not_borrow_their_role`.
        // A certified device is asked about by its person only: a label
        // that happens to read as the owner's id lends it nothing.
        {
            let ws = room_admitting("ada", &bob, CapRole::Viewer);
            let labels = PeerLabels::from([(handle, "ada".to_string())]);
            let principals = std::collections::BTreeMap::from([(handle, bob.clone())]);
            let rows = room_roster(&ws, &labels, &arrived, 1, &principals);
            assert_eq!(
                rows[0].role_id.as_deref(),
                Some(role_id(CapRole::Viewer)),
                "the chip is the certified person's, not whoever the label spells",
            );
        }

        // Phase `the_owner_endpoint_named_with_the_user_id_keeps_owner_role`.
        // The inviter labels its own endpoint with its user id and has no
        // certified device in the room; that row keeps the owner chip.
        {
            let ws = room_admitting("ada", &bob, CapRole::Editor);
            let labels = PeerLabels::from([(handle, "ada".to_string())]);
            let rows = room_roster(
                &ws,
                &labels,
                &arrived,
                1,
                &std::collections::BTreeMap::new(),
            );
            assert_eq!(
                rows[0].role_id.as_deref(),
                Some(role_id(CapRole::Owner)),
                "an uncertified endpoint labelled with the owner's id keeps the owner chip",
            );
        }

        // Phase `a_certified_person_without_membership_has_no_chip`.
        {
            let ws = room_admitting("ada", "someone-else", CapRole::Editor);
            let labels = PeerLabels::from([(handle, "Bob".to_string())]);
            let principals = std::collections::BTreeMap::from([(handle, bob)]);
            let rows = room_roster(&ws, &labels, &arrived, 1, &principals);
            assert_eq!(
                rows[0].role_id, None,
                "a certified person the room never admitted has no role to show",
            );
        }
    }
}

/// Two sessions, and a pointer that has to get from one to the other.
///
/// Everything above this point is asserted against one session's store,
/// with room set and names handed in as closures — not enough on its
/// own, since what a session publishes and what it accepts agree only
/// by construction. This drives the whole path end to end.
#[cfg(test)]
#[allow(
    clippy::significant_drop_tightening,
    reason = "test fixtures transitively own long-lived permits and lock guards by design"
)]
mod two_sessions {
    /// The tab every pointer below is over, and the tile this desk shows
    /// it in: at the origin, so a placed pointer reads the offset it was
    /// sent with.
    const DESK_TAB: u64 = 0x7ab;
    const DESK_TILES: [(u64, vello::kurbo::Rect); 1] =
        [(DESK_TAB, vello::kurbo::Rect::new(0.0, 0.0, 2000.0, 2000.0))];

    /// A pointer `(x, y)` from the corner of [`DESK_TAB`]'s tile.
    fn on_desk(x: f64, y: f64) -> PointerAnchor {
        PointerAnchor {
            tab: DESK_TAB,
            x,
            y,
        }
    }
    use super::*;
    use crate::network::actor::NetCmd;
    use crate::network::protocol::NeighbourMsg;
    use crate::shell::frame::FrameBuilder;
    use crate::shell::network::shell_network_for_test;
    use crate::shell::output_scene::OutputGeometry;
    use crate::shell::state::{GroupId, PeerOrigin, RoomId, WorkspaceTree};

    #[test]
    fn certified_people_replacement_and_room_removal_retire_software_presence_without_a_pointer() {
        let (tree, room) = host_naming(&[]);
        let tree = tree.shared();
        let (network, _commands) = shell_network_for_test(tree.clone(), 17);
        let network = Arc::new(network);
        let relay = PeerPresence::for_network(&network, rooms_of(&tree));
        let mut presence = PresenceCtx::default();
        let id = ui::permissions::UserId("opaque-certified-software".into());
        let row = ui::collaboration::Collaborator {
            id: id.0.clone(),
            name: "Tide".into(),
            colour: None,
            is_agent: true,
            is_self: false,
            joined_at: None,
        };
        relay.set_certified_people(&room.to_string(), vec![row.clone()]);
        relay.pump(&mut presence, &[]);
        assert!(
            !presence.is_agent(&id),
            "a certified roster in an inactive room must not leak into desktop presence"
        );
        // Appending a workspace does not focus it. Projection follows the
        // active room, so enter the fixture's room before checking its kind.
        {
            let mut tree = tree.write();
            let workspace = tree
                .desktop
                .workspaces
                .iter()
                .find(|workspace| workspace.room == room)
                .expect("the certified roster's room exists")
                .id;
            tree.set_active(workspace);
        }
        relay.pump(&mut presence, &[]);
        assert!(
            presence.is_agent(&id),
            "kind comes from the certified roster, not an id prefix"
        );
        assert!(
            presence.remote_cursors.is_empty(),
            "a roster is not pointer evidence"
        );
        relay.set_certified_people(&room.to_string(), Vec::new());
        relay.pump(&mut presence, &[]);
        assert!(!presence.is_agent(&id));
        assert!(!presence.names.contains_key(&id));
        relay.set_certified_people(&room.to_string(), vec![row]);
        relay.pump(&mut presence, &[]);
        assert!(presence.is_agent(&id));
        relay.set_rooms(Vec::new());
        relay.pump(&mut presence, &[]);
        assert!(!presence.is_agent(&id));
        assert!(!presence.names.contains_key(&id));
    }

    const OWNER: &str = "ada";

    /// A real Iroh endpoint id, minted from a keypair, so the parse is
    /// the production one.
    fn peer_id(seed: u8) -> String {
        iroh::SecretKey::from_bytes(&[seed; 32])
            .public()
            .to_string()
    }

    /// The handle presence frames identify `peer` by.
    fn handle_of(peer: &str) -> u64 {
        crate::network::behaviour::peer_liveness_handle(
            &peer.parse::<iroh::EndpointId>().expect("an endpoint id"),
        )
    }

    /// The rooms a session is in, read off its tree the way the frame
    /// loop does before calling [`PeerPresence::set_rooms`].
    fn rooms_of(tree: &crate::shell::state::WorkspaceTreeHandle) -> Vec<String> {
        tree.read()
            .desktop
            .workspaces
            .iter()
            .map(|w| w.room.to_string())
            .collect()
    }

    /// `spawn` is the removed legacy JSON-socket constructor; the typed
    /// Cap'n Proto broker under test opens its own socket the same way
    /// production does, just with no peer-sync bridge to Iroh.
    fn broker(dir: &std::path::Path, name: &str) -> crate::shell::collab_broker::BrokerHandle {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
            .expect("secure two-session broker fixture root");
        crate::shell::collab_broker::spawn_native(
            dir.join(name),
            &None,
            crate::shell::state::WorkspaceTree::default().shared(),
        )
        .expect("a broker on a temp socket")
    }

    /// A session standing in for the host: governs a room and has named
    /// everybody in it.
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

    /// A session that has joined `room`, with names replicated out of
    /// the room's document the way production does it.
    fn guest_joining(host: &mut WorkspaceTree, room: RoomId) -> WorkspaceTree {
        let mut tree = WorkspaceTree::default();
        let ws = tree.append_empty_workspace("Mine");
        assert!(
            tree.set_room(ws, room),
            "the guest has to end up in the room it joined",
        );
        // Its invite pins the host (hub 1) as the room's governor.
        assert!(tree.pin_room_authority(room, PeerOrigin::hub(1)));
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
                colour,
            } => {
                to.note_peer_colour(peer, colour.as_deref());
                to.handle_presence_message(peer, user, key, point, ts);
            }
            other => panic!("expected PresenceMoved, got {other:?}"),
        }
    }

    /// Take the one presence frame `cmds` is holding.
    fn published(cmds: &mut tokio::sync::mpsc::Receiver<NetCmd>) -> NeighbourMsg {
        match cmds
            .try_recv()
            .expect("the session must put its pointer on the wire")
        {
            NetCmd::Publish { msg, .. } => msg,
            other => panic!("expected a publish, got {other:?}"),
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn two_sessions_exchange_pointers_only_after_moving_and_only_in_shared_rooms() {
        /// Each session's pointer arrives on the other's screen under the
        /// room's name for it. Asserted in both directions — host and guest
        /// are not symmetric, and testing only the easy direction is how a
        /// half-broken path stays green.
        fn a_pointer_crosses_between_two_sessions_both_ways() {
            let guest_peer = peer_id(9);
            let host_peer = peer_id(11);
            let (mut host_tree, room) =
                host_naming(&[(&guest_peer, "bob"), (&host_peer, "ada")]);

            let guest_tree = guest_joining(&mut host_tree, room);

            let host_handle = host_tree.shared();
            let guest_handle = guest_tree.shared();

            let (host_net, mut host_cmds) =
                shell_network_for_test(host_handle.clone(), handle_of(&host_peer));
            let (guest_net, mut guest_cmds) =
                shell_network_for_test(guest_handle.clone(), handle_of(&guest_peer));
            let host_net = Arc::new(host_net);
            let guest_net = Arc::new(guest_net);

            let dir =
                std::env::temp_dir().join(format!("tilo-two-sessions-{}", std::process::id()));
            std::fs::create_dir_all(&dir).expect("a temp dir");
            let host_broker = broker(&dir, "h.sock");
            let guest_broker = broker(&dir, "g.sock");

            let host = PeerPresence::attach(&host_broker, &host_net, Vec::new());
            let guest = PeerPresence::attach(&guest_broker, &guest_net, Vec::new());
            host.set_rooms(rooms_of(&host_handle));
            guest.set_rooms(rooms_of(&guest_handle));

            let room_key = PresenceKey::Workspace {
                id: room.to_string(),
            };

            guest.set_pointer(Some(on_desk(420.0, 300.0)));
            assert!(
                guest.publish_local_cursor(&room.to_string()),
                "a session whose pointer has been somewhere must announce it",
            );
            over_the_wire(published(&mut guest_cmds), &host_net);

            let mut on_host = PresenceCtx::default();
            assert!(
                host.pump(&mut on_host, &DESK_TILES),
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

            host.set_pointer(Some(on_desk(880.0, 520.0)));
            assert!(host.publish_local_cursor(&room.to_string()));
            over_the_wire(published(&mut host_cmds), &guest_net);

            let mut on_guest = PresenceCtx::default();
            assert!(
                guest.pump(&mut on_guest, &DESK_TILES),
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

        /// A session that has not moved its pointer says nothing, and one
        /// that then moves it says something, over the real wire.
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

            let dir = std::env::temp_dir().join(format!("tilo-silent-{}", std::process::id()));
            std::fs::create_dir_all(&dir).expect("a temp dir");
            let host_broker = broker(&dir, "h.sock");
            let guest_broker = broker(&dir, "g.sock");
            let host = PeerPresence::attach(&host_broker, &host_net, Vec::new());
            let guest = PeerPresence::attach(&guest_broker, &guest_net, Vec::new());
            host.set_rooms(rooms_of(&host_handle));
            guest.set_rooms(rooms_of(&guest_handle));

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
                !host.pump(&mut on_host, &DESK_TILES),
                "so the host has nobody to paint"
            );
            assert!(on_host.pointers.is_empty());

            guest.set_pointer(Some(on_desk(640.0, 610.0)));
            assert!(guest.publish_local_cursor(&room.to_string()));
            over_the_wire(published(&mut guest_cmds), &host_net);
            assert!(host.pump(&mut on_host, &DESK_TILES));
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

        /// Where the previous test proves the gate, this proves it's
        /// reachable — takes the position from `ClientInput` exactly as
        /// `native.rs` does, rather than writing `None` by hand.
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

            let dir = std::env::temp_dir().join(format!("tilo-frameloop-{}", std::process::id()));
            std::fs::create_dir_all(&dir).expect("a temp dir");
            let host_broker = broker(&dir, "h.sock");
            let guest_broker = broker(&dir, "g.sock");
            let host = PeerPresence::attach(&host_broker, &host_net, Vec::new());
            let guest = PeerPresence::attach(&guest_broker, &guest_net, Vec::new());
            host.set_rooms(rooms_of(&host_handle));
            guest.set_rooms(rooms_of(&guest_handle));

            // No host pointer event has arrived. Optional position is the same
            // shared input datum delivered by either native adapter.
            guest.set_pointer(None);
            assert!(
                !guest.publish_local_cursor(&room.to_string()),
                "a session with no pointer has nothing to say about where one is",
            );
            assert!(
                guest_cmds.try_recv().is_err(),
                "and nothing may reach the wire for it",
            );

            let mut on_host = PresenceCtx::default();
            assert!(
                !host.pump(&mut on_host, &DESK_TILES),
                "so the host has nobody to paint"
            );
            assert!(
                !on_host.pointers.contains_key(&PresenceKey::Workspace {
                    id: room.to_string()
                }),
                "and no pill is drawn for a participant who never pointed at anything",
            );

            guest.set_pointer(Some(on_desk(640.0, 610.0)));
            assert!(guest.publish_local_cursor(&room.to_string()));
            over_the_wire(published(&mut guest_cmds), &host_net);
            assert!(host.pump(&mut on_host, &DESK_TILES));
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

        /// Asserted across two real sessions rather than against a closure.
        fn a_pointer_from_a_room_this_session_is_not_in_is_refused() {
            let stranger_peer = peer_id(33);
            let (host_tree, _room) = host_naming(&[(&stranger_peer, "Nobody")]);

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

            let dir = std::env::temp_dir().join(format!("tilo-stranger-{}", std::process::id()));
            std::fs::create_dir_all(&dir).expect("a temp dir");
            let host_broker = broker(&dir, "h.sock");
            let stranger_broker = broker(&dir, "s.sock");
            let host = PeerPresence::attach(&host_broker, &host_net, Vec::new());
            let stranger = PeerPresence::attach(&stranger_broker, &stranger_net, Vec::new());
            host.set_rooms(rooms_of(&host_handle));
            stranger.set_rooms(rooms_of(&stranger_handle));

            stranger.set_pointer(Some(on_desk(10.0, 10.0)));
            assert!(stranger.publish_local_cursor(&elsewhere.to_string()));
            over_the_wire(published(&mut stranger_cmds), &host_net);

            let mut on_host = PresenceCtx::default();
            assert!(
                !host.pump(&mut on_host, &DESK_TILES),
                "a room this session is not in is not this session's to hold",
            );
            assert!(
                on_host.pointers.is_empty(),
                "and nothing may be filed under it",
            );

            let _ = std::fs::remove_dir_all(&dir);
        }

        a_pointer_crosses_between_two_sessions_both_ways();
        a_session_is_silent_until_its_pointer_has_been_somewhere();
        the_frame_loop_publishes_nothing_for_a_pointer_that_has_never_moved();
        a_pointer_from_a_room_this_session_is_not_in_is_refused();
    }

    /// The key a session put on the wire, parsed back the way a receiver does.
    fn key_of(msg: &NeighbourMsg) -> PresenceKey {
        let NeighbourMsg::PresenceMoved { key, .. } = msg else {
            panic!("expected PresenceMoved, got {msg:?}");
        };
        serde_json::from_str(key).expect("a presence key")
    }

    /// A native session (broker attached) and a hosted one (the shell as a
    /// window, here with no broker) in one room. They used to publish
    /// different keys and each drop the other's, so neither saw the other;
    /// now both publish a tile-anchored pointer and both place it.
    #[test]
    fn hosted_and_native_publish_the_same_key_shape_and_see_each_other() {
        let hosted_peer = peer_id(31);
        let native_peer = peer_id(33);
        let (mut native_tree, room) =
            host_naming(&[(&hosted_peer, "bob"), (&native_peer, "ada")]);
        let hosted_tree = guest_joining(&mut native_tree, room);
        let native_handle = native_tree.shared();
        let hosted_handle = hosted_tree.shared();
        let (native_net, mut native_cmds) =
            shell_network_for_test(native_handle.clone(), handle_of(&native_peer));
        let (hosted_net, mut hosted_cmds) =
            shell_network_for_test(hosted_handle.clone(), handle_of(&hosted_peer));
        let native_net = Arc::new(native_net);
        let hosted_net = Arc::new(hosted_net);

        let dir = std::env::temp_dir().join(format!("tilo-hosted-native-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("a temp dir");
        let native_broker = broker(&dir, "n.sock");
        let native = PeerPresence::attach(&native_broker, &native_net, rooms_of(&native_handle));
        let hosted = PeerPresence::for_network(&hosted_net, rooms_of(&hosted_handle));
        let room_key = PresenceKey::Workspace {
            id: room.to_string(),
        };
        let tile_key = PresenceKey::tile_pointer(room.to_string(), DESK_TAB);

        hosted.set_pointer(Some(on_desk(420.0, 300.0)));
        assert!(hosted.publish_local_cursor(&room.to_string()));
        let from_hosted = published(&mut hosted_cmds);
        native.set_pointer(Some(on_desk(880.0, 520.0)));
        assert!(native.publish_local_cursor(&room.to_string()));
        let from_native = published(&mut native_cmds);
        assert_eq!(key_of(&from_hosted), tile_key, "the hosted shell's key");
        assert_eq!(
            key_of(&from_native),
            key_of(&from_hosted),
            "one key shape whichever way the shell runs",
        );

        over_the_wire(from_hosted, &native_net);
        let mut on_native = PresenceCtx::default();
        assert!(native.pump(&mut on_native, &DESK_TILES));
        let seen = on_native
            .pointers
            .get(&room_key)
            .expect("the native session places the hosted peer's pointer");
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].1, (420.0, 300.0));
        assert_eq!(on_native.display_name(&seen[0].0), "bob");

        over_the_wire(from_native, &hosted_net);
        let mut on_hosted = PresenceCtx::default();
        assert!(hosted.pump(&mut on_hosted, &DESK_TILES));
        let seen = on_hosted
            .pointers
            .get(&room_key)
            .expect("the hosted session places the native peer's pointer");
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].1, (880.0, 520.0));
        assert_eq!(on_hosted.display_name(&seen[0].0), "ada");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The hosted shell's frame reads its host through a [`SharedDesk`]:
    /// the pointer is anchored to the tile under it and published in the
    /// room on screen, nothing is published before it has moved, and a
    /// peer's pointer lands over this window's tile for its tab.
    #[test]
    fn a_hosted_frame_anchors_its_pointer_and_places_its_peers() {
        use vello::kurbo::Rect;
        let hosted_peer = peer_id(41);
        let other_peer = peer_id(43);
        let (mut other_tree, room) =
            host_naming(&[(&hosted_peer, "bob"), (&other_peer, "ada")]);
        let mut hosted_tree = guest_joining(&mut other_tree, room);
        looking_at_its_room(&mut hosted_tree);
        let hosted_handle = hosted_tree.shared();
        let (hosted_net, mut hosted_cmds) =
            shell_network_for_test(hosted_handle.clone(), handle_of(&hosted_peer));
        let hosted_net = Arc::new(hosted_net);
        let hosted = PeerPresence::for_network(&hosted_net, Vec::new());
        let shared = SharedDesk::default();
        shared.set_tiles(vec![(DESK_TAB, Rect::new(100.0, 50.0, 700.0, 650.0))]);
        let mut presence = PresenceCtx::default();

        hosted.frame(&mut presence, &shared.snapshot(), &hosted_handle);
        assert!(
            hosted_cmds.try_recv().is_err(),
            "a pointer that has not moved is not published at the origin",
        );

        shared.set_pointer(Some((130.0, 70.0)));
        hosted.frame(&mut presence, &shared.snapshot(), &hosted_handle);
        let NeighbourMsg::PresenceMoved { key, point, .. } = published(&mut hosted_cmds) else {
            panic!("expected PresenceMoved");
        };
        assert_eq!(
            serde_json::from_str::<PresenceKey>(&key).expect("a presence key"),
            PresenceKey::tile_pointer(room.to_string(), DESK_TAB),
            "published in the room on screen, anchored to the tile under it",
        );
        assert_eq!(point, (30.0, 20.0), "as an offset from that tile's corner");

        shared.set_pointer(None);
        hosted.frame(&mut presence, &shared.snapshot(), &hosted_handle);
        let NeighbourMsg::PresenceCleared { key: cleared, .. } = published(&mut hosted_cmds) else {
            panic!("expected PresenceCleared");
        };
        assert_eq!(cleared, key, "a pointer that left the window is taken away");
        hosted.frame(&mut presence, &shared.snapshot(), &hosted_handle);
        assert!(hosted_cmds.try_recv().is_err(), "and not taken away again",);

        hosted_net.handle_presence_message(
            handle_of(&other_peer),
            "anyone at all".into(),
            serde_json::to_string(&PresenceKey::tile_pointer(room.to_string(), DESK_TAB))
                .expect("a tile pointer key serialises"),
            (5.0, 6.0),
            0,
        );
        assert!(hosted.frame(&mut presence, &shared.snapshot(), &hosted_handle));
        let seen = presence
            .pointers
            .get(&PresenceKey::Workspace {
                id: room.to_string(),
            })
            .expect("the peer's pointer is placed over this window's tile");
        assert_eq!(seen[0].1, (105.0, 56.0));
        assert_eq!(
            presence.display_name(&seen[0].0),
            "ada",
            "named by the room, not by the frame",
        );
    }

    /// One strip of four windows in `room`, the same on every device —
    /// strips, groups and tabs are the room's — with `homes` saying which
    /// of this device's monitors shows which group.
    fn desk(room: RoomId, homes: &[(GroupId, &str)]) -> WorkspaceTree {
        use crate::shell::state::{
            AppKind, Strip, StripId, Tab, TileContent, WindowGroup, WindowWidth, Workspace,
            WorkspaceId,
        };
        let strip = StripId(77);
        let ws_id = WorkspaceId(7);
        let mut ws = Workspace::new(ws_id, "Desk");
        ws.set_room(room);
        ws.strips = vec![Strip {
            id: strip,
            name: "Main".into(),
            window_groups: DESK_GROUPS
                .iter()
                .map(|&id| WindowGroup {
                    id,
                    tabs: vec![Tab {
                        id: desk_tab(id),
                        title: format!("W{id}"),
                        tile: TileContent::Stub {
                            kind: AppKind::Other("Test".into()),
                            label: format!("W{id}"),
                        },
                    }],
                    active_tab: desk_tab(id),
                    width: WindowWidth::Fraction(250),
                    maximized: false,
                    focus_history: Vec::new(),
                })
                .collect(),
        }];
        let mut tree = WorkspaceTree::default();
        tree.desktop.workspaces = vec![ws];
        tree.desktop.active = ws_id;
        tree.desktop.active_strip.insert(ws_id, strip);
        tree.desktop.active_group.insert(strip, DESK_GROUPS[0]);
        for (group, monitor) in homes {
            tree.desktop.outputs.home.insert(*group, (*monitor).into());
        }
        tree
    }

    const DESK_GROUPS: [GroupId; 4] = [1001, 1002, 1003, 1004];

    fn desk_tab(group: GroupId) -> u64 {
        group.saturating_add(5000)
    }

    fn display(id: u32, monitor: &str, rect: (f64, f64, f64, f64)) -> OutputGeometry {
        let mut output = OutputGeometry::unscaled(rect.0, rect.1, rect.2, rect.3).with_id(id);
        output.monitor = monitor.into();
        output
    }

    /// A device laid out across `outputs`, settled the way the compositor's
    /// frames settle it.
    fn device(tree: WorkspaceTree, outputs: Vec<OutputGeometry>) -> FrameBuilder {
        let mut fb = FrameBuilder::with_tree(tree, (1600, 900));
        fb.set_output_geometry(outputs);
        for _ in 0..4 {
            let mut doc = automerge::AutoCommit::new();
            fb.pump_apps(&mut doc, &automerge::ROOT);
            fb.relayout();
            let _ = fb.paint();
        }
        fb
    }

    /// The tile `group` shows its active tab in, on this device.
    fn tile_of(fb: &FrameBuilder, group: GroupId) -> vello::kurbo::Rect {
        fb.host()
            .active_tile_rect(group)
            .expect("every group of the active strip has a tile")
    }

    #[test]
    fn a_pointer_over_a_window_lands_over_that_window_on_a_differently_arranged_desk() {
        let guest_peer = peer_id(41);
        let host_peer = peer_id(43);
        let (mut host_tree, room) = host_naming(&[(&guest_peer, "bob"), (&host_peer, "ada")]);
        let guest_tree = guest_joining(&mut host_tree, room);
        let host_handle = host_tree.shared();
        let guest_handle = guest_tree.shared();
        let (host_net, mut host_cmds) =
            shell_network_for_test(host_handle.clone(), handle_of(&host_peer));
        let (guest_net, _guest_cmds) =
            shell_network_for_test(guest_handle.clone(), handle_of(&guest_peer));
        let host_net = Arc::new(host_net);
        let guest_net = Arc::new(guest_net);
        let dir = std::env::temp_dir().join(format!("tilo-anchored-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("a temp dir");
        let host_broker = broker(&dir, "h.sock");
        let guest_broker = broker(&dir, "g.sock");
        let host = PeerPresence::attach(&host_broker, &host_net, Vec::new());
        let guest = PeerPresence::attach(&guest_broker, &guest_net, Vec::new());
        host.set_rooms(rooms_of(&host_handle));
        guest.set_rooms(rooms_of(&guest_handle));

        // The host works on one laptop panel; the guest has a panel and a
        // bigger monitor to its right, with the strip's last two windows
        // on the monitor. The same window sits somewhere else entirely on
        // each desk.
        let host_desk = device(
            desk(room, &[]),
            vec![display(0, "host-panel", (0.0, 0.0, 1600.0, 900.0))],
        );
        let guest_desk = device(
            desk(
                room,
                &[
                    (DESK_GROUPS[0], "guest-panel"),
                    (DESK_GROUPS[1], "guest-panel"),
                    (DESK_GROUPS[2], "guest-monitor"),
                    (DESK_GROUPS[3], "guest-monitor"),
                ],
            ),
            vec![
                display(0, "guest-panel", (0.0, 0.0, 1280.0, 800.0)),
                display(1, "guest-monitor", (1280.0, 0.0, 1920.0, 1080.0)),
            ],
        );
        let target = DESK_GROUPS[2];
        let on_host = tile_of(&host_desk, target);
        let on_guest = tile_of(&guest_desk, target);
        assert!(
            (on_host.x0 - on_guest.x0).abs() > 100.0,
            "the fixture only proves something if the window moved: {on_host:?} vs {on_guest:?}",
        );

        // The host points 37 px right of and 41 px below the window's
        // top-left corner.
        // Both sessions go through their frame builders, the way the
        // compositor's frame loop drives them.
        let mut host_desk = host_desk;
        let mut guest_desk = guest_desk;
        host_desk.set_peer_presence(host);
        guest_desk.set_peer_presence(guest);
        let over = (on_host.x0 + 37.0, on_host.y0 + 41.0);
        host_desk.note_local_pointer(Some(over));
        assert!(
            host_desk
                .peer_presence()
                .expect("attached above")
                .publish_local_cursor(&room.to_string())
        );
        over_the_wire(published(&mut host_cmds), &guest_net);

        assert!(guest_desk.pump_peer_presence());
        let seen = guest_desk
            .presence()
            .pointers
            .get(&PresenceKey::Workspace {
                id: room.to_string(),
            })
            .map(|p| p[0].1);
        #[allow(
            clippy::cast_possible_truncation,
            reason = "scene coordinates compared at f32, the precision presence stores"
        )]
        let expected = ((on_guest.x0 + 37.0) as f32, (on_guest.y0 + 41.0) as f32);
        assert_eq!(
            seen,
            Some(expected),
            "the guest must see the host's pointer over the same spot of the same window, \
             wherever its own monitors put that window",
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Put the fixture's workspace on screen — `append_empty_workspace`
    /// doesn't move `active`, but the roster reads whichever room that is.
    fn looking_at_its_room(tree: &mut WorkspaceTree) {
        tree.desktop.active = tree
            .desktop
            .workspaces
            .first()
            .expect("the fixture makes a workspace")
            .id;
    }

    /// A session's own view of who is in the room it is looking at.
    fn roster_of(
        tree: &crate::shell::state::WorkspaceTreeHandle,
    ) -> Vec<(String, Option<String>, bool)> {
        tree.read()
            .desktop
            .active_workspace()
            .map(|workspace| workspace.collaborators.as_slice())
            .unwrap_or_default()
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
        let relay = PeerPresence::attach(&broker(dir, sock), &net, Vec::new());
        relay.set_rooms(rooms_of(tree));
        (relay, net, cmds)
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn two_session_roster_lists_named_invited_and_arrived_peers_but_not_the_reader() {
        /// `refresh_roster` must actually write `WorkspaceTree::collaborators`.
        fn a_peer_who_joined_is_in_the_roster() {
            let guest_peer = peer_id(21);
            let host_peer = peer_id(22);
            let (mut host_tree, room) = host_naming(&[(&guest_peer, "bob")]);
            let mut guest_tree = guest_joining(&mut host_tree, room);
            looking_at_its_room(&mut host_tree);
            looking_at_its_room(&mut guest_tree);
            let host_handle = host_tree.shared();
            let guest_handle = guest_tree.shared();

            let dir = std::env::temp_dir().join(format!("tilo-roster-join-{}", std::process::id()));
            std::fs::create_dir_all(&dir).expect("a temp dir");
            let (host, host_net, _host_cmds) = session(&dir, "h.sock", &host_handle, &host_peer);
            let (guest, _guest_net, mut guest_cmds) =
                session(&dir, "g.sock", &guest_handle, &guest_peer);

            assert!(
                roster_of(&host_handle).is_empty(),
                "before anyone arrives the room has nobody in it",
            );

            guest.set_pointer(Some(on_desk(420.0, 300.0)));
            assert!(guest.publish_local_cursor(&room.to_string()));
            over_the_wire(published(&mut guest_cmds), &host_net);

            let mut on_host = PresenceCtx::default();
            assert!(
                host.pump(&mut on_host, &DESK_TILES),
                "the frame folds the guest in"
            );
            assert!(
                host.refresh_roster(&on_host),
                "and the room's people changed, which is a reason to repaint",
            );

            assert_eq!(
                roster_of(&host_handle),
                vec![("bob".to_string(), None, false)],
                "the host's roster must name the person who joined it, as \
                 somebody who is here rather than somebody who was asked",
            );
            assert_eq!(
                host_handle
                    .read()
                    .desktop
                    .active_workspace()
                    .unwrap()
                    .collaborators[0]
                    .session,
                Some(handle_of(&guest_peer)),
                "trusted screen-sharing disclosure joins viewers through the authenticated session",
            );

            assert!(
                !host.refresh_roster(&on_host),
                "restating the same room's people is not a change",
            );

            let _ = std::fs::remove_dir_all(&dir);
        }

        /// A plain invite says nothing about who its token is for, so the peer
        /// who redeems one has no name in the room and is listed as its id.
        fn a_peer_the_room_has_not_named_is_listed_as_its_id() {
            let guest_peer = peer_id(31);
            let host_peer = peer_id(32);
            let (mut host_tree, room) = host_naming(&[]);
            let mut guest_tree = guest_joining(&mut host_tree, room);
            looking_at_its_room(&mut host_tree);
            looking_at_its_room(&mut guest_tree);
            let host_handle = host_tree.shared();
            let guest_handle = guest_tree.shared();

            let dir =
                std::env::temp_dir().join(format!("tilo-roster-unnamed-{}", std::process::id()));
            std::fs::create_dir_all(&dir).expect("a temp dir");
            let (host, host_net, _host_cmds) = session(&dir, "h.sock", &host_handle, &host_peer);
            let (guest, _guest_net, mut guest_cmds) =
                session(&dir, "g.sock", &guest_handle, &guest_peer);

            guest.set_pointer(Some(on_desk(10.0, 20.0)));
            assert!(guest.publish_local_cursor(&room.to_string()));
            over_the_wire(published(&mut guest_cmds), &host_net);

            let mut on_host = PresenceCtx::default();
            host.pump(&mut on_host, &DESK_TILES);
            host.refresh_roster(&on_host);

            let expected = format!("peer@{}", handle_of(&guest_peer));
            assert_eq!(
                roster_of(&host_handle),
                vec![(expected, None, false)],
                "an unnamed peer keeps its id as its label",
            );

            let _ = std::fs::remove_dir_all(&dir);
        }

        /// A host names itself when it mints an invite; the roster must not
        /// read that back as a peer of its own.
        fn the_room_does_not_list_the_session_reading_it() {
            let guest_peer = peer_id(41);
            let host_peer = peer_id(42);
            let (mut host_tree, room) = host_naming(&[(&guest_peer, "bob"), (&host_peer, OWNER)]);
            let mut guest_tree = guest_joining(&mut host_tree, room);
            looking_at_its_room(&mut host_tree);
            looking_at_its_room(&mut guest_tree);
            let host_handle = host_tree.shared();
            let guest_handle = guest_tree.shared();

            let dir = std::env::temp_dir().join(format!("tilo-roster-self-{}", std::process::id()));
            std::fs::create_dir_all(&dir).expect("a temp dir");
            let (host, host_net, _host_cmds) = session(&dir, "h.sock", &host_handle, &host_peer);
            let (guest, _guest_net, mut guest_cmds) =
                session(&dir, "g.sock", &guest_handle, &guest_peer);

            guest.set_pointer(Some(on_desk(1.0, 2.0)));
            assert!(guest.publish_local_cursor(&room.to_string()));
            over_the_wire(published(&mut guest_cmds), &host_net);
            let mut on_host = PresenceCtx::default();
            host.pump(&mut on_host, &DESK_TILES);
            host.refresh_roster(&on_host);

            let listed: Vec<String> = roster_of(&host_handle)
                .into_iter()
                .map(|(n, ..)| n)
                .collect();
            assert_eq!(
                listed,
                vec!["bob".to_string()],
                "the roster is who else is here, and the host is not else",
            );

            let _ = std::fs::remove_dir_all(&dir);
        }

        /// Nothing crosses a wire here — only the room document's naming.
        fn a_named_peer_who_has_not_arrived_reads_as_invited() {
            let host_peer = peer_id(51);
            let guest_peer = peer_id(52);
            let (mut host_tree, room) = host_naming(&[(&host_peer, OWNER)]);
            let mut guest_tree = guest_joining(&mut host_tree, room);
            looking_at_its_room(&mut guest_tree);
            let guest_handle = guest_tree.shared();

            let dir =
                std::env::temp_dir().join(format!("tilo-roster-guest-{}", std::process::id()));
            std::fs::create_dir_all(&dir).expect("a temp dir");
            let (guest, _guest_net, _guest_cmds) =
                session(&dir, "g.sock", &guest_handle, &guest_peer);

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

            assert_eq!(
                crate::shell::state::active_workspace_peer_count(&guest_handle.read()),
                0,
                "an invitation nobody has redeemed is not a peer in the room",
            );

            let _ = std::fs::remove_dir_all(&dir);
        }

        /// An invitation becomes a participant without a pointer — joining
        /// isn't moving a mouse. The host observes the guest asking the
        /// room's document for its changes, under its authenticated identity.
        fn arriving_turns_an_invitation_into_a_participant() {
            let guest_peer = peer_id(61);
            let host_peer = peer_id(62);
            let (mut host_tree, room) = host_naming(&[(&guest_peer, "bob")]);
            looking_at_its_room(&mut host_tree);
            let host_handle = host_tree.shared();

            let dir = std::env::temp_dir().join(format!("tilo-roster-beat-{}", std::process::id()));
            std::fs::create_dir_all(&dir).expect("a temp dir");
            let (host, host_net, _host_cmds) = session(&dir, "h.sock", &host_handle, &host_peer);

            let quiet = PresenceCtx::default();
            assert!(host.refresh_roster(&quiet));
            assert_eq!(
                roster_of(&host_handle),
                vec![("bob".to_string(), None, true)],
                "before he redeems anything, ada has invited him and no more",
            );
            assert_eq!(
                crate::shell::state::active_workspace_peer_count(&host_handle.read()),
                0,
                "so the top bar still offers to invite the first person",
            );

            // Redeems it: the same request `accept_invite` sends.
            {
                let mut replica = host_net.replica.lock().expect("the replica lock");
                replica.handle_from(
                    &crate::shell::state::PeerOrigin::peer_id(&guest_peer),
                    &NeighbourMsg::SyncReq {
                        since: serde_json::to_vec(&Vec::<automerge::ChangeHash>::new())
                            .expect("empty heads serialise"),
                        room: room.to_string(),
                        invite_id: None,
                        nonce: 0,
                    },
                );
            }

            assert!(
                host.refresh_roster(&quiet),
                "the room's people changed, which is what makes the frame repaint",
            );
            assert_eq!(
                roster_of(&host_handle),
                vec![("bob".to_string(), None, false)],
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

        a_peer_who_joined_is_in_the_roster();
        a_peer_the_room_has_not_named_is_listed_as_its_id();
        the_room_does_not_list_the_session_reading_it();
        a_named_peer_who_has_not_arrived_reads_as_invited();
        arriving_turns_an_invitation_into_a_participant();
    }

    /// "Nothing" means not appearing: someone who shows nothing in a room
    /// says so on its typed route, and is off the other people's roster
    /// there even though their machine keeps taking part in the room's
    /// documents; being available again brings them back. The word is
    /// only taken for the room it was said in.
    #[test]
    fn someone_who_shows_nothing_is_not_listed_as_here() {
        let guest_peer = peer_id(63);
        let host_peer = peer_id(64);
        let (mut host_tree, room) = host_naming(&[(&guest_peer, "bob")]);
        looking_at_its_room(&mut host_tree);
        let host_handle = host_tree.shared();
        let dir = std::env::temp_dir().join(format!("tilo-roster-absent-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("a temp dir");
        let (host, host_net, _host_cmds) = session(&dir, "h.sock", &host_handle, &host_peer);
        {
            let mut replica = host_net.replica.lock().expect("the replica lock");
            replica.handle_from(
                &crate::shell::state::PeerOrigin::peer_id(&guest_peer),
                &NeighbourMsg::SyncReq {
                    since: serde_json::to_vec(&Vec::<automerge::ChangeHash>::new())
                        .expect("empty heads serialise"),
                    room: room.to_string(),
                    invite_id: None,
                    nonce: 0,
                },
            );
        }
        let mut presence = PresenceCtx::default();
        host.pump(&mut presence, &DESK_TILES);
        host.refresh_roster(&presence);
        assert_eq!(
            roster_of(&host_handle),
            vec![("bob".to_string(), None, false)],
            "taking part in the room's documents is being here"
        );

        let guest = format!("peer@{}", handle_of(&guest_peer));
        let key = PresenceKey::Workspace {
            id: room.to_string(),
        };
        assert!(
            typed_presence_authorized(&key, &PresencePayload::Left, &room.to_string()),
            "a room's people may be told someone is not here"
        );
        assert!(
            !typed_presence_authorized(&key, &PresencePayload::Left, &RoomId::new().to_string()),
            "only in the room it names"
        );
        host.typed_inbound.lock().expect("the typed queue").push((
            guest.clone(),
            key.clone(),
            PresencePayload::Left,
        ));
        assert!(host.pump(&mut presence, &DESK_TILES));
        host.refresh_roster(&presence);
        assert_eq!(
            roster_of(&host_handle),
            vec![("bob".to_string(), None, true)],
            "someone who shows nothing is not listed as here, whatever their machine syncs"
        );
        assert_eq!(
            crate::shell::state::active_workspace_peer_count(&host_handle.read()),
            0,
            "and the bar shows nobody"
        );

        host.typed_inbound.lock().expect("the typed queue").push((
            guest,
            key,
            PresencePayload::Availability {
                state: ui::Availability::Available,
                call: None,
            },
        ));
        assert!(host.pump(&mut presence, &DESK_TILES));
        host.refresh_roster(&presence);
        assert_eq!(
            roster_of(&host_handle),
            vec![("bob".to_string(), None, false)],
            "showing themselves again brings them back"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// How long [`while_the_replica_is_held`] holds the lock for frames
    /// that should not need it: far longer than a hundred frames take.
    const PATIENCE: std::time::Duration = std::time::Duration::from_secs(5);

    /// Run `frames` while another thread holds `net`'s replica lock.
    /// `None` when they did not finish before that thread let go after
    /// `patience`: they reached for the replica.
    fn while_the_replica_is_held<T>(
        net: &ShellNetwork,
        patience: std::time::Duration,
        frames: impl FnOnce() -> T,
    ) -> Option<T> {
        use std::sync::atomic::AtomicBool;
        let (locked, is_locked) = std::sync::mpsc::channel();
        let (done, finished) = std::sync::mpsc::channel::<()>();
        let released = AtomicBool::new(false);
        let released_flag = &released;
        std::thread::scope(|scope| {
            scope.spawn(move || {
                let held = net.replica.lock().expect("the replica lock");
                let _ = locked.send(());
                let _ = finished.recv_timeout(patience);
                released_flag.store(true, Ordering::SeqCst);
                drop(held);
            });
            is_locked
                .recv()
                .expect("the other thread holds the replica");
            let answer = frames();
            // Read before letting go: once told, the other thread releases.
            let held_throughout = !released.load(Ordering::SeqCst);
            let _ = done.send(());
            held_throughout.then_some(answer)
        })
    }

    /// `wakeups` frames of the presence pump and roster refresh, as
    /// [`FrameBuilder::pump_peer_presence`] runs them, while the replica is
    /// held elsewhere. Whether any frame said something changed, or `None`
    /// when they reached for the replica.
    fn frames_without_the_replica(
        relay: &PeerPresence,
        net: &ShellNetwork,
        presence: &mut PresenceCtx,
        wakeups: usize,
    ) -> Option<bool> {
        while_the_replica_is_held(net, PATIENCE, || {
            let mut changed = false;
            for _ in 0..wakeups {
                changed |= relay.pump(presence, &DESK_TILES);
                changed |= relay.refresh_roster(presence);
            }
            changed
        })
    }

    #[test]
    fn the_roster_is_derived_again_only_after_one_of_its_inputs_changed() {
        let guest_peer = peer_id(71);
        let host_peer = peer_id(72);
        let (mut host_tree, room) = host_naming(&[(&guest_peer, "bob")]);
        let mut guest_tree = guest_joining(&mut host_tree, room);
        looking_at_its_room(&mut host_tree);
        looking_at_its_room(&mut guest_tree);
        let host_handle = host_tree.shared();
        let guest_handle = guest_tree.shared();

        let dir = std::env::temp_dir().join(format!("tilo-roster-gated-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("a temp dir");
        let (host, host_net, _host_cmds) = session(&dir, "h.sock", &host_handle, &host_peer);
        let (guest, _guest_net, mut guest_cmds) =
            session(&dir, "g.sock", &guest_handle, &guest_peer);
        let mut on_host = PresenceCtx::default();

        // Phase `the_first_frame_derives_it`.
        assert!(host.pump(&mut on_host, &DESK_TILES) | host.refresh_roster(&on_host));
        assert_eq!(
            roster_of(&host_handle),
            vec![("bob".to_string(), None, true)],
            "the room's named invitee is listed before anyone arrives",
        );

        // Phase `idle_frames_lock_nothing`. Its own write of the roster is
        // not a reason to derive it again, and nothing else moved: a
        // hundred frames finish while the replica is held elsewhere.
        assert_eq!(
            frames_without_the_replica(&host, &host_net, &mut on_host, 100),
            Some(false),
            "an idle frame must not take the replica lock or report a change",
        );

        // Phase `an_arrival_is_listed_on_the_next_frame`. Joining without a
        // pointer — the request `accept_invite` sends.
        {
            let mut replica = host_net.replica.lock().expect("the replica lock");
            replica.handle_from(
                &crate::shell::state::PeerOrigin::peer_id(&guest_peer),
                &NeighbourMsg::SyncReq {
                    since: serde_json::to_vec(&Vec::<automerge::ChangeHash>::new())
                        .expect("empty heads serialise"),
                    room: room.to_string(),
                    invite_id: None,
                    nonce: 0,
                },
            );
        }
        assert!(
            host.refresh_roster(&on_host),
            "a silent joiner reaches the roster on the very next frame",
        );
        assert_eq!(
            roster_of(&host_handle),
            vec![("bob".to_string(), None, false)],
        );
        assert_eq!(
            frames_without_the_replica(&host, &host_net, &mut on_host, 100),
            Some(false),
            "and once listed, idle frames are free again",
        );

        // Phase `a_moving_pointer_does_not_rederive_it`. The guest's pointer
        // lands (the fold changes presence), then moves; who is here does
        // not change, so the roster is not derived again for the move.
        let start = Instant::now();
        guest.set_pointer(Some(on_desk(420.0, 300.0)));
        assert!(guest.publish_local_cursor_at(&room.to_string(), start));
        over_the_wire(published(&mut guest_cmds), &host_net);
        assert!(host.pump(&mut on_host, &DESK_TILES), "the pointer lands");
        host.refresh_roster(&on_host);
        guest.set_pointer(Some(on_desk(500.0, 310.0)));
        assert!(guest.publish_local_cursor_at(&room.to_string(), start + MIN_PUBLISH_GAP));
        over_the_wire(published(&mut guest_cmds), &host_net);
        let roster_before = roster_of(&host_handle);
        assert!(host.pump(&mut on_host, &DESK_TILES), "the pointer moves");
        assert_eq!(
            while_the_replica_is_held(&host_net, PATIENCE, || host.refresh_roster(&on_host)),
            Some(false),
            "a pointer moving is told apart from somebody arriving without the replica",
        );
        assert_eq!(roster_of(&host_handle), roster_before);

        // Phase `a_tree_write_rederives_it`. Renaming the guest in the room
        // is a tree write; the next frame lists the new name.
        assert!(
            host_handle
                .write()
                .name_peer(room, &OWNER.to_string(), &guest_peer, "bob E.")
        );
        assert!(host.refresh_roster(&on_host));
        assert_eq!(
            roster_of(&host_handle),
            vec![("bob E.".to_string(), None, false)],
        );

        // Phase `a_missed_signal_is_late_not_lost`. The roster is derived
        // again after `ROSTER_RECHECK` whatever the signals say: that frame
        // reaches for the replica, and so waits for it.
        let later = Instant::now() + ROSTER_RECHECK;
        assert_eq!(
            while_the_replica_is_held(&host_net, std::time::Duration::from_millis(300), || {
                host.refresh_roster_at(&on_host, later)
            }),
            None,
            "a frame past the recheck derives the roster again",
        );
        assert!(
            !host.refresh_roster_at(&on_host, later),
            "and finds the same people",
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Two devices the room names alike (two people's "Primary device") are
    /// still two people: where each is comes from its own presence, so
    /// following one goes where that one is.
    #[test]
    fn two_peers_named_alike_are_each_where_their_own_presence_says() {
        let (first, second) = (peer_id(61), peer_id(62));
        let (mut host_tree, room) =
            host_naming(&[(&first, "Primary device"), (&second, "Primary device")]);
        looking_at_its_room(&mut host_tree);
        let host_handle = host_tree.shared();
        let dir = std::env::temp_dir().join(format!("tilo-roster-alike-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("a temp dir");
        let (host, _host_net, _host_cmds) = session(&dir, "a.sock", &host_handle, &peer_id(60));
        let id = |peer: &str| ui::permissions::UserId(format!("peer@{}", handle_of(peer)));
        let mut presence = PresenceCtx::default();
        for peer in [&first, &second] {
            presence.names.insert(id(peer), "Primary device".into());
        }
        presence
            .here
            .insert(PresenceKey::Tab { id: "a".into() }, vec![id(&second)]);
        presence.pointers.insert(
            PresenceKey::Workspace {
                id: room.to_string(),
            },
            vec![(id(&first), (0.0, 0.0)), (id(&second), (0.0, 0.0))],
        );
        assert!(host.refresh_roster(&presence));
        let where_ = |peer: &str| {
            host_handle
                .read()
                .desktop
                .active_workspace()
                .and_then(|workspace| {
                    workspace
                        .collaborators
                        .iter()
                        .find(|row| row.id == id(peer).0)
                        .map(|row| row.active_tab)
                })
        };
        assert_eq!(where_(&second), Some(Some(0xa)), "the one in the window");
        assert_eq!(where_(&first), Some(None), "the other, in none");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// What an idle frame cost the roster before it was gated (deriving it
    /// every frame) against what it costs now, in a room naming 30 people.
    /// Printed for the record; asserted only in the direction that cannot
    /// flake, since one side takes the replica and parses 30 endpoint ids
    /// and the other loads three counters.
    #[test]
    fn an_idle_frame_costs_the_roster_a_fraction_of_deriving_it() {
        const FRAMES: u32 = 2_000;
        let people = (0..30_u8)
            .map(|seed| (peer_id(100 + seed), format!("Person {seed}")))
            .collect::<Vec<_>>();
        let named = people
            .iter()
            .map(|(peer, name)| (peer.as_str(), name.as_str()))
            .collect::<Vec<_>>();
        let (mut host_tree, _room) = host_naming(&named);
        looking_at_its_room(&mut host_tree);
        let host_handle = host_tree.shared();
        let dir = std::env::temp_dir().join(format!("tilo-roster-cost-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("a temp dir");
        let (host, _host_net, _host_cmds) = session(&dir, "h.sock", &host_handle, &peer_id(99));
        let quiet = PresenceCtx::default();
        assert!(host.refresh_roster(&quiet));
        assert_eq!(roster_of(&host_handle).len(), 30);

        let started = Instant::now();
        for _ in 0..FRAMES {
            let (changed, _, _) = host
                .derive_roster(&quiet, host.roster_tree.generation())
                .expect("the replica");
            assert!(!changed);
        }
        let derived = started.elapsed();
        let started = Instant::now();
        for _ in 0..FRAMES {
            assert!(!host.refresh_roster(&quiet));
        }
        let gated = started.elapsed();
        eprintln!(
            "#3805 roster per idle frame, 30 named peers: derived every frame {:?}, gated {:?}",
            derived / FRAMES,
            gated / FRAMES,
        );
        assert!(gated < derived, "gated {gated:?} vs derived {derived:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn peer_colours_are_applied_when_one_changes_not_every_frame() {
        let guest_peer = peer_id(81);
        let host_peer = peer_id(82);
        let (mut host_tree, room) = host_naming(&[(&guest_peer, "bob")]);
        let mut guest_tree = guest_joining(&mut host_tree, room);
        looking_at_its_room(&mut host_tree);
        looking_at_its_room(&mut guest_tree);
        let host_handle = host_tree.shared();
        let guest_handle = guest_tree.shared();

        let dir = std::env::temp_dir().join(format!("tilo-colours-gated-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("a temp dir");
        let (host, host_net, _host_cmds) = session(&dir, "h.sock", &host_handle, &host_peer);
        let (guest, _guest_net, mut guest_cmds) =
            session(&dir, "g.sock", &guest_handle, &guest_peer);
        let guest = {
            guest.set_pointer(Some(on_desk(1.0, 2.0)));
            assert!(guest.publish_local_cursor(&room.to_string()));
            guest
        };
        over_the_wire(published(&mut guest_cmds), &host_net);
        let user = ui::permissions::UserId(format!("peer@{}", handle_of(&guest_peer)));
        let worn = host_net
            .peer_colour(handle_of(&guest_peer))
            .expect("the guest's frame says what colour they wear");

        let mut on_host = PresenceCtx::default();
        host.pump(&mut on_host, &DESK_TILES);
        assert_eq!(
            on_host.color_overrides.get(&user).copied(),
            Some(ui::presence::identity_rgb(worn)),
            "the first frame paints the guest in their colour",
        );

        // Nothing restates it: a frame with no colour news leaves presence
        // as it is, which is what not walking every peer's colour looks like.
        on_host.color_overrides.remove(&user);
        for _ in 0..10 {
            host.pump(&mut on_host, &DESK_TILES);
        }
        assert_eq!(on_host.color_overrides.get(&user), None);

        let other = if worn == ui::tokens::PresenceHue::Violet {
            ui::tokens::PresenceHue::Teal
        } else {
            ui::tokens::PresenceHue::Violet
        };
        host_net.note_peer_colour(handle_of(&guest_peer), Some(other.id()));
        host.pump(&mut on_host, &DESK_TILES);
        assert_eq!(
            on_host.color_overrides.get(&user).copied(),
            Some(ui::presence::identity_rgb(other)),
            "a new colour is painted on the next frame",
        );
        drop(guest);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Two sessions in one room, each with its broker and network, the
    /// guest's pointer already on the host's screen: what the next tests
    /// build on.
    struct Pair {
        dir: std::path::PathBuf,
        room: RoomId,
        guest_peer: String,
        host_net: Arc<ShellNetwork>,
        guest_net: Arc<ShellNetwork>,
        host: PeerPresence,
        guest: PeerPresence,
        guest_cmds: tokio::sync::mpsc::Receiver<NetCmd>,
    }

    fn pair(tag: &str) -> Pair {
        let guest_peer = peer_id(41);
        let host_peer = peer_id(43);
        let (mut host_tree, room) = host_naming(&[(&guest_peer, "Ada"), (&host_peer, "ada")]);
        let guest_tree = guest_joining(&mut host_tree, room);
        let host_handle = host_tree.shared();
        let guest_handle = guest_tree.shared();
        let (host_net, _host_cmds) =
            shell_network_for_test(host_handle.clone(), handle_of(&host_peer));
        let (guest_net, guest_cmds) =
            shell_network_for_test(guest_handle.clone(), handle_of(&guest_peer));
        let host_net = Arc::new(host_net);
        let guest_net = Arc::new(guest_net);
        let dir = std::env::temp_dir().join(format!("tilo-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("a temp dir");
        let host = PeerPresence::attach(&broker(&dir, "h.sock"), &host_net, Vec::new());
        let guest = PeerPresence::attach(&broker(&dir, "g.sock"), &guest_net, Vec::new());
        host.set_rooms(rooms_of(&host_handle));
        guest.set_rooms(rooms_of(&guest_handle));
        Pair {
            dir,
            room,
            guest_peer,
            host_net,
            guest_net,
            host,
            guest,
            guest_cmds,
        }
    }

    impl Pair {
        /// The guest's endpoint key, as the live plane stamps its frames.
        fn guest_key(&self) -> [u8; 32] {
            *self
                .guest_peer
                .parse::<iroh::EndpointId>()
                .expect("an endpoint id")
                .as_bytes()
        }

        fn guest_user(&self) -> ui::permissions::UserId {
            ui::permissions::UserId(format!("peer@{}", handle_of(&self.guest_peer)))
        }
    }

    /// Cursor chat between two sessions: what the guest types beside its
    /// pointer reaches the host's screen live, beside that pointer, under
    /// the guest's name; it keeps the guest's resting pointer up while it
    /// shows; it lingers after Enter and goes; a lone emoji floats up once.
    #[test]
    #[allow(clippy::too_many_lines, reason = "one scenario read top to bottom")]
    fn cursor_chat_crosses_between_two_sessions_beside_the_pointer() {
        use crate::shell::live_presence::{Outbound, participant};
        use tilo_relay_protocol::live::{CursorChat, LiveRecord};
        let mut pair = pair("cursor-chat");
        let room = pair.room.to_string();
        let t0 = Instant::now();

        pair.guest.set_pointer(Some(on_desk(300.0, 200.0)));
        assert!(pair.guest.publish_local_cursor_at(&room, t0));
        over_the_wire(published(&mut pair.guest_cmds), &pair.host_net);
        let mut on_host = PresenceCtx::default();
        pair.host.pump_at(&mut on_host, &DESK_TILES, t0);
        let ada = pair.guest_user();
        let key = PresenceKey::Workspace { id: room };
        assert!(
            on_host.pointer_for(&key, &ada).is_some(),
            "the guest's pointer is here"
        );

        // The guest types; each change is a live frame of the room.
        let window = LiveRecord {
            app: "so.tilo.notes".into(),
            record: "note-1".into(),
        };
        let mut live = Outbound::default();
        let mut say = |text: &str, sent: bool| {
            live.say(
                7,
                (window.clone(), [300.0, 200.0]),
                participant("Ada".into(), ui::tokens::PresenceHue::Teal),
                Some(CursorChat {
                    text: text.into(),
                    sent,
                }),
                t0,
            )
            .expect("a frame")
        };
        pair.host_net
            .take_live_frame(pair.room, pair.guest_key(), &say("look at the sec", false));
        assert!(pair.host.pump_at(&mut on_host, &DESK_TILES, t0));
        // A moment later, once it has faded in.
        let typing = on_host
            .chats
            .bubble(
                &ada,
                t0 + Duration::from_secs(1),
                ui::tokens::EffectiveMotion::new(ui::tokens::Motion::default(), false),
            )
            .expect("the host shows what the guest is typing");
        assert_eq!(
            (typing.text.as_str(), typing.typing),
            ("look at the sec", true)
        );

        // Typing holds the pointer still for longer than a quiet pointer
        // lives: talking keeps it, and its name, on screen.
        let later = t0 + ui::presence::POINTER_IDLE + Duration::from_secs(1);
        pair.host_net.take_live_frame(
            pair.room,
            pair.guest_key(),
            &say("look at the second paragraph", true),
        );
        pair.host.pump_at(&mut on_host, &DESK_TILES, later);
        assert!(
            on_host.pointer_for(&key, &ada).is_some(),
            "a talking pointer is not retired for resting"
        );
        assert!(
            (on_host.pointer_name_opacity(&key, &ada, later, ui::presence::POINTER_FADE) - 1.0)
                .abs()
                < f32::EPSILON,
            "and its name stays while they talk"
        );
        let sent = on_host
            .chats
            .bubble(
                &ada,
                later,
                ui::tokens::EffectiveMotion::new(ui::tokens::Motion::default(), false),
            )
            .expect("a sent message stays a moment");
        assert!(!sent.typing);

        // After its linger it is gone.
        pair.host.pump_at(
            &mut on_host,
            &DESK_TILES,
            later + ui::presence::CURSOR_CHAT_LINGER,
        );
        assert!(
            on_host
                .chats
                .bubble(
                    &ada,
                    later + ui::presence::CURSOR_CHAT_LINGER,
                    ui::tokens::EffectiveMotion::new(ui::tokens::Motion::default(), false)
                )
                .is_none()
        );

        // A lone emoji is a reaction, played once.
        let reaction = live
            .react(
                7,
                (window, [300.0, 200.0]),
                participant("Ada".into(), ui::tokens::PresenceHue::Teal),
                "\u{1f389}".into(),
                later,
            )
            .expect("a frame");
        pair.host_net
            .take_live_frame(pair.room, pair.guest_key(), &reaction);
        pair.host.pump_at(&mut on_host, &DESK_TILES, later);
        assert!(
            on_host
                .chats
                .reaction(
                    &ada,
                    later,
                    ui::tokens::EffectiveMotion::new(ui::tokens::Motion::default(), false)
                )
                .is_some_and(|float| float.emoji == "\u{1f389}")
        );
        pair.host_net
            .take_live_frame(pair.room, pair.guest_key(), &reaction);
        assert!(
            !on_host.chats.react(&ada, 1, "\u{1f389}", later),
            "a restated reaction does not play again"
        );
        let _ = std::fs::remove_dir_all(&pair.dir);
    }

    /// Cursor chat in a room with no workspace host, so no live plane: what
    /// the guest types beside their pointer goes over the room's presence
    /// channel and shows on the host's screen beside that pointer, as it
    /// does over a live plane; a lone emoji floats up once.
    #[test]
    fn cursor_chat_crosses_a_room_with_no_live_plane_over_its_presence_channel() {
        use tilo_relay_protocol::live::CursorChat;
        let mut pair = pair("cursor-chat-p2p");
        let room = pair.room.to_string();
        let t0 = Instant::now();
        pair.guest.set_pointer(Some(on_desk(300.0, 200.0)));
        assert!(pair.guest.publish_local_cursor_at(&room, t0));
        over_the_wire(published(&mut pair.guest_cmds), &pair.host_net);
        let mut on_host = PresenceCtx::default();
        pair.host.pump_at(&mut on_host, &DESK_TILES, t0);
        let ada = pair.guest_user();
        let motion = ui::tokens::EffectiveMotion::new(ui::tokens::Motion::default(), false);
        let deliver = |pair: &mut Pair| match published(&mut pair.guest_cmds) {
            NeighbourMsg::AppPresenceV2 {
                peer,
                app_id,
                cluster_id,
                presence,
                ..
            } => pair
                .host_net
                .handle_typed_app_presence_message(peer, None, app_id, cluster_id, presence),
            other => panic!("expected room presence, got {other:?}"),
        };

        assert!(
            pair.guest_net.publish_cursor_chat(
                (&room, "so.tilo.notes", "note-1"),
                (10.0, 10.0),
                Some(CursorChat {
                    text: "look at the sec".into(),
                    sent: false,
                }),
            ),
            "a room with no live plane still carries what is said"
        );
        deliver(&mut pair);
        assert!(pair.host.pump_at(&mut on_host, &DESK_TILES, t0));
        let typing = on_host
            .chats
            .bubble(&ada, t0 + Duration::from_secs(1), motion)
            .expect("the host shows what the guest is typing");
        assert_eq!(
            (typing.text.as_str(), typing.typing),
            ("look at the sec", true)
        );

        assert!(pair.guest_net.publish_reaction(
            (&room, "so.tilo.notes", "note-1"),
            (10.0, 10.0),
            "\u{1f389}".into(),
        ));
        deliver(&mut pair);
        pair.host.pump_at(&mut on_host, &DESK_TILES, t0);
        assert!(
            on_host
                .chats
                .reaction(&ada, t0, motion)
                .is_some_and(|float| float.emoji == "\u{1f389}"),
            "and a lone emoji floats up from the pointer"
        );

        assert!(
            !pair.guest_net.publish_cursor_chat(
                (&room, "so.tilo.notes", "note-1"),
                (10.0, 10.0),
                Some(CursorChat {
                    text: "x"
                        .repeat(tilo_relay_protocol::live::MAX_CURSOR_CHAT_CHARS.saturating_add(1)),
                    sent: true,
                }),
            ),
            "a message past the bounds is refused on this route too"
        );
        let _ = std::fs::remove_dir_all(&pair.dir);
    }

    /// The voice channel someone is in (its id and name) says where they
    /// are: "That I'm here" sends how available they are but not their
    /// call; "Where I am" sends both.
    #[test]
    fn a_call_reaches_only_those_who_may_see_where_you_are() {
        let mut pair = pair("call-visibility");
        let room = pair.room.to_string();
        let key = PresenceKey::Workspace { id: room.clone() };
        let call = ui::CallPresence {
            channel: "c_voice".into(),
            name: "voice".into(),
        }
        .encode();
        let available = PresencePayload::Availability {
            state: ui::Availability::Available,
            call: Some(call.clone()),
        };
        let said = |pair: &mut Pair| match published(&mut pair.guest_cmds) {
            NeighbourMsg::AppPresenceV2 { presence, .. } => match presence.payload {
                collab_types::PeerPresencePayload::Availability { call, .. } => call,
                other => panic!("expected availability, got {other:?}"),
            },
            other => panic!("expected room presence, got {other:?}"),
        };
        for (visibility, heard) in [
            (ui::PresenceVisibility::ThatImHere, None),
            (ui::PresenceVisibility::WhereIAm, Some(call)),
        ] {
            pair.guest_net.set_presence_visibility(&room, visibility);
            let frame = crate::shell::collab_presence::to_peer(&key, &available)
                .expect("availability encodes");
            pair.guest_net
                .publish_app_presence("tilo-shell".into(), room.clone(), frame);
            assert_eq!(said(&mut pair), heard, "{visibility:?}");
        }
        let _ = std::fs::remove_dir_all(&pair.dir);
    }

    /// The moments after start, before the desktop has handed over what the
    /// person chose in a room: that room is held to the most private choice,
    /// so nothing of where they are leaves and nobody's pointer shows, never
    /// to a default looser than what they may have chosen.
    #[test]
    fn a_room_not_told_about_yet_is_held_to_nothing() {
        let pair = pair("unknown-visibility");
        let fresh = RoomId::new().to_string();
        assert_eq!(
            pair.guest_net.presence_visibility(&fresh),
            ui::PresenceVisibility::Nothing,
            "a room the desktop said nothing about yet shows nothing"
        );
        pair.guest.set_pointer(Some(on_desk(300.0, 200.0)));
        assert!(
            !pair.guest.publish_local_cursor_at(&fresh, Instant::now()),
            "no pointer is said there"
        );
        assert!(
            !pair.guest_net.presence_visibility(&fresh).sees_pointers(),
            "and nobody's pointer shows there"
        );
        pair.guest_net
            .set_presence_visibility(&fresh, ui::PresenceVisibility::WhereIAm);
        assert_eq!(
            pair.guest_net.presence_visibility(&fresh),
            ui::PresenceVisibility::WhereIAm,
            "once told, the person's choice holds"
        );
        let _ = std::fs::remove_dir_all(&pair.dir);
    }

    /// What people see, between two sessions: "That I'm here" sends no
    /// pointer, caret or cursor chat (the guest stays in the roster through
    /// its arrival); "Nothing" sends nothing, and its holder sees nobody's
    /// pointer either.
    #[test]
    fn what_people_see_holds_back_where_you_are_and_nothing_hides_pointers_both_ways() {
        let mut pair = pair("visibility");
        let room = pair.room.to_string();
        let t0 = Instant::now();

        pair.guest_net
            .set_presence_visibility(&room, ui::PresenceVisibility::ThatImHere);
        pair.guest.set_pointer(Some(on_desk(300.0, 200.0)));
        assert!(
            !pair.guest.publish_local_cursor_at(&room, t0),
            "\"That I'm here\": the pointer is not said"
        );
        assert!(
            pair.guest_cmds.try_recv().is_err(),
            "nothing reached the wire"
        );
        let caret = collab_types::PeerPresence {
            key: vec![1],
            payload: collab_types::PeerPresencePayload::Caret { pos: 3 },
        };
        pair.guest_net
            .publish_app_presence("so.tilo.notes".into(), room.clone(), caret);
        assert!(pair.guest_cmds.try_recv().is_err(), "nor a caret");
        let arrived = collab_types::PeerPresence {
            key: vec![1],
            payload: collab_types::PeerPresencePayload::Arrived,
        };
        pair.guest_net
            .publish_app_presence("so.tilo.notes".into(), room.clone(), arrived);
        assert!(
            pair.guest_cmds.try_recv().is_ok(),
            "but being here is: the face shows"
        );
        assert!(
            !pair.guest_net.publish_cursor_chat(
                (&room, "so.tilo.notes", "note-1"),
                (10.0, 10.0),
                Some(tilo_relay_protocol::live::CursorChat {
                    text: "hi".into(),
                    sent: true
                })
            ),
            "and cursor chat says where you are, so it is not sent either"
        );

        // "Where I am" again: the pointer goes out and reaches the host.
        pair.guest_net
            .set_presence_visibility(&room, ui::PresenceVisibility::WhereIAm);
        assert!(pair.guest.publish_local_cursor_at(&room, t0));
        over_the_wire(published(&mut pair.guest_cmds), &pair.host_net);
        let mut on_host = PresenceCtx::default();
        pair.host.pump_at(&mut on_host, &DESK_TILES, t0);
        let key = PresenceKey::Workspace { id: room.clone() };
        assert!(on_host.pointer_for(&key, &pair.guest_user()).is_some());

        // The host chooses "Nothing": the guest's pointer leaves its screen
        // at once, and arriving ones are not shown.
        pair.host_net
            .set_presence_visibility(&room, ui::PresenceVisibility::Nothing);
        assert!(pair.host.pump_at(&mut on_host, &DESK_TILES, t0));
        assert!(
            on_host.pointer_for(&key, &pair.guest_user()).is_none(),
            "someone who shows nothing sees no pointers"
        );
        pair.guest.set_pointer(Some(on_desk(500.0, 400.0)));
        let later = t0 + MIN_PUBLISH_GAP;
        assert!(pair.guest.publish_local_cursor_at(&room, later));
        over_the_wire(published(&mut pair.guest_cmds), &pair.host_net);
        pair.host.pump_at(&mut on_host, &DESK_TILES, later);
        assert!(on_host.pointer_for(&key, &pair.guest_user()).is_none());
        let _ = std::fs::remove_dir_all(&pair.dir);
    }
}

#[allow(
    unused_imports,
    reason = "The native WorkspaceExt methods are selected by host-specific workspace code and test configurations."
)]
use crate::shell::state::WorkspaceExt as _;
use tilo_lock::MutexRecover as _;
