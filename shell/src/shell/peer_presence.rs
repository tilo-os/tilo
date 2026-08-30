//! Peer pointers, from an app in a room to the compositor drawing it.
//!
//! Outbound: a [`RoomSubscription`] republishes each local app's
//! surface-local pointer as this session's cursor in desktop
//! coordinates — per session, not per connection, so one person stays
//! one cursor across windows. [`should_publish_cursor`] throttles to
//! [`MIN_PUBLISH_GAP`] and restates a still pointer every
//! [`CURSOR_KEEPALIVE`] for late joiners.
//!
//! Inbound: [`PeerPresence::pump`] folds arrivals through the same
//! decode an app uses. A peer's identity (`peer@<handle>`) comes from
//! the frame; its name is a governance fact from
//! [`ShellNetwork::peer_labels`] — a frame's own `user` field is never
//! trusted, since a peer naming itself could name anybody.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ui::collaboration::{CollabEvent, PresencePayload};
use ui::presence::{PresenceCtx, PresenceKey};

use crate::shell::collab_broker::{BrokerHandle, RoomSubscription};
use crate::shell::network::ShellNetwork;

/// Bridges the session's collaboration broker and its peers for
/// pointer presence. Held by the compositor for the life of the session.
pub struct PeerPresence {
    /// This session's pointer in scene space. `None` until it has
    /// actually been somewhere — not `(0, 0)`, since a session that
    /// never moves would otherwise announce the origin as a real position.
    cursor: Arc<Mutex<Option<(f64, f64)>>>,
    /// Inbound presence from other hosts, filled by [`ShellNetwork::tick`].
    inbound: Arc<Mutex<Vec<crate::shell::network::InboundPresence>>>,
    subscription: Arc<RoomSubscription>,
    /// The rooms this session is in, mirrored from [`Self::set_rooms`]
    /// so a room this session has left stops being remembered below.
    rooms: Mutex<Vec<String>>,
    /// What this session last put on the wire, per room: position and
    /// when. Read by [`should_publish_cursor`].
    last_published: Mutex<HashMap<String, ((f64, f64), Instant)>>,
    net: Arc<ShellNetwork>,
}

/// Smallest gap between two pointer publishes for the same room — ~30
/// Hz, matching the rate the toolkit throttles an app's pointer to so a
/// desktop cursor and an in-app cursor cost the same on the wire.
pub const MIN_PUBLISH_GAP: Duration = Duration::from_millis(33);

/// How long a stationary pointer goes unmentioned before the session
/// restates it. Presence is gossip: a host that joins after somebody
/// stopped moving has never heard where they are.
pub const CURSOR_KEEPALIVE: Duration = Duration::from_secs(1);

/// Should this session put its pointer on the wire again? `last` is
/// what it last published for this room and when. Pure so the decision
/// can be asserted on directly.
pub fn should_publish_cursor(
    last: Option<((f64, f64), Instant)>,
    at: (f64, f64),
    now: Instant,
    min_gap: Duration,
    keepalive: Duration,
) -> bool {
    let Some((was, when)) = last else {
        return true; // Never said anything about this room — say it.
    };
    let elapsed = now.saturating_duration_since(when);
    if was == at {
        return elapsed >= keepalive;
    }
    elapsed >= min_gap
}

