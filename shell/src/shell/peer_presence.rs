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
//! **Inbound.** [`PeerPresence::pump`] drains what arrived from other
//! hosts and folds it in through
//! [`ui::presence::PresenceCtx::apply_remote_presence`] — the same
//! decode an app runs — so a peer painted by the compositor and a peer
//! painted inside an app are the same peer, arrived by the same route.

use std::sync::{Arc, Mutex};

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
}

impl PeerPresence {
    /// Subscribe to `rooms` on `broker` and republish what the local
    /// apps publish in them to `net`.
    pub fn attach(broker: &BrokerHandle, net: Arc<ShellNetwork>, rooms: Vec<String>) -> Self {
        let cursor = Arc::new(Mutex::new((0.0_f64, 0.0_f64)));
        let inbound = net.inbound_presence_handle();

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
        }
    }

    /// Re-point the subscription at the session's current rooms.
    /// Called whenever the workspace set changes — a new workspace,
    /// or one that just joined a peer's room by invite.
    pub fn set_rooms(&self, rooms: Vec<String>) {
        self.subscription.set_rooms(rooms);
    }

    /// Record where this session's pointer is, in scene space.
    pub fn set_cursor(&self, x: f64, y: f64) {
        if let Ok(mut g) = self.cursor.lock() {
            *g = (x, y);
        }
    }

    /// Fold everything that arrived from other hosts into `presence`.
    /// Returns who was seen and where, so the frame loop can treat a
    /// peer moving as a reason to repaint *and* put the people it just
    /// heard from on the room's roster.
    pub fn pump(&self, presence: &mut PresenceCtx) -> Vec<PeerSighting> {
        let drained: Vec<_> = {
            let Ok(mut buf) = self.inbound.lock() else {
                return Vec::new();
            };
            std::mem::take(&mut *buf)
        };
        fold_inbound(drained, presence)
    }
}

/// One peer, heard from in one room.
///
/// A presence frame is the only unforced evidence the compositor gets
/// that a particular person is in a particular room right now: it came
/// off the wire from another host, addressed to a room this session is
/// in, carrying the id that host says is sitting at it. The roster is
/// built out of exactly these, which is why the chrome can only ever
/// show someone who really arrived.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PeerSighting {
    /// The room the frame was addressed to — a `RoomId` rendered as a
    /// uuid string, which is what apps publish under and what the
    /// workspace carries.
    pub room: String,
    /// The publishing session's libp2p handle. Stable across the
    /// session's lifetime and identical on every host that hears it.
    pub peer: u64,
    /// Who that session says is at it, or empty when it said nothing.
    pub user: String,
}

/// Fold inbound presence into `presence`, returning every peer that
/// landed. Split out from [`PeerPresence::pump`] so the fold can be
/// driven without standing a network up; `pump` does nothing else.
///
/// The returned sightings are the roster's evidence. Each entry already
/// carried who the publishing session says is at it, and that was
/// previously used to label a cursor and then dropped — so the
/// compositor knew a named person was in the room at the moment it drew
/// their pointer, and still had nobody to put in the avatar stack.
pub fn fold_inbound(
    entries: Vec<crate::shell::network::InboundPresence>,
    presence: &mut PresenceCtx,
) -> Vec<PeerSighting> {
    let mut seen: Vec<PeerSighting> = Vec::new();
    for entry in entries {
        let Ok(key) = serde_json::from_str::<PresenceKey>(&entry.key) else {
            continue;
        };
        // Label the cursor with who the publishing session says is at
        // it. When it says nothing, fall back to the session handle
        // rather than inventing a name — an unlabelled peer is honest,
        // a wrong label is not.
        let user = if entry.user.is_empty() {
            crate::shell::state::session_handle(entry.peer)
        } else {
            entry.user.clone()
        };
        // Only a workspace-scoped frame names a room. A peer pointing
        // inside a document publishes under that document's key, which
        // says where in the text they are and nothing about which room
        // they are in.
        if let PresenceKey::Workspace { id } = &key {
            let sighting = PeerSighting {
                room: id.clone(),
                peer: entry.peer,
                user: user.clone(),
            };
            if !seen.contains(&sighting) {
                seen.push(sighting);
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
    }
    seen
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::network::InboundPresence;
    use ui::permissions::UserId;

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

    #[test]
    fn a_peer_lands_on_the_room_the_overlay_reads() {
        // The overlay looks pointers up by room, because the room is
        // what an app publishes under. A frame that landed anywhere
        // else would be held and never drawn.
        let room = "3f2b1c00-0000-4000-8000-000000000001";
        let mut presence = PresenceCtx::default();
        assert!(
            !fold_inbound(
                vec![arrived("bob", 7, room, (120.0, 240.0))],
                &mut presence
            )
            .is_empty()
        );

        let key = PresenceKey::Workspace { id: room.into() };
        let peers = presence
            .pointers
            .get(&key)
            .expect("the pointer should be filed under the room");
        assert_eq!(peers.len(), 1);
        assert_eq!(peers[0].0, UserId("bob".into()));
        assert_eq!(peers[0].1, (120.0, 240.0));
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
        );
        let peers = presence
            .pointers
            .get(&PresenceKey::Workspace { id: here.into() })
            .expect("a pointer in this room");
        assert_eq!(peers.len(), 1);
        assert_eq!(peers[0].0, UserId("bob".into()));
    }

    /// The fold has to hand back the room and the person, not just the
    /// fact that something moved. The roster is built from these, and a
    /// `bool` was exactly enough to draw a cursor and not enough to say
    /// who was in the room — which is how the avatar stack stayed empty
    /// through a session with two real peers pointing at each other.
    #[test]
    fn a_folded_frame_reports_the_room_and_the_person_it_came_from() {
        let room = "3f2b1c00-0000-4000-8000-000000000001";
        let mut presence = PresenceCtx::default();
        let seen = fold_inbound(vec![arrived("bob", 7, room, (1.0, 2.0))], &mut presence);
        assert_eq!(
            seen,
            vec![PeerSighting {
                room: room.into(),
                peer: 7,
                user: "bob".into(),
            }],
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
        assert!(fold_inbound(vec![entry], &mut presence).is_empty());
    }

    #[test]
    fn a_session_that_names_nobody_is_labelled_with_its_handle() {
        // Falling back to the session handle keeps the caret labelled
        // without inventing a person who did not announce themselves.
        let room = "3f2b1c00-0000-4000-8000-000000000001";
        let mut presence = PresenceCtx::default();
        fold_inbound(vec![arrived("", 0x2a, room, (5.0, 5.0))], &mut presence);
        let peers = presence
            .pointers
            .get(&PresenceKey::Workspace { id: room.into() })
            .expect("a pointer");
        assert_eq!(peers[0].0, UserId("p000000000000002a".into()));
    }
}