/// What to put on the wire for a room right now, if anything. `at` is
/// `None` until the pointer has moved; `None` here means silence, not
/// `Some((0.0, 0.0))`.
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
                let key = PresenceKey::Workspace {
                    id: route.cluster_id.clone(),
                };
                let Ok(key_str) = serde_json::to_string(&key) else {
                    return;
                };
                // Nothing to say if the pointer has never moved.
                let Some(point) = *sink_cursor
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
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

    /// Re-point the subscription at the session's current rooms. Called
    /// whenever the workspace set changes.
    pub fn set_rooms(&self, rooms: Vec<String>) {
        if let Ok(mut g) = self.rooms.lock() {
            // Forget publishes to a left room, so rejoining re-announces
            // rather than staying silent behind a stale keepalive.
            if let Ok(mut last) = self.last_published.lock() {
                last.retain(|room, _| rooms.iter().any(|r| r == room));
            }
            *g = rooms.clone();
        }
        self.subscription.set_rooms(rooms);
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
        if room.is_empty() {
            return false;
        }
        let at = *self
            .cursor
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut last = self
            .last_published
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(at) = cursor_to_publish(
            at,
            last.get(room).copied(),
            now,
            MIN_PUBLISH_GAP,
            CURSOR_KEEPALIVE,
        ) else {
            return false;
        };
        let key = PresenceKey::Workspace {
            id: room.to_string(),
        };
        let Ok(key_str) = serde_json::to_string(&key) else {
            return false;
        };
        self.net.publish_presence(key_str, at);
        last.insert(room.to_string(), (at, now));
        true
    }

    /// Record where this session's pointer is. The `Some` half of
    /// [`Self::set_cursor_position`]; the frame loop uses the other half.
    pub fn set_cursor(&self, x: f64, y: f64) {
        self.set_cursor_position(Some((x, y)));
    }

    /// Record where this session's pointer is, or that it has never been
    /// anywhere — an `Option` because a never-moved pointer must stay
    /// silent rather than publish the origin as a real position.
    pub fn set_cursor_position(&self, at: Option<(f64, f64)>) {
        if let Ok(mut g) = self.cursor.lock() {
            *g = at;
        }
    }

    /// Fold everything that arrived from other hosts into `presence`,
    /// returning whether anything landed. Who is *in* the room is
    /// answered separately by `refresh_roster`.
    pub fn pump(&self, presence: &mut PresenceCtx) -> bool {
        let drained: Vec<_> = {
            let Ok(mut buf) = self.inbound.lock() else {
                return false;
            };
            std::mem::take(&mut *buf)
        };
        let net = Arc::clone(&self.net);
        let mine: Vec<String> = self.rooms.lock().map(|g| g.clone()).unwrap_or_default();
        fold_inbound(drained, presence, &|room| net.peer_labels(room), &|room| {
            mine.iter().any(|r| r == room)
        })
    }

    /// Write the active workspace's room's people into the tree, where
    /// the roster and top bar read them. Separate from [`Self::pump`]:
    /// answers "did who-is-here change", not "did a peer move" — a peer
    /// named while sitting still changes the former, not the latter.
    /// Returns whether the list changed, a reason to repaint.
    pub fn refresh_roster(&self, presence: &PresenceCtx) -> bool {
        let local = self.net.local_peer_id_hash();
        let Ok(replica) = self.net.replica.lock() else {
            return false;
        };
        // Written only on change — a lock every frame would contend
        // with the network thread.
        let rows = {
            let tree = replica.tree.read();
            let Some(ws) = tree.workspaces.iter().find(|w| w.id == tree.active) else {
                return false;
            };
            if ws.room.is_nil() {
                return false;
            }
            let labels = tree.room_peer_labels(ws.room);
            // Two ways to be present: moving cursor, or CRDT participation.
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

/// Fold inbound presence into `presence`, returning whether anything
/// landed. Split from [`PeerPresence::pump`] so it can be tested without
/// a network; `labels` is a parameter for the same reason (production
/// wires [`ShellNetwork::peer_labels`]).
pub fn fold_inbound(
    entries: Vec<crate::shell::network::InboundPresence>,
    presence: &mut PresenceCtx,
    labels: &dyn Fn(&str) -> PeerLabels,
    member_of: &dyn Fn(&str) -> bool,
) -> bool {
    let mut any = false;
    // Entries in one drain are nearly always the same room.
    let mut resolved: Option<(String, PeerLabels)> = None;
    for entry in entries {
        let Ok(key) = serde_json::from_str::<PresenceKey>(&entry.key) else {
            continue;
        };
        // Only rooms this session is in — an access rule.
        if let PresenceKey::Workspace { id: room } = &key
            && !member_of(room)
        {
            continue;
        }
        // Spelled from the handle, never `entry.user`'s unverified claim.
        let user = format!("peer@{}", entry.peer);

        if let PresenceKey::Workspace { id: room } = &key {
            let known = match &resolved {
                Some((seen, map)) if seen == room => map,
                _ => {
                    resolved = Some((room.clone(), labels(room)));
                    &resolved.as_ref().expect("just set").1
                }
            };
            if let Some(name) = known.get(&entry.peer) {
                presence.set_display_name(ui::permissions::UserId(user.clone()), name.clone());
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
        any = true; // Not on loop entry: a `continue` above is a rejected frame.
    }
    any
}

/// The people in `ws`'s room. Two sources, neither sufficient alone:
/// `labels` is the room document's answer (everyone its authority has
/// named, including an invitee who never moved their mouse), `arrived`
/// is who actually turned up (presence plus CRDT participants).
///
/// A named peer that hasn't arrived is marked [`Collaborator::invited`],
/// keyed off the transport-authenticated identity so a peer cannot
/// promote its own row. `local` is dropped — the roster is who *else*
/// is here.
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
            // Same spelling as the presence path, so avatar and cursor share an id.
            let id = format!("peer@{handle}");
            let name = labels.get(&handle).cloned().unwrap_or_else(|| id.clone());
            let role = ws.role_of(&name);
            let who = crate::shell::state::Collaborator::new(id, name);
            let who = match role {
                Some(role) => who.with_role(crate::shell::state::role_label(role)),
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
    use ui::permissions::UserId;

    #[test]
    fn a_session_that_has_said_nothing_speaks() {
        assert!(should_publish_cursor(
            None,
            (10.0, 10.0),
            Instant::now(),
            MIN_PUBLISH_GAP,
            CURSOR_KEEPALIVE,
        ));
    }

    #[test]
    fn a_pointer_that_has_never_moved_is_not_announced_at_the_origin() {
        let now = Instant::now();

        assert_eq!(
            cursor_to_publish(None, None, now, MIN_PUBLISH_GAP, CURSOR_KEEPALIVE),
            None,
            "a session whose pointer has never moved must not announce one"
        );

        // The origin is not banned once actually moved to.
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

    /// A frame as it arrives off the wire. `user` is the sender's own
    /// (untrusted) claimed name — tests below pass a wrong one on purpose.
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
    fn a_peer_lands_on_the_room_the_overlay_reads() {
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

    /// A folded frame lands in two stores — `pointers` (overlay paint)
    /// and `remote_cursors` (follow-that-peer) — and both being
    /// *painted* would draw one person twice.
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

    #[test]
    fn one_person_is_one_entry_across_every_room_in_the_store() {
        let mine = "3f2b1c00-0000-4000-8000-000000000001";
        let theirs = "3f2b1c00-0000-4000-8000-0000000000ff";
        let mut presence = PresenceCtx::default();

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

    #[test]
    fn naming_a_peer_does_not_conjure_a_cursor() {
        let _room = "3f2b1c00-0000-4000-8000-000000000001";
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
        assert_eq!(presence.display_name(&peers[0].0), "peer@42", "never blank");
    }
}
